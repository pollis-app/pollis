//! #1571: a device ends up outside a DM's MLS tree, and the DM is empty on it
//! with no error anywhere.
//!
//! The incident: a user's phone, enrolled minutes before a peer opened a DM
//! with them, never got a leaf. The creator's create-time reconcile lost epoch
//! 0 to the recipient's own external join and added nobody else; no device on
//! the (all-mobile) roster ever retried the add; and when the phone finally
//! opened the DM it joined at the head, and the ingest passed every earlier
//! message as "sent before this device joined" — the silent pass a NEW device
//! gets. Each test below pins one layer of the fix:
//!
//! * a device whose regular pool is empty is still added, through its
//!   last-resort KeyPackage — which is never consumed;
//! * a device with nothing to claim at all (a build without last-resort
//!   packages) is NAMED by reconcile, and its late join shows what it missed;
//! * a device left out is re-added with no desktop sweep: by a sender, before
//!   it seals, and by mobile's landing/resume backstop;
//! * a Welcome that fails to apply is acknowledged AND the device re-joins at
//!   once, and what it missed is shown;
//! * a creator whose reconcile loses its epoch race retries and adds the rest
//!   of the roster;
//! * enrollment publishes a pool and a last-resort package straight away;
//! * a NEW device's empty start stays silent (loss #2).

use crate::harness::{enroll_second_device, expect_losses, wipe, writable_log, writable_remote, TestClient};
use serial_test::serial;

/// `(user_id, device_id)` of `client`.
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

/// Readable message bodies and the number of placeholders `client` shows for
/// `dm` after opening it.
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

/// Spend every REGULAR package of `who` server-side — the state a pool is in
/// after its device was away for more adds than it holds, or lost claims to
/// commit races. The last-resort package, if any, is left alone.
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

/// Remove every package of `who`, last-resort included — a device on a build
/// that predates last-resort packages, with its pool spent.
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

/// Alice on two devices, bob on one. Returns `(alice_d1, alice_d2, bob, alice_id, bob_id)`.
async fn alice_two_devices_and_bob() -> (TestClient, TestClient, TestClient, String, String) {
    let mut alice_d1 = TestClient::new().await;
    let alice = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    // `user_device.created_at` is second-granular, and a registration in the
    // same second as the membership decides nothing (`join_branch::
    // ds_instant_window`). In the incident the phone was enrolled eleven
    // minutes before the DM; one second is enough to say "before".
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    (alice_d1, alice_d2, bob, alice.id, bob_profile.id)
}

/// The issue's first scenario, on a current build: alice_d2's regular pool is
/// empty when bob opens the DM. Its last-resort package is claimed instead, so
/// it is in the tree from the start and reads everything — and the package is
/// still there, unclaimed, for the next add.
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

/// A device on a build that predates last-resort packages, pool spent: there
/// is nothing to add it with. Reconcile names it instead of dropping it before
/// the diff, and when the device finally opens the DM and joins at the head,
/// what it missed is shown — it was registered before the DM existed, so this
/// is not the silent "new device" pass.
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

    // ONE open: the pass that joins at the head also shows what was missed.
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

/// Loss #2 stays silent: a device enrolled AFTER the DM and its messages starts
/// empty there, with no placeholder (the zero-loss check on drop fails on any).
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

    // `user_device.created_at` is second-granular: keep the new device's
    // registration unambiguously after the membership.
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

/// The issue's second scenario: a mobile-only roster, so no desktop sweep ever
/// runs. alice_d2 had nothing to claim when bob opened the DM; it then signs in
/// on a current build (a pool and a last-resort package). Bob's next message
/// re-adds it before sealing, and the device reads that message — no sweep, and
/// without opening the DM first.
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

/// Mobile's landing / resume backstop (`reconcile_backstop_all`, bridged for
/// the app): a current member re-adds the missing device with no sweep and no
/// send.
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

    // Steady state: nothing to do, nothing done.
    let again = pollis_core::commands::mls::reconcile_backstop_all(&alice_d1.state, &alice_id)
        .await
        .expect("backstop");
    assert_eq!(again.reconciled, 0, "got {again:?}");

    drop((alice_d1, alice_d2, bob));
}

/// The issue's third scenario: alice_d2 is added, but its Welcome cannot be
/// applied. The drain acknowledges it (an unappliable Welcome must not be
/// retried forever) AND re-joins the DM in the same pass, so the device is a
/// member again at once. What was sealed for its unusable leaf in between is
/// cryptographically gone; it is shown, and everything after is read.
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

    // The Welcome cannot be opened (its blob is garbage — the same outcome as a
    // KeyPackage private key lost with a wiped local DB).
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

    // A second drain finds nothing to retry.
    alice_d2.poll().await;

    drop((alice_d1, alice_d2, bob));
}

/// The incident's ordering (prod metadata): the recipient's device external-
/// joined epoch 0 seconds after the DM was created, BEFORE the creator's
/// reconcile committed. That reconcile lost the race and, before the fix,
/// returned — so no other device was ever added by it, and every one had to
/// join itself. It now retries on the converged epoch and adds the rest.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_creator_whose_reconcile_loses_epoch_zero_still_adds_the_other_devices() {
    use pollis_api::DsRequest;
    wipe().await;
    let (alice_d1, alice_d2, bob, alice_id, bob_id) = alice_two_devices_and_bob().await;
    let d2 = identity(&alice_d2).await;

    // The DM rows and the creator's epoch-0 group; the reconcile half is run
    // by hand below, after the recipient's join.
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

    // alice_d1 sees the request and joins epoch 0 first.
    alice_d1.fetch_dm_messages(&dm).await;
    assert!(holds_group(&alice_d1, &dm).await, "precondition: alice_d1 external-joined");
    assert_eq!(crate::harness::ds_head_epoch(&dm).await, 1, "precondition: epoch 0 is taken");

    // The creator's reconcile, still on epoch 0 locally, loses — and retries.
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

/// Lead (b) from the prod metadata: a freshly enrolled device must be addable
/// the moment enrollment finishes — a full pool and a last-resort package, with
/// nothing else having run on it.
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
