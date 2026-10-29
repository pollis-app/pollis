//! #1571: a device left outside a DM's MLS tree, silently. Each test pins one layer of the fix.

use crate::harness::{enroll_second_device, expect_losses, wipe, writable_log, writable_remote, TestClient};
use serial_test::serial;

async fn identity(client: &TestClient) -> (String, String) {
    let device = client
        .state
        .device_id
        .lock()
        .await
        .clone()
        .expect("device id set");
    (client.user_id().to_string(), device)
}

async fn tree_has(client: &TestClient, conversation_id: &str, who: &(String, String)) -> bool {
    pollis_lib::commands::mls::local_tree_members(&client.state, conversation_id)
        .await
        .contains(who)
}

async fn holds_group(client: &TestClient, conversation_id: &str) -> bool {
    let guard = client.state.local_db.lock().await;
    guard
        .as_ref()
        .is_some_and(|db| pollis_lib::commands::mls::has_local_group(db.conn(), conversation_id))
}

/// Readable bodies and placeholder count after opening `dm`.
async fn open_dm(client: &TestClient, dm: &str) -> (Vec<String>, usize) {
    let rows = client.fetch_dm_messages(dm).await;
    let bodies = rows
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect();
    let placeholders = rows
        .iter()
        .filter(|m| m["undecryptable"].as_bool() == Some(true))
        .count();
    (bodies, placeholders)
}

/// Spend every REGULAR package of `who`; the last-resort one stays.
async fn burn_regular_pool(who: &(String, String)) {
    writable_remote()
        .await
        .conn()
        .await
        .expect("remote conn")
        .execute(
            "UPDATE mls_key_package SET claimed = 1 \
             WHERE user_id = ?1 AND device_id = ?2 AND last_resort = 0",
            libsql::params![who.0.clone(), who.1.clone()],
        )
        .await
        .expect("burn the pool");
}

/// Remove every package, as on a pre-last-resort build.
async fn drop_all_packages(who: &(String, String)) {
    writable_remote()
        .await
        .conn()
        .await
        .expect("remote conn")
        .execute(
            "DELETE FROM mls_key_package WHERE user_id = ?1 AND device_id = ?2",
            libsql::params![who.0.clone(), who.1.clone()],
        )
        .await
        .expect("drop the packages");
}

/// `(unclaimed regular, last-resort rows, claimed last-resort rows)` of `who`.
async fn pool(who: &(String, String)) -> (i64, i64, i64) {
    let conn = writable_remote().await.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT \
                 COUNT(*) FILTER (WHERE last_resort = 0 AND claimed = 0), \
                 COUNT(*) FILTER (WHERE last_resort = 1), \
                 COUNT(*) FILTER (WHERE last_resort = 1 AND claimed = 1) \
             FROM mls_key_package WHERE user_id = ?1 AND device_id = ?2",
            libsql::params![who.0.clone(), who.1.clone()],
        )
        .await
        .expect("count");
    let row = rows.next().await.expect("row").expect("row");
    (row.get(0).unwrap(), row.get(1).unwrap(), row.get(2).unwrap())
}

async fn alice_two_devices_and_bob() -> (TestClient, TestClient, TestClient, String, String) {
    let mut alice_d1 = TestClient::new().await;
    let alice = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    // `created_at` is second-granular (`join_branch::ds_instant_window`).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    (alice_d1, alice_d2, bob, alice.id, bob_profile.id)
}

/// Empty regular pool: the last-resort package is claimed and stays unclaimed for the next add.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_device_with_an_empty_pool_is_added_through_its_last_resort_package() {
    wipe().await;
    let (alice_d1, alice_d2, bob, alice_id, bob_id) = alice_two_devices_and_bob().await;
    let d2 = identity(&alice_d2).await;
    burn_regular_pool(&d2).await;
    assert_eq!(pool(&d2).await, (0, 1, 0), "precondition: only the last-resort package left");

    let dm = bob.create_dm(&[bob_id.as_str(), alice_id.as_str()]).await;
    alice_d1.accept_dm_request(&dm).await;
    bob.send_channel_message(&dm, "bob first").await;
    alice_d1.fetch_dm_messages(&dm).await;
    alice_d1.send_channel_message(&dm, "alice from d1").await;

    bob.fetch_dm_messages(&dm).await;
    assert!(tree_has(&bob, &dm, &d2).await, "alice_d2 must have been added at create time");

    // alice_d2 wakes only now — it ran nothing while the DM was made and used.
    alice_d2.poll().await;
    let (bodies, placeholders) = open_dm(&alice_d2, &dm).await;
    assert!(bodies.contains(&"bob first".to_string()), "got {bodies:?}");
    assert!(bodies.contains(&"alice from d1".to_string()), "got {bodies:?}");
    assert_eq!(placeholders, 0);

    let (_, last_resort, claimed) = pool(&d2).await;
    assert_eq!((last_resort, claimed), (1, 0), "the last-resort package is never consumed");

    // And it admits the device again, to a second conversation, on the same
    // empty pool.
    let mut carol = TestClient::new().await;
    let carol_profile = carol.sign_up("carol@test.local").await;
    let dm2 = carol.create_dm(&[carol_profile.id.as_str(), alice_id.as_str()]).await;
    carol.fetch_dm_messages(&dm2).await;
    assert!(tree_has(&carol, &dm2, &d2).await, "the same last-resort package adds it again");
    carol.send_channel_message(&dm2, "carol here").await;
    alice_d2.poll().await;
    let (bodies, _) = open_dm(&alice_d2, &dm2).await;
    assert!(bodies.contains(&"carol here".to_string()), "got {bodies:?}");

    drop((alice_d1, alice_d2, bob, carol));
}

/// Pre-last-resort build: reconcile names the device, and its late join shows what it missed.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_device_with_nothing_to_claim_is_reported_and_its_late_join_is_shown() {
    wipe().await;
    expect_losses();
    let (alice_d1, alice_d2, bob, alice_id, bob_id) = alice_two_devices_and_bob().await;
    let d2 = identity(&alice_d2).await;
    drop_all_packages(&d2).await;

    let dm = bob.create_dm(&[bob_id.as_str(), alice_id.as_str()]).await;
    let outcome = pollis_lib::commands::mls::reconcile_group_mls_impl(&bob.state, &dm, &bob_id)
        .await
        .expect("reconcile");
    assert!(
        outcome.skipped_no_suite_kp.contains(&d2),
        "a registered roster device with no leaf and nothing to claim must be reported, got {:?}",
        outcome.skipped_no_suite_kp
    );

    alice_d1.accept_dm_request(&dm).await;
    bob.send_channel_message(&dm, "sent while d2 was out").await;
    alice_d1.fetch_dm_messages(&dm).await;
    alice_d1.send_channel_message(&dm, "also while d2 was out").await;
    assert!(!tree_has(&bob, &dm, &d2).await, "precondition: alice_d2 was left out");

    let (bodies, placeholders) = open_dm(&alice_d2, &dm).await;
    assert!(holds_group(&alice_d2, &dm).await, "opening the DM joins it");
    assert!(
        !bodies.iter().any(|b| b.contains("while d2 was out")),
        "those were sealed before it joined and cannot be read, got {bodies:?}"
    );
    assert!(
        placeholders >= 2,
        "both messages it missed must be shown as placeholders, not passed in silence \
         (got {placeholders})"
    );

    bob.send_channel_message(&dm, "after d2 joined").await;
    let (bodies, _) = open_dm(&alice_d2, &dm).await;
    assert!(bodies.contains(&"after d2 joined".to_string()), "got {bodies:?}");

    drop((alice_d1, alice_d2, bob));
}

/// Loss #2 stays silent: a device enrolled after the DM starts empty, no placeholder.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_new_device_still_starts_empty_without_placeholders() {
    wipe().await;
    let mut alice_d1 = TestClient::new().await;
    let alice = alice_d1.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let dm = bob.create_dm(&[bob_profile.id.as_str(), alice.id.as_str()]).await;
    alice_d1.accept_dm_request(&dm).await;
    bob.send_channel_message(&dm, "before the new device").await;
    alice_d1.fetch_dm_messages(&dm).await;

    // `created_at` is second-granular.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let (bodies, placeholders) = open_dm(&alice_d2, &dm).await;
    assert!(bodies.is_empty(), "a new device has no history, got {bodies:?}");
    assert_eq!(placeholders, 0, "and says nothing about it");

    bob.send_channel_message(&dm, "after the new device").await;
    let (bodies, _) = open_dm(&alice_d2, &dm).await;
    assert!(bodies.contains(&"after the new device".to_string()), "got {bodies:?}");

    drop((alice_d1, alice_d2, bob));
}

/// Mobile-only roster: bob's next send re-adds alice_d2 before sealing.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_sender_re_adds_a_missing_device_before_it_seals() {
    wipe().await;
    let (alice_d1, alice_d2, bob, alice_id, bob_id) = alice_two_devices_and_bob().await;
    let d2 = identity(&alice_d2).await;
    drop_all_packages(&d2).await;

    let dm = bob.create_dm(&[bob_id.as_str(), alice_id.as_str()]).await;
    alice_d1.accept_dm_request(&dm).await;
    assert!(!tree_has(&bob, &dm, &d2).await, "precondition: alice_d2 was left out");

    crate::harness::republish_key_packages(&alice_d2).await;
    bob.send_channel_message(&dm, "first message").await;
    assert!(tree_has(&bob, &dm, &d2).await, "the send must have re-added alice_d2 first");

    alice_d2.poll().await;
    let (bodies, placeholders) = open_dm(&alice_d2, &dm).await;
    assert!(bodies.contains(&"first message".to_string()), "got {bodies:?}");
    assert_eq!(placeholders, 0);

    drop((alice_d1, alice_d2, bob));
}

/// Mobile's `reconcile_backstop_all` re-adds the device with no sweep and no send.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_mobile_backstop_re_adds_a_missing_device() {
    wipe().await;
    let (alice_d1, alice_d2, bob, alice_id, bob_id) = alice_two_devices_and_bob().await;
    let d2 = identity(&alice_d2).await;
    drop_all_packages(&d2).await;

    let dm = bob.create_dm(&[bob_id.as_str(), alice_id.as_str()]).await;
    alice_d1.accept_dm_request(&dm).await;
    assert!(!tree_has(&bob, &dm, &d2).await, "precondition: alice_d2 was left out");

    crate::harness::republish_key_packages(&alice_d2).await;
    alice_d1.fetch_dm_messages(&dm).await;
    let report = pollis_core::commands::mls::reconcile_backstop_all(&alice_d1.state, &alice_id)
        .await
        .expect("backstop");
    assert_eq!(report.reconciled, 1, "got {report:?}");
    assert!(tree_has(&alice_d1, &dm, &d2).await);

    bob.send_channel_message(&dm, "after the backstop").await;
    alice_d2.poll().await;
    let (bodies, _) = open_dm(&alice_d2, &dm).await;
    assert!(bodies.contains(&"after the backstop".to_string()), "got {bodies:?}");

    let again = pollis_core::commands::mls::reconcile_backstop_all(&alice_d1.state, &alice_id)
        .await
        .expect("backstop");
    assert_eq!(again.reconciled, 0, "got {again:?}");

    drop((alice_d1, alice_d2, bob));
}

/// An unappliable Welcome is acknowledged and the device re-joins in the same pass.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_welcome_that_fails_to_apply_is_acked_and_the_device_rejoins_at_once() {
    wipe().await;
    expect_losses();
    let (alice_d1, alice_d2, bob, alice_id, bob_id) = alice_two_devices_and_bob().await;
    let d2 = identity(&alice_d2).await;

    let dm = bob.create_dm(&[bob_id.as_str(), alice_id.as_str()]).await;
    alice_d1.accept_dm_request(&dm).await;
    bob.fetch_dm_messages(&dm).await;
    assert!(tree_has(&bob, &dm, &d2).await, "precondition: alice_d2 was added");

    // A garbage blob, as with a KeyPackage key lost in a wiped local DB.
    let log = writable_log().await.conn().await.expect("log conn");
    let n = log
        .execute(
            "UPDATE mls_welcome SET welcome_data = X'00' \
             WHERE conversation_id = ?1 AND recipient_device_id = ?2",
            libsql::params![dm.clone(), d2.1.clone()],
        )
        .await
        .expect("spoil the welcome");
    assert_eq!(n, 1, "precondition: one Welcome for alice_d2");

    bob.send_channel_message(&dm, "sealed for the unusable leaf").await;

    alice_d2.poll().await;
    assert!(
        holds_group(&alice_d2, &dm).await,
        "the drain that failed to apply the Welcome must re-join the DM itself"
    );
    let mut rows = log
        .query(
            "SELECT delivered FROM mls_welcome WHERE conversation_id = ?1 AND recipient_device_id = ?2",
            libsql::params![dm.clone(), d2.1.clone()],
        )
        .await
        .expect("welcome row");
    let delivered: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(delivered, 1, "the failed Welcome is acknowledged, so it is not retried");

    let (bodies, placeholders) = open_dm(&alice_d2, &dm).await;
    assert!(placeholders >= 1, "the message it could not read is shown, got {bodies:?}");

    bob.send_channel_message(&dm, "after the re-join").await;
    let (bodies, _) = open_dm(&alice_d2, &dm).await;
    assert!(bodies.contains(&"after the re-join".to_string()), "got {bodies:?}");

    alice_d2.poll().await;

    drop((alice_d1, alice_d2, bob));
}

/// The creator's reconcile loses epoch 0 to the recipient's external join, then retries.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_creator_whose_reconcile_loses_epoch_zero_still_adds_the_other_devices() {
    use pollis_api::DsRequest;
    wipe().await;
    let (alice_d1, alice_d2, bob, alice_id, bob_id) = alice_two_devices_and_bob().await;
    let d2 = identity(&alice_d2).await;

    let dm = ulid::Ulid::new().to_string();
    let body = pollis_api::profile::CreateDmBody {
        id: dm.clone(),
        creator_id: bob_id.clone(),
        member_ids: vec![bob_id.clone(), alice_id.clone()],
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let status = crate::harness::signed_post_status(
        &bob,
        <pollis_api::profile::CreateDmBody as DsRequest>::PATH,
        &serde_json::to_vec(&body).expect("serialize CreateDmBody"),
    )
    .await;
    assert_eq!(status, 200, "create_dm rows");
    pollis_lib::commands::mls::init_mls_group(&bob.state, &dm, &bob_id)
        .await
        .expect("init_mls_group");

    // alice_d2 has nothing published, so only the creator's retry can add it.
    drop_all_packages(&d2).await;
    alice_d1.fetch_dm_messages(&dm).await;
    assert!(holds_group(&alice_d1, &dm).await, "precondition: alice_d1 external-joined");
    assert_eq!(crate::harness::ds_head_epoch(&dm).await, 1, "precondition: epoch 0 is taken");
    crate::harness::republish_key_packages(&alice_d2).await;

    pollis_lib::commands::mls::reconcile_group_mls_impl(&bob.state, &dm, &bob_id)
        .await
        .expect("reconcile");
    assert!(
        tree_has(&bob, &dm, &d2).await,
        "the creator must add alice_d2 on the converged epoch, not give up"
    );

    bob.send_channel_message(&dm, "hello both").await;
    alice_d2.poll().await;
    let (bodies, _) = open_dm(&alice_d2, &dm).await;
    assert!(bodies.contains(&"hello both".to_string()), "got {bodies:?}");

    drop((alice_d1, alice_d2, bob));
}

/// A freshly enrolled device is addable at once (pool + last-resort package).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn enrollment_publishes_a_pool_and_a_last_resort_package_at_once() {
    wipe().await;
    let mut alice_d1 = TestClient::new().await;
    alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let (regular, last_resort, _) = pool(&identity(&alice_d2).await).await;
    assert_eq!(regular, 5);
    assert_eq!(last_resort, 1);
    let (regular, last_resort, _) = pool(&identity(&alice_d1).await).await;
    assert_eq!((regular, last_resort), (5, 1), "and sign-up does the same");
    drop((alice_d1, alice_d2));
}

/// The joining device runs the backstop once, so it adds its own sibling.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_device_that_joins_adds_its_own_users_other_device() {
    use pollis_api::DsRequest;
    wipe().await;
    let (alice_d1, alice_d2, bob, alice_id, bob_id) = alice_two_devices_and_bob().await;
    let d2 = identity(&alice_d2).await;
    assert_eq!(pool(&d2).await.0, 5, "precondition: alice_d2 has a full pool");

    let dm = ulid::Ulid::new().to_string();
    let body = pollis_api::profile::CreateDmBody {
        id: dm.clone(),
        creator_id: bob_id.clone(),
        member_ids: vec![bob_id.clone(), alice_id.clone()],
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let status = crate::harness::signed_post_status(
        &bob,
        <pollis_api::profile::CreateDmBody as DsRequest>::PATH,
        &serde_json::to_vec(&body).expect("serialize CreateDmBody"),
    )
    .await;
    assert_eq!(status, 200, "create_dm rows");
    pollis_lib::commands::mls::init_mls_group(&bob.state, &dm, &bob_id)
        .await
        .expect("init_mls_group");

    alice_d1.fetch_dm_messages(&dm).await;
    assert!(holds_group(&alice_d1, &dm).await, "precondition: alice_d1 joined");
    assert!(
        tree_has(&alice_d1, &dm, &d2).await,
        "the joining device must add its own user's other device"
    );

    bob.fetch_dm_messages(&dm).await;
    bob.send_channel_message(&dm, "from bob").await;
    alice_d1.fetch_dm_messages(&dm).await;
    alice_d1.send_channel_message(&dm, "from alice_d1").await;

    alice_d2.poll().await;
    let (bodies, placeholders) = open_dm(&alice_d2, &dm).await;
    assert!(bodies.contains(&"from bob".to_string()), "got {bodies:?}");
    assert!(bodies.contains(&"from alice_d1".to_string()), "got {bodies:?}");
    assert_eq!(placeholders, 0);

    drop((alice_d1, alice_d2, bob));
}

/// The sweep and mobile's backstop re-add the member's OWN other device too.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_members_sweep_re_adds_its_own_users_other_device() {
    wipe().await;
    let (alice_d1, alice_d2, bob, alice_id, bob_id) = alice_two_devices_and_bob().await;
    let d2 = identity(&alice_d2).await;
    drop_all_packages(&d2).await;

    let dm = bob.create_dm(&[bob_id.as_str(), alice_id.as_str()]).await;
    alice_d1.accept_dm_request(&dm).await;
    alice_d1.fetch_dm_messages(&dm).await;
    assert!(!tree_has(&alice_d1, &dm, &d2).await, "precondition: alice_d2 is outside");

    crate::harness::republish_key_packages(&alice_d2).await;
    assert_eq!(pool(&d2).await.0, 5, "a full pool, outside the tree");
    alice_d1.sweep().await;
    assert!(tree_has(&alice_d1, &dm, &d2).await, "alice_d1's sweep must add alice_d2");

    bob.fetch_dm_messages(&dm).await;
    bob.send_channel_message(&dm, "after the sweep").await;
    alice_d2.poll().await;
    let (bodies, placeholders) = open_dm(&alice_d2, &dm).await;
    assert!(bodies.contains(&"after the sweep".to_string()), "got {bodies:?}");
    assert_eq!(placeholders, 0);

    drop((alice_d1, alice_d2, bob));
}

/// A late first hold, then remove + re-add: the removal gap is not owed (#1524).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_removal_gap_after_a_late_first_join_stays_silent() {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let group_id = alice.create_group("Gap").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    join_group(&alice, &bob, &group_id, &bob_p.username).await;
    alice.send_channel_message(&channel_id, "while bob is in").await;
    bob.fetch_channel_messages(&channel_id).await;

    alice.remove_member(&group_id, &bob_p.id).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    alice.send_channel_message(&channel_id, "while bob is out").await;
    bob.process_commits_for(&channel_id).await;

    join_group(&alice, &bob, &group_id, &bob_p.username).await;
    alice.send_channel_message(&channel_id, "bob is back").await;
    let msgs = bob.fetch_channel_messages(&channel_id).await;
    let bodies: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(bodies.contains(&"bob is back"), "got {bodies:?}");
    drop((alice, bob));
}

async fn join_group(admin: &TestClient, joiner: &TestClient, group_id: &str, username: &str) {
    admin.invite(group_id, username).await;
    let invite = joiner.first_pending_invite().await.expect("invite");
    joiner.accept_invite(invite["id"].as_str().expect("invite id")).await;
    joiner.poll().await;
}
