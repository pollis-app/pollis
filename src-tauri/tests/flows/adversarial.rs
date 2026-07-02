//! Adversarial MLS recovery suite.
//!
//! Governing theme (`docs/backend-core-invariants.md`): *make invalid states
//! unrepresentable*, and *group-membership encryption must be bulletproof — we
//! can't rely on timeliness or best-case; any member may be offline for
//! arbitrarily long and must pick up seamlessly*. Every scenario here tries to
//! CREATE an invalid / lossy state through the DS fault seam (see
//! `harness::DsFault` and `harness::drop_commit_row`) and then proves the group
//! either refuses the invalid state or *converges* out of it — happy-path replay
//! is explicitly NOT what these tests cover.
//!
//! Convergence is asserted the way the rest of the harness asserts it: through
//! the real command pipeline (`fetch_channel_messages` decrypts, `group_member_ids`
//! reads the roster, `ds_head_epoch` reads the commit-log head). A member that
//! forked onto a divergent tree, wedged at a stale epoch, or squatted a duplicate
//! leaf would fail the decrypt assertions — those are the load-bearing checks.

use crate::harness::{
    arm_ds_fault, drop_commit_row, ds_fault_armed, ds_head_epoch, wipe, writable_remote, DsFault,
    TestClient,
};
use serial_test::serial;

// ─── Scenario 6 — cross-channel epoch strand (regression) ────────────────────

/// **Regression test for the cross-channel variant of the sweep/realtime
/// message-loss bug.** All channels in a group share ONE MLS group
/// (`mls_group_id == group_id`), but message ingest is per-channel
/// (`get_channel_messages(channel_id)` pulls only that channel's envelopes). Before
/// the fix, opening one channel advanced the *shared* local MLS group past an epoch
/// at which a *sibling* channel held an un-ingested message — and with
/// `max_past_epochs = 0` that message's keys were then gone, exactly as in the
/// cold-launch sweep, but triggered through the normal fetch path with no sweep
/// involved. The group-level interleaved catch-up
/// (`catch_up_mls_group_interleaved`) closes this: opening ANY channel catches up
/// the WHOLE group, decrypting every sibling channel's messages at each epoch
/// before advancing past it.
///
/// Sequence: alice + carol in a group with two text channels A and B.
/// 1. alice sends `mB0` on channel B at epoch E (carol is a member but has not
///    fetched B yet, so it sits un-ingested).
/// 2. alice adds bob — a membership commit advancing the shared MLS group E→E+1.
/// 3. carol opens channel A (`fetch` A) — this applies the pending commit,
///    advancing carol's shared local group to E+1 and discarding epoch-E keys,
///    WITHOUT ingesting channel B's `mB0`.
/// 4. carol opens channel B — its ingest now starts from E+1; `mB0` (sealed at E)
///    is behind the epoch wall and is dropped.
///
/// The assertion is the bulletproof invariant: carol, a continuous member since
/// `mB0` was sent, MUST decrypt it. This test is EXPECTED TO FAIL until the fix
/// (group-level interleaved catch-up) lands — it exists to confirm the
/// cross-channel variant is real, not just reasoned.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn cross_channel_sibling_message_is_not_stranded() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("CrossChan").await;
    let chan_a = alice.general_channel_id(&group_id).await;
    let chan_b = alice.create_channel(&group_id, "beta").await;

    // Carol is a full member of the group's (shared) MLS group.
    join_member(&alice, &carol, &group_id, &chan_a, &carol_p.username).await;
    carol.process_commits_for(&chan_a).await;

    // (1) alice sends on channel B at the current epoch E. Carol does NOT fetch B,
    // so mB0 stays un-ingested for her.
    alice.send_channel_message(&chan_b, "mB0").await;

    // (2) membership change: adding bob advances the SHARED MLS group E→E+1.
    join_member(&alice, &bob, &group_id, &chan_a, &bob_p.username).await;
    alice.process_commits_for(&chan_a).await;

    // (3) carol opens channel A first. This applies bob's commit (E→E+1) for the
    // shared group and discards epoch-E keys — without ingesting channel B.
    let _ = contents(&carol, &chan_a).await;

    // (4) carol opens channel B. mB0 was sealed at E, which carol was advanced
    // past in step 3.
    let carol_b = contents(&carol, &chan_b).await;

    // Bulletproof invariant: a continuous member must decrypt every message sent
    // while they were a member. With the group-level interleaved catch-up, opening
    // channel A caught carol up on the whole group — including channel B's mB0.
    assert!(
        carol_b.contains(&"mB0".to_string()),
        "CROSS-CHANNEL STRAND: carol was a continuous member when mB0 was sent on channel B, \
         but opening channel A first advanced the shared MLS group past mB0's epoch and lost it. \
         channel-B view={carol_b:?}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

// ─── Scenario 7 — removed (not revoked) member lockout on catch-up (repro) ───

/// **Deterministic repro for the fuzzer's second finding.** The model fuzzer, on
/// `[Add(1), Add(2), Remove(1), Add(3)]`, flagged: a member who was REMOVED from
/// the group (but whose device was NOT revoked) ended a convergence catch-up
/// holding a message sent AFTER their removal — a `MEMBERSHIP LEAK`. Static
/// reading says this can't happen: removal deletes the `group_member` row, the DS
/// gates commit submission by `is_member`, and self-eviction deletes the local
/// group; so a removed member's external-join recovery should be DS-rejected and
/// their view empty. This test mirrors the minimal fuzzer sequence deterministically
/// to settle whether the leak is REAL or a fuzzer-oracle artifact.
///
/// bob is added, carol is added, bob is REMOVED (device left registered — this is
/// NOT the revoked-device case), dave is added, then alice sends a post-removal
/// message. bob then comes online and runs the same catch-up a returning client
/// runs. The lockout invariant: removed bob must NOT decrypt the post-removal
/// message, and must not be a current member. carol (a continuous member) MUST
/// decrypt it — proving the message was genuinely delivered and it's specifically
/// removed bob who must be locked out.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn removed_member_locked_out_on_catchup() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let mut dave = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;
    let dave_p = dave.sign_up("dave@test.local").await;

    let group_id = alice.create_group("RemovedLockout").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    // [Add(1), Add(2), Remove(1), Add(3)] — bob in, carol in, bob out, dave in.
    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    alice.remove_member(&group_id, bob_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;
    join_member(&alice, &dave, &group_id, &channel_id, &dave_p.username).await;
    alice.process_commits_for(&channel_id).await;

    // Post-removal message (the fuzzer's final "probe"): sent while bob is out.
    alice.send_channel_message(&channel_id, "post-removal").await;
    carol.process_commits_for(&channel_id).await;
    dave.process_commits_for(&channel_id).await;

    // bob (removed, device still registered) comes online and catches up exactly
    // as a returning client does — the fuzzer's convergence step for an ever-member.
    bob.poll().await;
    bob.process_commits_for(&channel_id).await;
    let bob_view = contents(&bob, &channel_id).await;

    // Sanity: bob is not a current member.
    let members = alice.group_member_ids(&group_id).await;
    assert!(
        !members.contains(&bob_p.id),
        "bob should not be a current member after removal, got: {members:?}"
    );

    // Positive control: carol (continuous member) DID receive the post-removal
    // message — so it was genuinely delivered, isolating the question to bob.
    assert!(
        contents(&carol, &channel_id).await.contains(&"post-removal".to_string()),
        "carol (a continuous member) must decrypt the post-removal message"
    );

    // LOCKOUT INVARIANT: removed bob must NOT decrypt a message sent after his
    // removal. If this fails, the fuzzer's MEMBERSHIP LEAK is real — a removed
    // (not revoked) member climbed back in via the catch-up / external-join path.
    assert!(
        !bob_view.contains(&"post-removal".to_string()),
        "MEMBERSHIP LEAK CONFIRMED: removed bob decrypted a post-removal message via catch-up — \
         a removed (not revoked) member must stay locked out. bob view={bob_view:?}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
    drop(dave);
}

// ─── Scenario 8 — committer strands its own un-ingested inbound msg (repro) ──

/// **Deterministic repro for the fuzzer's committer-ingest finding (issue #440).**
/// The group-level catch-up fix (#438) covers the *fetch/sweep/realtime* paths, but
/// NOT the commit-INITIATION paths. When a member initiates a commit (invite /
/// remove) they merge their own commit and advance their epoch immediately; if they
/// were holding an un-ingested inbound message at the current epoch, `max_past_epochs
/// = 0` discards its keys and it is lost to them.
///
/// Mirrors the fuzzer's minimal sequence `[Add(2), Send(2), Add(1)]`: carol joins,
/// carol sends `m0` (alice is a member but has NOT fetched, so `m0` is un-ingested
/// for her), then alice adds bob — merging her own add commit and advancing past
/// `m0`'s epoch before ingesting it. The invariant: alice, a continuous member since
/// `m0` was sent, MUST decrypt it. EXPECTED TO FAIL until the committer-ingest fix
/// lands (ingest the current epoch before merging a self-initiated commit); it
/// exists to confirm the committer strand is real, not a fuzzer-oracle artifact
/// (the earlier removed-member finding turned out to be one).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn committer_does_not_strand_inbound_message() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("CommitterIngest").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    // carol joins; alice is caught up to carol's join epoch.
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;

    // carol sends m0 at the current epoch. alice does NOT fetch — m0 stays
    // un-ingested for her.
    carol.send_channel_message(&channel_id, "m0").await;

    // alice initiates a commit (adds bob) WITHOUT first ingesting m0: `invite`
    // merges her own add commit and advances her epoch past m0's epoch.
    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;

    // alice fetches. m0 was sealed at the pre-add epoch she advanced past.
    let alice_view = contents(&alice, &channel_id).await;

    // Positive control: carol (the sender) has m0 locally — it was genuinely sent.
    assert!(
        contents(&carol, &channel_id).await.contains(&"m0".to_string()),
        "carol (sender) must have m0"
    );

    // Bulletproof invariant: alice was a member when m0 was sent, so she MUST
    // decrypt it. Fails until the committer-ingest fix (#440) lands.
    assert!(
        alice_view.contains(&"m0".to_string()),
        "COMMITTER STRAND: alice lost m0 (sent by carol while alice was a member) because she \
         advanced her own epoch by committing an add before ingesting it. alice view={alice_view:?}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

// ─── shared helpers ──────────────────────────────────────────────────────────

/// Invite `member` to `group_id`, accept, drain the Welcome, and replay commits
/// so the member is a fully-joined participant of the channel's MLS group.
/// Mirrors the proven join sequence in `rejoin.rs` / `heavy_churn.rs`.
async fn join_member(
    inviter: &TestClient,
    member: &TestClient,
    group_id: &str,
    channel_id: &str,
    member_username: &str,
) {
    inviter.invite(group_id, member_username).await;
    let invite_id = member
        .first_pending_invite()
        .await
        .expect("member should have a pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    member.accept_invite(&invite_id).await;
    member.poll().await;
    inviter.process_commits_for(channel_id).await;
    member.process_commits_for(channel_id).await;
}

/// The decrypted plaintext bodies visible to `client` in `channel_id`.
async fn contents(client: &TestClient, channel_id: &str) -> Vec<String> {
    client
        .fetch_channel_messages(channel_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

// ─── Scenario 1 — generalized #411: adopt-your-own-canonical-commit ──────────

/// **Invalid state it attacks:** a committer that treats a lost DS success
/// response as "my commit failed", rolls back, and wedges — while the DS has in
/// fact durably committed the commit (the #411 shape, generalized to
/// `Fail500PostWrite`).
///
/// Here the commit under the fault is a **member removal**, not an add — so this
/// exercises `reconcile::our_commit_is_canonical` on a different commit type from
/// the ported add-based test in `messages.rs`. Alice removes carol; the DS
/// persists the remove commit + resulting GroupInfo, then returns 500. Alice must
/// observe her own commit is canonical at that epoch and ADOPT it (advance her
/// epoch, keep the roster change) rather than roll back.
///
/// **Convergence proved:** the fault fires exactly once; alice is not wedged (she
/// can still send and the remaining member bob decrypts it); the roster converged
/// (carol is gone, alice+bob remain); and evicted carol cannot read the
/// post-adopt traffic. A rollback-and-wedge would fail the "bob decrypts" check.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn fail500_post_write_commit_is_adopted_not_wedged() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("PostWrite").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    bob.process_commits_for(&channel_id).await;

    // Arm: the DS will PERSIST alice's next commit (the carol-removal) and its
    // GroupInfo, then answer 500. Alice believes the submit failed.
    arm_ds_fault(DsFault::Fail500PostWrite);
    alice.remove_member(&group_id, carol_p.id.as_str()).await;

    assert!(
        !ds_fault_armed(),
        "Fail500PostWrite should have fired exactly once on the removal commit"
    );

    // Both remaining members settle on the adopted commit.
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    // Roster converged: carol removed, alice + bob remain.
    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&alice.user_id().to_string()) && members.contains(&bob_p.id),
        "alice and bob must remain members after the adopted removal, got: {members:?}"
    );
    assert!(
        !members.contains(&carol_p.id),
        "carol must be removed after alice adopted her own canonical removal commit, got: {members:?}"
    );

    // LOAD-BEARING: alice did not wedge — she sends and the remaining member bob
    // decrypts. A rollback would have stranded alice at the pre-removal epoch and
    // this message would be undecryptable to bob.
    alice.send_channel_message(&channel_id, "post-adopt").await;
    bob.process_commits_for(&channel_id).await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"post-adopt".to_string()),
        "bob (a current member) must decrypt alice's post-adopt message — alice \
         wedged/rolled-back her own canonical commit otherwise"
    );

    // Evicted carol cannot read post-removal traffic.
    assert!(
        !contents(&carol, &channel_id).await.contains(&"post-adopt".to_string()),
        "REMOVAL BYPASS: evicted carol decrypted a post-removal message"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// Contrast case to the post-write fault: `Fail500PreWrite` is a CLEAN failure —
/// the DS returns 500 WITHOUT persisting anything. This pins the distinction the
/// fault menu draws. A post-write fault leaves a canonical commit the client must
/// ADOPT; a pre-write fault leaves NOTHING, so the client must roll its staged
/// commit back cleanly — never adopt a phantom epoch, never wedge.
///
/// **Convergence proved:** after a pre-write-faulted add attempt, (1) the
/// commit-log head is UNCHANGED — nothing landed — which is the direct evidence
/// that a pre-write fault is not a lost-response case; and (2) alice is not
/// wedged or forked onto a phantom epoch: she still round-trips a message with
/// the existing member bob at her real, unchanged epoch. A client that wrongly
/// "adopted" the never-persisted commit would advance past bob and this decrypt
/// would fail.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn fail500_pre_write_persists_nothing_and_does_not_wedge() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("PreWrite").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;

    // Baseline head after bob's add.
    let head_before = ds_head_epoch(&group_id).await;

    // Arm a pre-write failure, then attempt to add carol. The add commit's submit
    // 500s BEFORE persisting, so `reconcile` finds its commit is NOT canonical
    // and rolls back cleanly (surfacing an error). Use the non-panicking form —
    // a clean surfaced error is the CORRECT outcome here.
    arm_ds_fault(DsFault::Fail500PreWrite);
    let _ = alice
        .invoke_try(
            "send_group_invite",
            serde_json::json!({
                "groupId": group_id,
                "inviterId": alice.user_id(),
                "inviteeIdentifier": carol_p.username,
            }),
        )
        .await;
    assert!(
        !ds_fault_armed(),
        "Fail500PreWrite should have fired exactly once"
    );

    // (1) Nothing landed: the commit-log head did not move. A lost-RESPONSE
    // (post-write) fault would have advanced it; a pre-write fault must not.
    assert_eq!(
        ds_head_epoch(&group_id).await,
        head_before,
        "Fail500PreWrite persisted a commit — the head advanced when nothing should have landed"
    );

    // (2) No wedge / no phantom epoch: alice still talks to the existing member
    // bob at her real epoch.
    alice.send_channel_message(&channel_id, "post-prewrite").await;
    bob.process_commits_for(&channel_id).await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"post-prewrite".to_string()),
        "bob (a current member) must decrypt after a clean pre-write failure — alice \
         wedged or adopted a phantom epoch otherwise"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

// ─── Scenario 2 — epoch-gap recovery (#430-P2 / F1) ─────────────────────────

/// **Invalid state it attacks:** a non-contiguous commit log (invariant F1). A
/// member is offline while the group advances several epochs, and one *interior*
/// commit row is then dropped (`drop_commit_row`) — the exact "a row deleted
/// after another member applied it" shape the DB-trigger work (I1) is meant to
/// make impossible. The returning member must NOT wedge forever on the gap: it
/// drops its stale local group and recovers onto the current published epoch via
/// external-join.
///
/// **Convergence proved:** the returning member reaches the shared DS head, is a
/// current member, retains the in-membership message it had already ratcheted
/// past the gap for (M1, decrypted by the interleave hook before the gap is
/// hit), and decrypts fresh post-recovery traffic. The whole group agrees on the
/// head epoch.
///
/// **Accepted loss (documented, not fought):** messages sealed at the epochs the
/// gap forces the member to *jump over* via external-join are unrecoverable for
/// that member — that is the direct consequence of the injected F1 gap, and is
/// exactly what the I1 DB triggers exist to prevent upstream. This test proves
/// the CLIENT recovers and converges; it does not claim the gap itself is lossless.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn epoch_gap_recovers_via_external_join() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let mut dave = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;
    let dave_p = dave.sign_up("dave@test.local").await;

    let group_id = alice.create_group("Gap").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    // Bob joins at the add commit (commit epoch 0 → group head 1). Bob's MLS
    // epoch is now 1; he goes "offline" (no further poll/process/fetch until the
    // very end).
    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;

    // A message at bob's join epoch — the interleave hook decrypts this on his
    // return, BEFORE the replay reaches the gap, so it must survive.
    alice.send_channel_message(&channel_id, "M1-at-join-epoch").await;

    // Churn while bob is offline, each membership change advancing one epoch:
    //   commit epoch 1: carol add   (head 2)
    //   commit epoch 2: carol remove(head 3)   <-- this row will be dropped
    //   commit epoch 3: dave add    (head 4)
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    alice.remove_member(&group_id, carol_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;
    join_member(&alice, &dave, &group_id, &channel_id, &dave_p.username).await;

    // Sanity: the log advanced to head 4 before we punch the gap.
    assert_eq!(
        ds_head_epoch(&group_id).await,
        4,
        "expected head epoch 4 after add/remove/add churn"
    );

    // Punch the gap: delete the carol-remove commit (epoch 2). The log now reads
    // 0,1,[gap],3 — a returning member replaying from epoch 1 sees 1 then 3.
    drop_commit_row(&group_id, 2).await;
    assert_eq!(
        ds_head_epoch(&group_id).await,
        4,
        "dropping an interior row must not change the head (MAX(epoch)+1 = 4)"
    );

    // Bob comes back. This single fetch drains his backlog: the hook decrypts
    // M1 at epoch 1, the replay applies the epoch-1 commit, then hits the gap at
    // epoch 3, forgets the stale group, and external-joins onto the head.
    let bob_after_return = contents(&bob, &channel_id).await;
    assert!(
        bob_after_return.contains(&"M1-at-join-epoch".to_string()),
        "bob must retain the message at his join epoch (decrypted before the gap \
         is hit), got: {bob_after_return:?}"
    );

    // Alice applies bob's recovery external-join, then sends fresh traffic.
    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "after-recovery").await;

    // LOAD-BEARING: bob recovered — he is a current member and decrypts
    // post-recovery traffic instead of wedging on the gap forever.
    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&bob_p.id),
        "bob must be a current member after gap recovery, got: {members:?}"
    );
    assert!(
        contents(&bob, &channel_id).await.contains(&"after-recovery".to_string()),
        "bob must decrypt post-recovery traffic — he wedged on the epoch gap otherwise"
    );

    // Whole group converges on the head: dave (a continuous member) also reads it.
    dave.process_commits_for(&channel_id).await;
    assert!(
        contents(&dave, &channel_id).await.contains(&"after-recovery".to_string()),
        "dave must decrypt post-recovery traffic — the group forked at the gap otherwise"
    );

    drop(alice);
    drop(bob);
    drop(carol);
    drop(dave);
}

// ─── Scenario 3 — dropped Welcome recovers via external-join ────────────────

/// **Invalid state it attacks:** a newly-added member stranded because their only
/// Welcome was lost (invariant F5). The add commit + resulting GroupInfo land,
/// but `DropWelcome` prevents the Welcome from ever being persisted, so the new
/// member cannot join from a Welcome. Because the member's leaf was already added
/// by the (landed) commit, recovering via external-join creates a *second* leaf —
/// the duplicate-leaf-prune path the staying members must resolve.
///
/// **Convergence proved:** the new member ends up a functional current member —
/// they decrypt a message alice sends after they recover, alice decrypts a
/// message they send, and the roster lists them exactly once. A fork from an
/// unpruned duplicate leaf would fail the cross-direction decrypt checks.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dropped_welcome_recovers_via_external_join() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("DropWelcome").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    // Arm the Welcome drop, THEN add bob. The commit + GroupInfo land; bob's
    // Welcome never becomes fetchable.
    arm_ds_fault(DsFault::DropWelcome);
    alice.invite(&group_id, &bob_p.username).await;
    assert!(
        !ds_fault_armed(),
        "DropWelcome should have fired exactly once on bob's add"
    );

    // Bob accepts and polls — but there is no Welcome to drain. His commit-log
    // catch-up finds no local group and must external-join from the published
    // GroupInfo instead.
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should still see a pending invite (only the Welcome was dropped)")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    bob.process_commits_for(&channel_id).await;

    // Alice reconciles bob's external-join (and prunes his stale add-leaf).
    alice.process_commits_for(&channel_id).await;

    // Roster lists bob exactly once — no duplicate-leaf residue at the user level.
    let members = alice.group_member_ids(&group_id).await;
    assert_eq!(
        members.iter().filter(|m| **m == bob_p.id).count(),
        1,
        "bob must appear exactly once in the roster after dropped-Welcome recovery, got: {members:?}"
    );

    // LOAD-BEARING both directions: a fork from an unpruned duplicate leaf would
    // break one of these decrypts.
    alice.send_channel_message(&channel_id, "alice-to-bob").await;
    bob.process_commits_for(&channel_id).await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"alice-to-bob".to_string()),
        "bob must decrypt alice's message after recovering via external-join"
    );

    bob.send_channel_message(&channel_id, "bob-to-alice").await;
    alice.process_commits_for(&channel_id).await;
    assert!(
        contents(&alice, &channel_id).await.contains(&"bob-to-alice".to_string()),
        "alice must decrypt recovered bob's message — a duplicate leaf forked the tree otherwise"
    );

    drop(alice);
    drop(bob);
}

// ─── Scenario 4 — eviction then re-add, with a provable blackout window ──────

/// **Invalid state it attacks:** an evicted member who can still read traffic
/// sent while they were out (a membership/forward-secrecy leak) — or, conversely,
/// a re-added member who is wedged out of the conversation they legitimately
/// rejoined.
///
/// Bob joins, reads a pre-removal message, is removed, and while he is out alice
/// sends two messages; then bob is re-added. Bob must: (1) still hold the
/// pre-removal message he decrypted while a member, (2) decrypt post-re-add
/// traffic, and (3) **provably NOT** decrypt the two messages sent while he was
/// evicted (sealed at epochs he was not a member of — bounded-history caveat (a)
/// plus MLS forward secrecy). The roster must list him exactly once.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn eviction_then_readd_has_provable_blackout() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Evict").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;

    // Bob reads a message while he is a member — it lands in his local message
    // store and must survive the removal (removal forgets MLS crypto state, not
    // decrypted history).
    alice.send_channel_message(&channel_id, "pre-removal").await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"pre-removal".to_string()),
        "bob must decrypt the pre-removal message while still a member"
    );

    // Evict bob and settle his leaf out of the tree.
    alice.remove_member(&group_id, bob_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;

    // Blackout window: messages bob must never be able to read.
    alice.send_channel_message(&channel_id, "evicted-1").await;
    alice.send_channel_message(&channel_id, "evicted-2").await;

    // Re-add bob.
    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    alice.process_commits_for(&channel_id).await;

    // Roster lists bob exactly once (a stale + fresh leaf would double-count at
    // the tree level; the user-level roster must still be clean).
    let members = alice.group_member_ids(&group_id).await;
    assert_eq!(
        members.iter().filter(|m| **m == bob_p.id).count(),
        1,
        "bob must appear exactly once after re-add, got: {members:?}"
    );

    // Post-re-add traffic must reach bob.
    alice.send_channel_message(&channel_id, "post-readd").await;
    bob.process_commits_for(&channel_id).await;
    let bob_contents = contents(&bob, &channel_id).await;
    assert!(
        bob_contents.contains(&"post-readd".to_string()),
        "re-added bob must decrypt post-re-add traffic, got: {bob_contents:?}"
    );
    assert!(
        bob_contents.contains(&"pre-removal".to_string()),
        "bob must retain the pre-removal message he decrypted while a member, got: {bob_contents:?}"
    );

    // PROVABLE BLACKOUT: the two evicted-window messages must be undecryptable to
    // bob even after re-add.
    for blacked_out in ["evicted-1", "evicted-2"] {
        assert!(
            !bob_contents.contains(&blacked_out.to_string()),
            "MEMBERSHIP LEAK: re-added bob decrypted {blacked_out:?}, sent while he was \
             evicted — he must never read epochs he was not a member of. got: {bob_contents:?}"
        );
    }

    drop(alice);
    drop(bob);
}

// ─── Scenario 5 — revoked-device lockout across every recovery path ─────────

/// **Invalid state it attacks:** a revoked device climbing back into a group it
/// was removed from (invariant: a device whose `user_device` row is tombstoned
/// must stay out). The device drives EVERY recovery entry point a client has —
/// `process_pending_commits` and `get_channel_messages` — and each must fail
/// CLEANLY: a no-op, never a panic, never a wedge of the rest of the group.
///
/// **Asserted loudly (never a silent no-op as a pass):** the `local_device_registered`
/// gate can silently skip external-join, so the load-bearing checks are the
/// *observable lockout* — the revoked device cannot decrypt any post-removal
/// message and is not in the roster — combined with the group staying live
/// (carol keeps receiving). If the gate wrongly let the device back in, the
/// "cannot decrypt" assertions fail; if it wrongly wedged the group, carol's
/// assertions fail.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn revoked_device_locked_out_of_every_recovery_path() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Revoke").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    bob.process_commits_for(&channel_id).await;

    // Baseline: bob is a real, decrypting member.
    alice.send_channel_message(&channel_id, "before-revoke").await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"before-revoke".to_string()),
        "bob should decrypt while still a registered member"
    );

    // Revoke bob's device server-side: tombstone its `user_device` row (the
    // #372 revoked-device state). Poke the writable MAIN handle directly — the
    // client's own view is read-only.
    {
        let remote = writable_remote().await;
        let conn = remote.conn().await.expect("remote conn");
        conn.execute(
            "UPDATE user_device SET revoked_at = datetime('now') WHERE user_id = ?1",
            libsql::params![bob_p.id.clone()],
        )
        .await
        .expect("tombstone bob user_device");
    }

    // Remove bob from the group; remaining members prune his leaf.
    alice.remove_member(&group_id, bob_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    alice.send_channel_message(&channel_id, "after-revoke-1").await;

    // Bob drives EVERY recovery entry point. Both must return cleanly (these
    // helpers panic on an `Err`, so reaching the assertions proves no panic/error)
    // and neither may climb the revoked device back in.
    bob.process_commits_for(&channel_id).await;
    let bob_contents = contents(&bob, &channel_id).await;

    // LOAD-BEARING lockout: the revoked device cannot read post-removal traffic
    // and is not in the roster.
    assert!(
        !bob_contents.contains(&"after-revoke-1".to_string()),
        "REVOCATION BYPASS: revoked bob decrypted a post-removal message — it climbed \
         back in via a recovery path. got: {bob_contents:?}"
    );
    let members = alice.group_member_ids(&group_id).await;
    assert!(
        !members.contains(&bob_p.id),
        "revoked bob must not reappear in the roster, got: {members:?}"
    );

    // No wedge: the group keeps advancing for the legitimate members.
    alice.send_channel_message(&channel_id, "after-revoke-2").await;
    carol.process_commits_for(&channel_id).await;
    assert!(
        contents(&carol, &channel_id).await.contains(&"after-revoke-2".to_string()),
        "group wedged after the revoked-device recovery attempts — carol could not \
         receive a new message"
    );

    // And bob is STILL out after a second recovery attempt on fresh traffic.
    let bob_contents = contents(&bob, &channel_id).await;
    assert!(
        !bob_contents.contains(&"after-revoke-2".to_string()),
        "REVOCATION BYPASS: revoked bob decrypted later traffic on a repeat recovery \
         attempt. got: {bob_contents:?}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}
