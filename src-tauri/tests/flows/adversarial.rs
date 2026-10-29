//! Adversarial MLS recovery suite.
//!
//! Each scenario creates an invalid state through the DS fault seam and proves the group refuses or converges out of it.

use crate::harness::{
    arm_ds_fault, corrupt_commit_row, drop_commit_row, ds_fault_armed, ds_head_epoch,
    ds_prune_commit_log, expect_losses, force_high_water_for_all_members, prune_commits_below,
    steal_leaf, wipe,
    writable_remote, DsFault, TestClient,
};
use serial_test::serial;

/// Channels share one MLS group, so opening channel A must not strand sibling channel B's message.
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

    join_member(&alice, &carol, &group_id, &chan_a, &carol_p.username).await;
    carol.process_commits_for(&chan_a).await;

    // Carol does not fetch B.
    alice.send_channel_message(&chan_b, "mB0").await;

    join_member(&alice, &bob, &group_id, &chan_a, &bob_p.username).await;
    alice.process_commits_for(&chan_a).await;

    // Applies bob's commit without ingesting channel B.
    let _ = contents(&carol, &chan_a).await;

    let carol_b = contents(&carol, &chan_b).await;

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

/// Fuzzer finding `[Add(1), Add(2), Remove(1), Add(3)]`: a removed (not revoked) member stays locked out.
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

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    alice.remove_member(&group_id, bob_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;
    join_member(&alice, &dave, &group_id, &channel_id, &dave_p.username).await;
    alice.process_commits_for(&channel_id).await;

    alice.send_channel_message(&channel_id, "post-removal").await;
    carol.process_commits_for(&channel_id).await;
    dave.process_commits_for(&channel_id).await;

    bob.poll().await;
    bob.process_commits_for(&channel_id).await;
    let bob_view = contents(&bob, &channel_id).await;

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        !members.contains(&bob_p.id),
        "bob should not be a current member after removal, got: {members:?}"
    );

    // Positive control: delivered, isolating the question to bob.
    assert!(
        contents(&carol, &channel_id).await.contains(&"post-removal".to_string()),
        "carol (a continuous member) must decrypt the post-removal message"
    );

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

/// `[Add(1), Remove(1), Add(2)]`: external-join recovery is gated on current membership, not just registration.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn removed_member_cannot_climb_back_via_external_join() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("RemovedClimbBack").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    alice.remove_member(&group_id, bob_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    alice.process_commits_for(&channel_id).await;

    // Without the membership gate this external-joins bob back in.
    bob.poll().await;
    bob.process_commits_for(&channel_id).await;

    // A message sent after the climb-back is what makes the leak observable.
    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "post-climb").await;

    bob.poll().await;
    bob.process_commits_for(&channel_id).await;
    let bob_view = contents(&bob, &channel_id).await;

    // Positive control: delivered, isolating the question to bob.
    carol.process_commits_for(&channel_id).await;
    assert!(
        contents(&carol, &channel_id).await.contains(&"post-climb".to_string()),
        "carol (a continuous member) must decrypt the post-climb message"
    );

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        !members.contains(&bob_p.id),
        "bob should not be a current member after removal, got: {members:?}"
    );

    assert!(
        !bob_view.contains(&"post-climb".to_string()),
        "MEMBERSHIP LEAK CONFIRMED: removed bob decrypted a post-climb message — he rebuilt \
         himself into the tree via external-join. A removed member must be gated out of the \
         recovery path on membership, not just device-revocation. bob view={bob_view:?}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// #440: a committer ingests the current epoch before advancing past it (`[Add(2), Send(2), Add(1)]`).
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

    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;

    // alice does not fetch.
    carol.send_channel_message(&channel_id, "m0").await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;

    let alice_view = contents(&alice, &channel_id).await;

    assert!(
        contents(&carol, &channel_id).await.contains(&"m0".to_string()),
        "carol (sender) must have m0"
    );

    assert!(
        alice_view.contains(&"m0".to_string()),
        "COMMITTER STRAND: alice lost m0 (sent by carol while alice was a member) because she \
         advanced her own epoch by committing an add before ingesting it. alice view={alice_view:?}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// #430 P1: the sweep's reconcile backstop prunes the leaf of a member whose remove commit was dropped.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sweep_backstops_dropped_remove_eviction() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("SweepEvict").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    bob.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    // Baseline: bob has live ratchet state.
    alice.send_channel_message(&channel_id, "pre-removal").await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"pre-removal".to_string()),
        "bob must decrypt the pre-removal message while still a member"
    );

    // A dropped remove: delete the roster row server-side without pruning the leaf.
    {
        let remote = writable_remote().await;
        let conn = remote.conn().await.expect("remote conn");
        let affected = conn
            .execute(
                "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
                libsql::params![group_id.clone(), bob_p.id.clone()],
            )
            .await
            .expect("delete bob's group_member row");
        assert_eq!(affected, 1, "expected exactly one group_member row for bob to delete");
    }

    alice.sweep().await;

    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "post-eviction").await;

    // Positive control: delivered, isolating the question to bob.
    carol.process_commits_for(&channel_id).await;
    assert!(
        contents(&carol, &channel_id).await.contains(&"post-eviction".to_string()),
        "carol (a continuous member) must decrypt the post-eviction message"
    );

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        !members.contains(&bob_p.id),
        "bob should not be a current member after removal, got: {members:?}"
    );

    bob.poll().await;
    bob.process_commits_for(&channel_id).await;
    let bob_view = contents(&bob, &channel_id).await;

    assert!(
        !bob_view.contains(&"post-eviction".to_string()),
        "EVICTION FORWARD-SECRECY GAP: removed bob decrypted a post-eviction message — the sweep \
         reconcile backstop failed to prune his stale leaf after a dropped remove commit. \
         bob view={bob_view:?}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// Invite, accept, drain the Welcome and replay commits.
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

async fn contents(client: &TestClient, channel_id: &str) -> Vec<String> {
    client
        .fetch_channel_messages(channel_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

/// A committer must adopt its own canonical removal commit after a `Fail500PostWrite` (#411).
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

    // The DS persists the removal, then answers 500.
    arm_ds_fault(DsFault::Fail500PostWrite);
    alice.remove_member(&group_id, carol_p.id.as_str()).await;

    assert!(
        !ds_fault_armed(),
        "Fail500PostWrite should have fired exactly once on the removal commit"
    );

    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&alice.user_id().to_string()) && members.contains(&bob_p.id),
        "alice and bob must remain members after the adopted removal, got: {members:?}"
    );
    assert!(
        !members.contains(&carol_p.id),
        "carol must be removed after alice adopted her own canonical removal commit, got: {members:?}"
    );

    // Load-bearing: a rollback would strand alice at the pre-removal epoch.
    alice.send_channel_message(&channel_id, "post-adopt").await;
    bob.process_commits_for(&channel_id).await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"post-adopt".to_string()),
        "bob (a current member) must decrypt alice's post-adopt message — alice \
         wedged/rolled-back her own canonical commit otherwise"
    );

    assert!(
        !contents(&carol, &channel_id).await.contains(&"post-adopt".to_string()),
        "REMOVAL BYPASS: evicted carol decrypted a post-removal message"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// `Fail500PreWrite` persists nothing: the head is unchanged and alice rolls back cleanly.
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

    let head_before = ds_head_epoch(&group_id).await;

    // A clean surfaced error is the correct outcome, hence the non-panicking form.
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

    assert_eq!(
        ds_head_epoch(&group_id).await,
        head_before,
        "Fail500PreWrite persisted a commit — the head advanced when nothing should have landed"
    );

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

/// F1 gap: a dropped interior commit forces external-join; M1 and mid-replay M2 must be ingested first (#440/#441).
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

    // Bob joins, then goes offline until the end.
    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    let bob_join_epoch = ds_head_epoch(&group_id).await;

    // Decrypted before the replay reaches the gap.
    alice.send_channel_message(&channel_id, "M1-at-join-epoch").await;

    // The row to drop is read off the log: a join's commit count isn't constant (#666).
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    let remove_epoch = ds_head_epoch(&group_id).await;

    // M2 must sit at a mid-replay epoch, or its assertion goes vacuous.
    assert!(
        remove_epoch > bob_join_epoch,
        "M2 must sit at an epoch bob reaches only by APPLYING a commit \
         (join epoch {bob_join_epoch}, M2 epoch {remove_epoch}) — otherwise this scenario \
         only re-tests the initial-epoch hook"
    );
    alice.send_channel_message(&channel_id, "M2-mid-replay").await;

    alice.remove_member(&group_id, carol_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;
    join_member(&alice, &dave, &group_id, &channel_id, &dave_p.username).await;

    // The gap must be interior to be classified as a gap.
    let head_before_gap = ds_head_epoch(&group_id).await;
    assert!(
        head_before_gap > remove_epoch + 1,
        "the dave add must sit above the carol-remove epoch {remove_epoch} for the gap to be \
         interior, head is {head_before_gap}"
    );

    drop_commit_row(&group_id, remove_epoch).await;
    assert_eq!(
        ds_head_epoch(&group_id).await,
        head_before_gap,
        "dropping an interior row must not change the head (MAX(epoch)+1)"
    );

    // Hook decrypts M1, replay hits the gap, then external-join onto the head.
    let bob_after_return = contents(&bob, &channel_id).await;
    assert!(
        bob_after_return.contains(&"M1-at-join-epoch".to_string()),
        "bob must retain the message at his join epoch (decrypted before the gap \
         is hit), got: {bob_after_return:?}"
    );
    // M2 is ingested before the rebuild discards its keys.
    assert!(
        bob_after_return.contains(&"M2-mid-replay".to_string()),
        "STRAND THROUGH REBUILD: bob lost M2, sent at a mid-replay epoch he held keys for. The \
         interleave hook must ingest each epoch's messages BEFORE the external-join rebuild jumps \
         the local group to head. got: {bob_after_return:?}"
    );

    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "after-recovery").await;

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&bob_p.id),
        "bob must be a current member after gap recovery, got: {members:?}"
    );
    assert!(
        contents(&bob, &channel_id).await.contains(&"after-recovery".to_string()),
        "bob must decrypt post-recovery traffic — he wedged on the epoch gap otherwise"
    );

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

/// #539: a member pruned past its epoch external-joins (losing only the gap); a removed one must not (I5).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn commit_log_prune_recovers_via_external_join() {
    wipe().await;
    // The pruned-gap message is lost on bob on purpose.
    expect_losses();

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let mut dave = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;
    let dave_p = dave.sign_up("dave@test.local").await;

    let group_id = alice.create_group("Prune").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    alice.send_channel_message(&channel_id, "M1-at-join-epoch").await;

    // The message after carol joins lands in bob's pruned gap; dave's add is the floor.
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    alice.send_channel_message(&channel_id, "M2-in-pruned-gap").await;
    alice.remove_member(&group_id, carol_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;
    let dave_add_epoch = ds_head_epoch(&group_id).await;
    join_member(&alice, &dave, &group_id, &channel_id, &dave_p.username).await;

    let head_before_prune = ds_head_epoch(&group_id).await;
    assert!(
        head_before_prune > dave_add_epoch,
        "dave's add must have advanced the log past the prune floor {dave_add_epoch}, \
         head is {head_before_prune}"
    );

    let pruned = prune_commits_below(&group_id, dave_add_epoch).await;
    assert_eq!(
        pruned, dave_add_epoch as u64,
        "every epoch below {dave_add_epoch} must be pruned"
    );
    assert_eq!(
        ds_head_epoch(&group_id).await,
        head_before_prune,
        "pruning below the head must not change MAX(epoch)+1"
    );

    let bob_after = contents(&bob, &channel_id).await;
    assert!(
        bob_after.contains(&"M1-at-join-epoch".to_string()),
        "bob keeps the message at his join epoch (decrypted before the gap), got: {bob_after:?}"
    );
    assert!(
        !bob_after.contains(&"M2-in-pruned-gap".to_string()),
        "bob must NOT recover the message sealed in the pruned gap (accepted loss #1), got: {bob_after:?}"
    );

    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "after-recovery").await;

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&bob_p.id),
        "bob must be a current member after prune recovery, got: {members:?}"
    );
    assert!(
        contents(&bob, &channel_id).await.contains(&"after-recovery".to_string()),
        "bob must decrypt post-recovery traffic — he wedged on the pruned gap otherwise"
    );

    dave.process_commits_for(&channel_id).await;
    assert!(
        contents(&dave, &channel_id).await.contains(&"after-recovery".to_string()),
        "dave must decrypt post-recovery traffic — the group forked at the prune otherwise"
    );

    // Carol (removed) must not climb back: may_rejoin checks membership.
    let _ = contents(&carol, &channel_id).await;
    alice.process_commits_for(&channel_id).await;
    let members = alice.group_member_ids(&group_id).await;
    assert!(
        !members.contains(&carol_p.id),
        "REMOVED carol must NOT rejoin the tree via prune-gap recovery (I5), got: {members:?}"
    );
    alice.send_channel_message(&channel_id, "post-removal-secret").await;
    assert!(
        !contents(&carol, &channel_id).await.contains(&"post-removal-secret".to_string()),
        "removed carol must not decrypt post-removal traffic after a prune-gap recovery attempt"
    );

    drop(alice);
    drop(bob);
    drop(carol);
    drop(dave);
}

/// #681: a forged high-water far above head must not let the retention prune wipe the live log.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn forged_retention_high_water_cannot_wipe_the_live_log() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut dave = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let dave_p = dave.sign_up("dave@test.local").await;

    let group_id = alice.create_group("Retention").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    alice.send_channel_message(&channel_id, "M1").await;
    bob.process_commits_for(&channel_id).await;
    assert!(contents(&bob, &channel_id).await.contains(&"M1".to_string()));

    let head_before = ds_head_epoch(&group_id).await;
    assert!(
        head_before > 1,
        "the group must have a real commit history, head is {head_before}"
    );

    // A forged high-water for the whole roster.
    force_high_water_for_all_members(&group_id, head_before + 100_000).await;

    let report = ds_prune_commit_log(&group_id).await;

    assert!(
        report.floor < head_before,
        "the floor {} must stay below head {head_before} — a forged report must not wipe the log",
        report.floor
    );
    assert_eq!(
        ds_head_epoch(&group_id).await,
        head_before,
        "a forged high-water must not reset the conversation head (the log-wipe)"
    );

    // The group must still advance through the real client path.
    join_member(&alice, &dave, &group_id, &channel_id, &dave_p.username).await;
    alice.send_channel_message(&channel_id, "after-clamp").await;
    bob.process_commits_for(&channel_id).await;
    dave.process_commits_for(&channel_id).await;

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&dave_p.id),
        "dave's add must land after the prune, got: {members:?}"
    );
    assert!(
        contents(&bob, &channel_id).await.contains(&"after-clamp".to_string()),
        "bob must still advance after a forged high-water — he wedged otherwise"
    );
    assert!(
        contents(&dave, &channel_id).await.contains(&"after-clamp".to_string()),
        "dave must converge on the post-prune head"
    );

    drop(alice);
    drop(bob);
    drop(dave);
}

/// F5: a dropped Welcome recovers via external-join, with the duplicate leaf pruned.
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

    arm_ds_fault(DsFault::DropWelcome);
    alice.invite(&group_id, &bob_p.username).await;
    assert!(
        !ds_fault_armed(),
        "DropWelcome should have fired exactly once on bob's add"
    );

    // No Welcome to drain, so the catch-up must external-join.
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

    alice.process_commits_for(&channel_id).await;

    let members = alice.group_member_ids(&group_id).await;
    assert_eq!(
        members.iter().filter(|m| **m == bob_p.id).count(),
        1,
        "bob must appear exactly once in the roster after dropped-Welcome recovery, got: {members:?}"
    );

    // Load-bearing both ways: an unpruned duplicate leaf would break one.
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

/// Evict then re-add: bob keeps pre-removal history, reads post-re-add, and never the blackout window.
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

    // Removal forgets MLS state, not decrypted history.
    alice.send_channel_message(&channel_id, "pre-removal").await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"pre-removal".to_string()),
        "bob must decrypt the pre-removal message while still a member"
    );

    alice.remove_member(&group_id, bob_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;

    alice.send_channel_message(&channel_id, "evicted-1").await;
    alice.send_channel_message(&channel_id, "evicted-2").await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    alice.process_commits_for(&channel_id).await;

    let members = alice.group_member_ids(&group_id).await;
    assert_eq!(
        members.iter().filter(|m| **m == bob_p.id).count(),
        1,
        "bob must appear exactly once after re-add, got: {members:?}"
    );

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

/// Re-rostered after removal, with the re-add Welcome dropped: the external-join gate must allow him back.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn removed_then_rerostered_member_recovers_via_external_join() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Reroster").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    bob.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    alice.send_channel_message(&channel_id, "pre-removal").await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"pre-removal".to_string()),
        "bob must decrypt the pre-removal message while still a member"
    );

    alice.remove_member(&group_id, bob_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    // Blackout probe.
    alice.send_channel_message(&channel_id, "blackout").await;
    carol.process_commits_for(&channel_id).await;

    // Removed: he self-evicts and the external-join is gated out.
    bob.poll().await;
    bob.process_commits_for(&channel_id).await;
    let members_while_out = alice.group_member_ids(&group_id).await;
    assert!(
        !members_while_out.contains(&bob_p.id),
        "bob must be out of the roster while removed, got: {members_while_out:?}"
    );

    // Re-roster with the Welcome dropped, so only the external-join gate can let him in.
    arm_ds_fault(DsFault::DropWelcome);
    alice.invite(&group_id, &bob_p.username).await;
    assert!(
        !ds_fault_armed(),
        "DropWelcome should have fired exactly once on bob's re-add"
    );

    // Membership restored, so may_rejoin now allows it.
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should see a pending re-invite (only the Welcome was dropped)")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    bob.process_commits_for(&channel_id).await;

    alice.process_commits_for(&channel_id).await;

    let members = alice.group_member_ids(&group_id).await;
    assert_eq!(
        members.iter().filter(|m| **m == bob_p.id).count(),
        1,
        "re-rostered bob must appear exactly once in the roster, got: {members:?}"
    );

    alice.send_channel_message(&channel_id, "post-readd").await;
    bob.process_commits_for(&channel_id).await;
    let bob_contents = contents(&bob, &channel_id).await;

    // Positive control: delivered, isolating the invariant to bob.
    carol.process_commits_for(&channel_id).await;
    assert!(
        contents(&carol, &channel_id).await.contains(&"post-readd".to_string()),
        "carol (a continuous member) must decrypt the post-readd message"
    );

    // Load-bearing: the fail-closed gate must not over-reach into the happy path.
    assert!(
        bob_contents.contains(&"post-readd".to_string()),
        "RE-ROSTER LOCKOUT: bob was re-added to the group but never recovered through the \
         external-join gate — a removal must be revocable, not a permanent lockout. \
         bob view={bob_contents:?}"
    );

    assert!(
        bob_contents.contains(&"pre-removal".to_string()),
        "bob must retain the pre-removal message he decrypted while a member, got: {bob_contents:?}"
    );

    assert!(
        !bob_contents.contains(&"blackout".to_string()),
        "MEMBERSHIP LEAK: re-rostered bob decrypted the blackout message sent while he was \
         removed — re-add must land a fresh leaf, not restore the evicted epoch. \
         bob view={bob_contents:?}"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// A revoked and removed device stays out across every recovery entry point, without wedging the group.
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
    carol.process_commits_for(&channel_id).await;

    alice.send_channel_message(&channel_id, "before-revoke").await;
    for (c, label) in [(&alice, "alice"), (&bob, "bob"), (&carol, "carol")] {
        assert!(
            contents(c, &channel_id).await.contains(&"before-revoke".to_string()),
            "{label} should decrypt 'before-revoke' while bob is still a registered member"
        );
    }

    // Tombstone bob's `user_device` row (#372) via the writable main handle.
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

    alice.remove_member(&group_id, bob_p.id.as_str()).await;
    alice.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    alice.send_channel_message(&channel_id, "after-revoke-1").await;

    for (c, label) in [(&alice, "alice"), (&carol, "carol")] {
        assert!(
            contents(c, &channel_id).await.contains(&"after-revoke-1".to_string()),
            "{label} should decrypt 'after-revoke-1'"
        );
    }

    // These helpers panic on Err, so reaching the assertions proves no panic.
    bob.process_commits_for(&channel_id).await;
    let bob_contents = contents(&bob, &channel_id).await;

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

    alice.send_channel_message(&channel_id, "after-revoke-2").await;
    carol.process_commits_for(&channel_id).await;
    assert!(
        contents(&carol, &channel_id).await.contains(&"after-revoke-2".to_string()),
        "group wedged after the revoked-device recovery attempts — carol could not \
         receive a new message"
    );

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

/// #666: a passive attacker with bob's stolen state reads until alice commits, and loses access once bob commits.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_stolen_leaf_is_locked_out_once_the_victim_rotates() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("PCS").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    alice.invite(&group_id, &bob_p.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob has a pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    // Steal after the join-time rotation, so the lockout is credited to this one.
    alice.send_channel_message(&channel_id, "before-steal").await;
    bob.process_commits_for(&channel_id).await;
    let _ = bob.fetch_channel_messages(&channel_id).await;

    let attacker = steal_leaf(&bob).await;
    assert_eq!(attacker.victim(), bob_p.id, "we stole the right device");

    alice.send_channel_message(&channel_id, "attacker-can-read-this").await;
    assert!(
        attacker
            .can_read(&channel_id, &group_id, "attacker-can-read-this")
            .await,
        "the stolen state could not read traffic at the epoch it was stolen at — \
         the compromise never happened, so the lockout below would prove nothing"
    );

    // Control: alice's UpdatePath reaches bob's stolen leaf, so epoch churn alone doesn't evict.
    assert!(
        alice.self_update(&group_id).await,
        "alice's rotation must land — she is the only committer at this point"
    );
    bob.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "epoch-moved-but-still-readable").await;
    assert!(
        attacker
            .can_read(&channel_id, &group_id, "epoch-moved-but-still-readable")
            .await,
        "the attacker fell off the commit chain after a commit by someone OTHER than \
         its victim. That is not post-compromise security, it is a stalled replay — \
         and it would make the lockout below vacuous, since any epoch change would \
         produce it. Fix the model, not the assertion."
    );

    // An UpdatePath carries no ciphertext for the committer's own leaf.
    assert!(
        bob.self_update(&group_id).await,
        "bob's rotation must land — he is the only committer at this point"
    );
    alice.process_commits_for(&channel_id).await;

    // The attacker's replay fails with "Cannot decrypt own messages" and does not external-join.
    alice.send_channel_message(&channel_id, "attacker-must-not-read-this").await;
    bob.process_commits_for(&channel_id).await;
    assert!(
        !attacker
            .can_read(&channel_id, &group_id, "attacker-must-not-read-this")
            .await,
        "NO POST-COMPROMISE SECURITY: a stolen leaf still decrypted traffic sealed \
         after the victim rotated. The self-update either did not rotate the leaf \
         key or did not land."
    );

    let bob_view = contents(&bob, &channel_id).await;
    assert!(
        bob_view.contains(&"attacker-must-not-read-this".to_string()),
        "bob must still read his own group after rotating, got: {bob_view:?}"
    );
    assert!(
        ds_head_epoch(&group_id).await >= 2,
        "two rotations must have advanced the canonical log"
    );
    assert_eq!(
        alice.group_member_ids(&group_id).await.len(),
        2,
        "rotation is not a membership change"
    );
    let _ = alice_p;

    drop(alice);
    drop(bob);
}

/// #679: revoking one device, with no roster change, must prune its leaf on its own.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn revoking_one_device_prunes_its_leaf_with_no_roster_change() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("RevokeDevice").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    // Carol is the control, so a pass can't be earned by wedging the group.
    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    bob.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    alice.send_channel_message(&channel_id, "before-revoke").await;
    bob.process_commits_for(&channel_id).await;
    assert!(
        contents(&bob, &channel_id)
            .await
            .contains(&"before-revoke".to_string()),
        "SETUP FAILED: bob never became a decrypting member, so this test cannot \
         demonstrate anything about revocation"
    );

    // Deliberately no `remove_member`: that is the entire bug.
    {
        let remote = writable_remote().await;
        let conn = remote.conn().await.expect("remote conn");
        let affected = conn
            .execute(
                "UPDATE user_device SET revoked_at = datetime('now') \
                 WHERE user_id = ?1 AND revoked_at IS NULL",
                libsql::params![bob_p.id.clone()],
            )
            .await
            .expect("tombstone bob's device");
        assert_eq!(affected, 1, "exactly one device row tombstoned");
    }

    // Scope guard: a future `remove_member` would make this a duplicate test.
    assert!(
        alice.group_member_ids(&group_id).await.contains(&bob_p.id),
        "bob must still be a roster member — only his DEVICE was revoked; without \
         this, the leaf would be pruned by the removal and #679 would go untested"
    );

    // The sweep's backstop is now the only thing that can prune the leaf.
    alice.sweep().await;
    alice.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    alice.send_channel_message(&channel_id, "after-revoke").await;

    carol.process_commits_for(&channel_id).await;
    assert!(
        contents(&carol, &channel_id)
            .await
            .contains(&"after-revoke".to_string()),
        "carol is an unaffected member and must still receive messages — pruning a \
         revoked device must not wedge the group"
    );

    bob.process_commits_for(&channel_id).await;
    bob.sweep().await;
    bob.process_commits_for(&channel_id).await;
    let bob_view = contents(&bob, &channel_id).await;
    assert!(
        !bob_view.contains(&"after-revoke".to_string()),
        "REVOCATION LEAK (#679): bob's REVOKED device decrypted a message sent after \
         revocation, with no roster change to prune its leaf. Revocation is cosmetic — \
         the device keeps reading every future message and keeps deriving the call media \
         key. bob view={bob_view:?}"
    );

    // Still a roster member: the eviction was device-scoped.
    assert!(
        alice.group_member_ids(&group_id).await.contains(&bob_p.id),
        "bob's leaf must be pruned WITHOUT removing him from the roster"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// Raw envelope count on the writable world handle.
async fn gc_envelope_count(conversation_id: &str) -> i64 {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("count query");
    rows.next().await.expect("row").expect("some row").get::<i64>(0).expect("count")
}

/// #685: a revoked, never-synced device must not wedge the watermark GC gate.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn revoked_device_does_not_wedge_envelope_gc() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("GcRevoke").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;

    let remote = writable_remote().await;

    // A revoked second device with no watermark row.
    {
        let conn = remote.conn().await.expect("remote conn");
        let affected = conn
            .execute(
                "INSERT INTO user_device (device_id, user_id, revoked_at) \
                 VALUES ('bob-ghost', ?1, datetime('now'))",
                libsql::params![bob_p.id.clone()],
            )
            .await
            .expect("insert revoked ghost device");
        assert_eq!(affected, 1, "exactly one revoked device row inserted");
    }

    // Reset the add-member-seeded watermarks.
    remote
        .conn()
        .await
        .expect("remote conn")
        .execute(
            "DELETE FROM conversation_watermark WHERE conversation_id = ?1",
            libsql::params![channel_id.clone()],
        )
        .await
        .expect("clear watermarks");

    alice.send_channel_message(&channel_id, "old-msg").await;
    alice.fetch_channel_messages(&channel_id).await;
    bob.fetch_channel_messages(&channel_id).await;

    alice.send_channel_message(&channel_id, "new-msg").await;
    alice.fetch_channel_messages(&channel_id).await;

    // Setup check: nothing is pruned yet regardless of the fix.
    assert_eq!(
        gc_envelope_count(&channel_id).await,
        2,
        "SETUP: both envelopes present before every live device catches up"
    );

    // Pre-#685 the ghost disabled the gate; the sweep drives GC (#689).
    bob.fetch_channel_messages(&channel_id).await;
    {
        let conn = remote.conn().await.expect("remote conn");
        let stale = pollis_delivery::messages::watermark_stale_modifier();
        pollis_delivery::messages::sweep_envelope_gc(&conn, &stale)
            .await
            .expect("gc sweep");
    }

    // `seq <= MIN(last_seq)` (#1087), so both go; a wedge would leave both.
    assert_eq!(
        gc_envelope_count(&channel_id).await,
        0,
        "GC WEDGED (#685): a revoked device with no watermark disabled envelope \
         pruning — the live devices have read past both envelopes, so both must go."
    );

    assert!(
        contents(&alice, &channel_id).await.contains(&"new-msg".to_string()),
        "alice must still read new-msg — GC must not have broken the live channel"
    );

    drop(alice);
    drop(bob);
}

/// #680: an undeserializable commit must trigger `Recover(MalformedCommit)` and external-join, not a wedge.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn corrupt_commit_recovers_instead_of_wedging() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let mut dave = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;
    let dave_p = dave.sign_up("dave@test.local").await;

    let group_id = alice.create_group("Wedge").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;

    alice.send_channel_message(&channel_id, "M1-at-join-epoch").await;

    // Read off the log, not counted (#666).
    let corrupt_epoch = ds_head_epoch(&group_id).await;
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;

    // Keep the corrupt row interior.
    join_member(&alice, &dave, &group_id, &channel_id, &dave_p.username).await;
    let head_before = ds_head_epoch(&group_id).await;
    assert!(
        head_before > corrupt_epoch + 1,
        "the dave add must sit above the corrupt epoch {corrupt_epoch} (head {head_before})"
    );

    // Epoch column intact, so the replay reaches `tls_deserialize`.
    corrupt_commit_row(&group_id, corrupt_epoch).await;
    assert_eq!(
        ds_head_epoch(&group_id).await,
        head_before,
        "corrupting a row's bytes must not change the head"
    );

    let bob_after_return = contents(&bob, &channel_id).await;
    assert!(
        bob_after_return.contains(&"M1-at-join-epoch".to_string()),
        "bob must retain the message at his join epoch (decrypted before the corrupt \
         commit is hit), got: {bob_after_return:?}"
    );

    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "after-recovery").await;

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&bob_p.id),
        "bob must be a current member after corrupt-commit recovery, got: {members:?}"
    );
    assert!(
        contents(&bob, &channel_id).await.contains(&"after-recovery".to_string()),
        "bob must decrypt post-recovery traffic — he wedged on the corrupt commit otherwise \
         (the 'exists locally but can never advance' state #680 makes unreachable)"
    );

    dave.process_commits_for(&channel_id).await;
    assert!(
        contents(&dave, &channel_id).await.contains(&"after-recovery".to_string()),
        "dave must decrypt post-recovery traffic — the group forked at the corrupt commit otherwise"
    );

    drop(alice);
    drop(bob);
    drop(carol);
    drop(dave);
}

/// #1081: MLS forbids a self-remove, so the leave wake-up must make a remaining member stage it.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_leave_is_committed_by_a_remaining_member_not_left_for_the_sweep() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("LeaveEvict").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    bob.process_commits_for(&channel_id).await;
    carol.process_commits_for(&channel_id).await;

    alice.send_channel_message(&channel_id, "pre-leave").await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"pre-leave".to_string()),
        "bob must decrypt while still a member"
    );

    bob.leave_group(&group_id).await;

    // Leaving can't remove the leaf: MLS refuses a self-remove.
    let before = pollis_lib::commands::mls::local_tree_members(&alice.state, &group_id).await;
    assert!(
        before.iter().any(|(u, _)| u == &bob_p.id),
        "bob's leaf must still be in alice's tree immediately after he leaves: {before:?}"
    );

    // No sweep: only the wake-up.
    pollis_lib::commands::mls::apply_membership_wake(
        &alice.state,
        &group_id,
        &alice_p.id,
        true,
    )
    .await;

    let after = pollis_lib::commands::mls::local_tree_members(&alice.state, &group_id).await;
    assert!(
        !after.iter().any(|(u, _)| u == &bob_p.id),
        "LEAVE EVICTION GAP: bob's leaf is still in alice's tree after she handled the leave \
         wake-up — nothing staged the remove commit, so every message she seals from here is \
         still sealed for the leaf of a device that has left. tree={after:?}"
    );

    assert!(
        after.iter().any(|(u, _)| u == &carol_p.id),
        "carol must still be in the tree: {after:?}"
    );

    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "post-leave").await;

    carol.poll().await;
    carol.process_commits_for(&channel_id).await;
    assert!(
        contents(&carol, &channel_id).await.contains(&"post-leave".to_string()),
        "carol (a continuous member) must decrypt the post-leave message"
    );

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        !members.contains(&bob_p.id),
        "bob should not be a current member after leaving, got: {members:?}"
    );

    // His wiped view proves little; the tree walk above is load-bearing.
    bob.poll().await;
    bob.process_commits_for(&channel_id).await;
    assert!(
        !contents(&bob, &channel_id).await.contains(&"post-leave".to_string()),
        "a wiped leaver must read nothing"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// #1086: a member cannot delete another's envelope; the client really sends the capability hash.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_member_cannot_delete_another_members_envelope() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("CapDelete").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;

    let message_id = alice
        .send_channel_message_id(&channel_id, "alice's message")
        .await;

    let conn = writable_remote().await.conn().await.expect("remote conn");

    // Otherwise the test passes vacuously on the legacy NULL path.
    let stored: Option<String> = {
        let mut rows = conn
            .query(
                "SELECT delete_token_hash FROM message_envelope WHERE id = ?1",
                libsql::params![message_id.clone()],
            )
            .await
            .expect("select");
        rows.next()
            .await
            .expect("row")
            .expect("alice's envelope must exist")
            .get::<Option<String>>(0)
            .expect("column")
    };
    assert!(
        stored.is_some(),
        "the send path must attach a deletion capability, or this proves nothing"
    );

    // Bob passes membership but can't produce Alice's capability.
    let attack = pollis_delivery::messages::apply_delete_message(
        &conn,
        Some(bob_p.id.as_str()),
        &pollis_api::messages::DeleteMessageBody {
            message_id: message_id.clone(),
            conversation_id: channel_id.clone(),
            msg_sender_id: Some(bob_p.id.clone()),
            actor_id: Some(bob_p.id.clone()),
            delete_token: None,
        },
    )
    .await
    .expect("the call itself succeeds; the outcome is the assertion");
    assert!(
        matches!(attack, pollis_delivery::writes::WriteOutcome::Forbidden),
        "a member must not delete another member's envelope, got {attack:?}"
    );

    let survives: i64 = {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM message_envelope WHERE id = ?1",
                libsql::params![message_id.clone()],
            )
            .await
            .expect("count");
        rows.next().await.expect("row").expect("some").get(0).expect("count")
    };
    assert_eq!(
        survives, 1,
        "MESSAGE LOSS: bob removed alice's envelope before its recipients fetched it"
    );

    bob.process_commits_for(&channel_id).await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"alice's message".to_string()),
        "bob must receive the message he could not delete"
    );

    // Control: the author still deletes her own.
    alice.delete_message(&message_id).await;
    let after: i64 = {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM message_envelope WHERE id = ?1",
                libsql::params![message_id.clone()],
            )
            .await
            .expect("count");
        rows.next().await.expect("row").expect("some").get(0).expect("count")
    };
    assert_eq!(after, 0, "the author's own delete must still remove the envelope");

    drop(alice);
    drop(bob);
}
