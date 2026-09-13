//! Leaf cross-signing: a device gets into an MLS tree only on a KeyPackage whose
//! leaf key the claimed user's own `account_id_pub` certified.
//!
//! Threat model (`docs/security-whitepaper.md` §5.3): the Delivery Service is
//! untrusted. At claim time it can hand the committer ANY bytes as "bob's
//! KeyPackage" — including one an attacker generated, self-signed with the
//! attacker's leaf key, carrying the credential `bob:bobs-device`.
//! `KeyPackageIn::validate` accepts it (it is self-consistent) and the credential
//! string matches, so before this suite the only thing standing between that
//! package and the group's key schedule was a log line on OTHER members' replay.
//!
//! Two invariants, each with its own scenario:
//!
//! 1. **The committer refuses.** `reconcile` never turns a KeyPackage into an
//!    Add unless its leaf key is the device key bob's account certified. The
//!    forged package is burnt and reported (`refused_uncertified`); nothing is
//!    committed; no Welcome exists for the attacker to open.
//! 2. **Replaying members flag and evict.** A committer that does add such a
//!    leaf (a pre-fix client, or a malicious member — modelled with a
//!    harness-only switch) cannot make it stick: every honest member derives
//!    the added leaves from the commit's OWN Add proposals, records an
//!    `uncertified_mls_leaf` security event, and the next reconcile removes
//!    the leaf — append-only, on the canonical branch.
//!
//! Both are asserted the way the rest of the harness asserts membership:
//! through the real command path, the local ratchet tree, and the DS log.

use crate::harness::{welcome_blobs, wipe, writable_remote, TestClient};
use serial_test::serial;

/// Code point of the one production suite (`CS_PQ`), as stored in
/// `mls_key_package.ciphersuite`.
const CS_PQ_CODE_POINT: i64 = 0x0052;

/// Play the malicious server: replace every unclaimed KeyPackage `victim` has
/// published with `count` forged ones that claim the victim's credential but
/// carry a leaf key nobody certified. The forgery is minted on the attacker's
/// own client (its keys never touch the victim), and planted through the
/// writable remote handle — the DS's owner-scoped publish endpoint would refuse
/// it, which is exactly why a DS *operator* is the adversary here.
async fn substitute_forged_key_packages(attacker: &TestClient, victim: &TestClient, count: usize) {
    let victim_id = victim.user_id().to_string();
    let victim_device = victim
        .state
        .device_id
        .lock()
        .await
        .clone()
        .expect("victim device_id");

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    conn.execute(
        "DELETE FROM mls_key_package WHERE user_id = ?1",
        libsql::params![victim_id.clone()],
    )
    .await
    .expect("drop victim key packages");

    for _ in 0..count {
        let (ref_hash, kp_bytes) = pollis_lib::commands::mls::forge_key_package_for(
            &attacker.state,
            &victim_id,
            &victim_device,
        )
        .await
        .expect("forge key package");
        conn.execute(
            "INSERT INTO mls_key_package (ref_hash, user_id, key_package, claimed, device_id, ciphersuite) \
             VALUES (?1, ?2, ?3, 0, ?4, ?5)",
            libsql::params![
                ref_hash,
                victim_id.clone(),
                kp_bytes,
                victim_device.clone(),
                CS_PQ_CODE_POINT
            ],
        )
        .await
        .expect("plant forged key package");
    }
}

async fn device_of(client: &TestClient) -> String {
    client
        .state
        .device_id
        .lock()
        .await
        .clone()
        .expect("device_id")
}

/// Scenario 1 — the committer refuses a KeyPackage whose leaf is not
/// cross-signed by the claimed account.
///
/// alice creates a group. The server swaps bob's KeyPackage pool for forgeries.
/// alice invites bob: her reconcile claims a forgery, sees a leaf key that is
/// not the key bob's cert certifies, and refuses. The group's epoch does not
/// move, no Welcome exists, and bob's device is not in alice's tree. Then the
/// control: bob republishes his REAL pool and alice's next reconcile admits him.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn committer_refuses_a_key_package_not_cross_signed_by_the_claimed_account() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut attacker = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    attacker.sign_up("attacker@test.local").await;
    let bob_device = device_of(&bob).await;

    let group_id = alice.create_group("Cross-signed").await;
    let epoch_before = crate::harness::ds_head_epoch(&group_id).await;

    // Two forgeries: the invite's reconcile burns one, the explicit reconcile
    // below burns the other — a refused package is claimed and discarded, never
    // handed back, so each attempt costs the attacker a fresh forgery.
    substitute_forged_key_packages(&attacker, &bob, 2).await;

    // The invite path runs reconcile internally; its outcome is logged, not
    // returned, so the observable assertions are made against the DS log and
    // alice's tree.
    alice.invite(&group_id, &bob_profile.username).await;

    // Run the SAME reconcile once more, directly, to read its verdict.
    let outcome = pollis_lib::commands::mls::reconcile_group_mls_impl(
        &alice.state,
        &group_id,
        &alice_profile.id,
    )
    .await
    .expect("reconcile runs");
    assert!(
        outcome.added.is_empty(),
        "a forged KeyPackage must never become an Add: {:?}",
        outcome.added
    );
    assert!(
        outcome
            .refused_uncertified
            .iter()
            .any(|(u, d, _)| u == &bob_profile.id && d == &bob_device),
        "the refusal must be reported for bob's device, got {:?}",
        outcome.refused_uncertified
    );

    // Nothing was committed, so no epoch advanced and no Welcome was written —
    // the attacker holding the forged leaf's private key has nothing to open.
    assert_eq!(
        crate::harness::ds_head_epoch(&group_id).await,
        epoch_before,
        "refusing an add must not publish a commit"
    );
    assert!(
        welcome_blobs(&group_id).await.is_empty(),
        "no Welcome may exist for a leaf that was never added"
    );
    let members = pollis_lib::commands::mls::local_tree_members(&alice.state, &group_id).await;
    assert!(
        !members.iter().any(|(u, _)| u == &bob_profile.id),
        "bob's forged leaf must not be in alice's tree: {members:?}"
    );

    // Control: bob's REAL pool (signed by the device key his cert certifies)
    // is admitted by the very same check.
    crate::harness::republish_key_packages(&bob).await;
    let outcome = pollis_lib::commands::mls::reconcile_group_mls_impl(
        &alice.state,
        &group_id,
        &alice_profile.id,
    )
    .await
    .expect("reconcile runs");
    assert!(
        outcome
            .added
            .iter()
            .any(|(u, d)| u == &bob_profile.id && d == &bob_device),
        "bob's genuine KeyPackage must be admitted: {:?}",
        outcome.added
    );
    assert!(outcome.refused_uncertified.is_empty(), "{:?}", outcome.refused_uncertified);

    // And bob can actually join and read.
    let invite = bob.first_pending_invite().await.expect("pending invite");
    let invite_id = invite["id"].as_str().expect("invite id").to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    let channel = alice.general_channel_id(&group_id).await;
    alice.send_channel_message(&channel, "hello bob").await;
    let msgs = bob.fetch_channel_messages(&channel).await;
    assert!(
        msgs.iter().any(|m| m["content"] == "hello bob"),
        "bob, admitted on his real leaf, must decrypt: {msgs:?}"
    );

    drop(alice);
    drop(bob);
    drop(attacker);
}

/// Scenario 2 — a rogue leaf that a committer DID add is flagged by every
/// replaying member, from the commit itself, and evicted on the next reconcile.
///
/// alice + carol share a group. The server swaps bob's pool for a forgery and
/// alice — with the committer-side check switched off, playing a pre-fix or
/// malicious client — adds the forged leaf. carol replays the commit: she
/// derives the added leaf from the commit's Add proposal (the DS columns are
/// not consulted), records an `uncertified_mls_leaf` security event, and her
/// reconcile removes the leaf. alice's own tree, having replayed the eviction,
/// agrees.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn inbound_replay_flags_and_evicts_a_leaf_not_cross_signed_by_its_account() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut attacker = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    attacker.sign_up("attacker@test.local").await;
    let bob_device = device_of(&bob).await;

    let group_id = alice.create_group("Flagged").await;
    alice.invite(&group_id, &carol_profile.username).await;
    let invite = carol.first_pending_invite().await.expect("carol's invite");
    carol
        .accept_invite(invite["id"].as_str().expect("invite id"))
        .await;
    carol.poll().await;
    let channel = alice.general_channel_id(&group_id).await;
    alice.send_channel_message(&channel, "before").await;
    assert!(carol
        .fetch_channel_messages(&channel)
        .await
        .iter()
        .any(|m| m["content"] == "before"));

    // The rogue add. `set_skip_committer_leaf_check` exists only under the
    // test-harness feature; it is reset before any assertion so the eviction
    // below is the PRODUCTION reconcile.
    substitute_forged_key_packages(&attacker, &bob, 1).await;
    pollis_lib::commands::mls::set_skip_committer_leaf_check(true);
    alice.invite(&group_id, &bob_profile.username).await;
    pollis_lib::commands::mls::set_skip_committer_leaf_check(false);
    let members = pollis_lib::commands::mls::local_tree_members(&alice.state, &group_id).await;
    assert!(
        members.iter().any(|(u, d)| u == &bob_profile.id && d == &bob_device),
        "precondition: the rogue leaf is in alice's tree: {members:?}"
    );

    // carol replays the commit. The leaf is read off the commit — the harness
    // did not touch the add-metadata columns, and it would not matter if it had.
    carol.process_commits_for(&group_id).await;
    let events = carol
        .invoke_json(
            "list_security_events",
            serde_json::json!({ "userId": carol_profile.id, "limit": 50 }),
        )
        .await;
    let events = events.as_array().cloned().unwrap_or_default();
    assert!(
        events.iter().any(|e| {
            e["kind"] == "uncertified_mls_leaf" && e["device_id"] == bob_device.as_str()
        }),
        "carol must record an uncertified_mls_leaf security event for bob's device: {events:?}"
    );

    // Eviction. carol's replay also kicked a detached reconcile; running it
    // here directly makes the outcome observable and deterministic — whichever
    // of the two lands, the tree ends without the rogue leaf.
    let _ = pollis_lib::commands::mls::reconcile_group_mls_impl(
        &carol.state,
        &group_id,
        &carol_profile.id,
    )
    .await
    .expect("carol's reconcile runs");
    let members = pollis_lib::commands::mls::local_tree_members(&carol.state, &group_id).await;
    assert!(
        !members.iter().any(|(u, d)| u == &bob_profile.id && d == &bob_device),
        "the rogue leaf must be evicted from carol's tree: {members:?}"
    );

    // alice replays the eviction and agrees; the group keeps working.
    alice.process_commits_for(&group_id).await;
    let members = pollis_lib::commands::mls::local_tree_members(&alice.state, &group_id).await;
    assert!(
        !members.iter().any(|(u, d)| u == &bob_profile.id && d == &bob_device),
        "alice must converge on the eviction: {members:?}"
    );
    alice.send_channel_message(&channel, "after").await;
    assert!(
        carol
            .fetch_channel_messages(&channel)
            .await
            .iter()
            .any(|m| m["content"] == "after"),
        "the group must keep delivering after the eviction"
    );

    drop(alice);
    drop(carol);
    drop(bob);
    drop(attacker);
}
