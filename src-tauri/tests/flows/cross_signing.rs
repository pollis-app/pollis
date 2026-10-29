//! Leaf cross-signing: a device gets into an MLS tree only on a KeyPackage whose
//! leaf key the claimed user's own `account_id_pub` certified.
//!
//! The untrusted DS can hand a committer a self-consistent forged KeyPackage (whitepaper §5.3).

use crate::harness::{welcome_blobs, wipe, writable_remote, TestClient};
use serial_test::serial;

const CS_PQ_CODE_POINT: i64 = 0x0052;

/// Malicious server: replace `victim`'s pool with forgeries, planted directly (the DS endpoint would refuse).
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

/// The committer refuses an uncertified leaf; then bob's real pool is admitted.
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

    // Two: each refused attempt burns one.
    substitute_forged_key_packages(&attacker, &bob, 2).await;

    // Its outcome is logged, not returned.
    alice.invite(&group_id, &bob_profile.username).await;

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

/// A rogue leaf a committer DID add is flagged by replaying members and evicted.
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

    // Reset before any assertion, so the eviction is the production reconcile.
    substitute_forged_key_packages(&attacker, &bob, 1).await;
    pollis_lib::commands::mls::set_skip_committer_leaf_check(true);
    alice.invite(&group_id, &bob_profile.username).await;
    pollis_lib::commands::mls::set_skip_committer_leaf_check(false);
    let members = pollis_lib::commands::mls::local_tree_members(&alice.state, &group_id).await;
    assert!(
        members.iter().any(|(u, d)| u == &bob_profile.id && d == &bob_device),
        "precondition: the rogue leaf is in alice's tree: {members:?}"
    );

    // The leaf is read off the commit, not the add-metadata columns.
    carol.process_commits_for(&group_id).await;
    let events = carol
        .invoke_json(
            "list_security_events",
            serde_json::json!({ "userId": carol_profile.id, "limit": 50 }),
        )
        .await;
    let events = events["events"].as_array().cloned().unwrap_or_default();
    assert!(
        events.iter().any(|e| {
            e["kind"] == "uncertified_mls_leaf" && e["device_id"] == bob_device.as_str()
        }),
        "carol must record an uncertified_mls_leaf security event for bob's device: {events:?}"
    );

    // Run directly for determinism; either reconcile removes the leaf.
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
