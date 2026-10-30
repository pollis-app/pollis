//! Rich push notifications decrypted on the device (#1599), end to end through the real
//! command pipeline and the in-process DS.
//!
//! The "extension" here is `pollis_core::notify::fetch::run` on a second `AppState` built from
//! the same keystore, data dir and config as the app's, exactly what `pollis_notify_fetch`
//! builds in a Notification Service Extension. The two hold separate descriptors on
//! `state.lock`, so they exclude each other the way two processes do.

use std::collections::HashSet;
use std::time::Duration;

use crate::harness::{wipe, world, writable_remote, TestClient, TEST_PIN};
use pollis_core::notify::escrow::{self, test_support, NotificationContent, ESCROW_SLOT};
use pollis_core::notify::fetch::{self, Outcome, Params, Reason};
use pollis_core::notify::lock::StateLock;
use pollis_delivery::push::mint_push_handles;
use serial_test::serial;

/// Escrow is mobile-only; these tests stand in for a phone.
struct MobileBuild;

impl MobileBuild {
    fn on() -> Self {
        test_support::force_escrow_supported_globally(true);
        Self
    }
}

impl Drop for MobileBuild {
    fn drop(&mut self) {
        test_support::force_escrow_supported_globally(false);
    }
}

async fn headless(client: &TestClient, handle: Option<String>, budget: Duration) -> Outcome {
    client.activate();
    fetch::run(Params {
        keystore: client.state.keystore.clone(),
        config: world().await.config.clone(),
        handle,
        budget,
        local_dir: Some(client.state.local_data_dir()),
    })
    .await
}

async fn handle_for(user_id: &str, conversation_id: &str, kind: &str) -> String {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let ids = vec![user_id.to_string()];
    mint_push_handles(&conn, &ids, conversation_id, kind).await[user_id].clone()
}

async fn set_content(client: &TestClient, mode: &str) {
    client.activate();
    escrow::set_notification_content(&client.state, mode)
        .await
        .unwrap_or_else(|e| panic!("set_notification_content({mode}): {e}"));
}

async fn escrowed(client: &TestClient) -> bool {
    client
        .state
        .keystore
        .load_for_user(ESCROW_SLOT, client.user_id())
        .await
        .expect("keystore")
        .is_some()
}

fn items(outcome: &Outcome) -> Vec<fetch::Item> {
    match outcome {
        Outcome::Ok { items, .. } => items.clone(),
        Outcome::Failed(r) => panic!("headless fetch failed: {r:?}"),
    }
}

/// `(id, content)` of every live message in a page.
fn contents(page: &[serde_json::Value]) -> Vec<(String, String)> {
    page.iter()
        .filter(|m| m["undecryptable"] != true)
        .map(|m| {
            (
                m["id"].as_str().unwrap_or_default().to_string(),
                m["content"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The acceptance race: the app (sending AND fetching) and a headless fetch on the same data
/// dir, three rounds. Every message arrives exactly once on both sides, nothing is recorded as
/// lost (the harness asserts that on drop), and both directions keep decrypting afterwards, so
/// neither side's MLS state was rolled back by the other.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_app_and_a_headless_fetch_race_without_losing_or_rolling_back() {
    wipe().await;
    let _mobile = MobileBuild::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    set_content(&bob, "full").await;
    assert!(escrowed(&bob).await, "the setting escrows the key on a phone");

    let dm = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm).await;
    alice.fetch_dm_messages(&dm).await;
    bob.fetch_dm_messages(&dm).await;

    let mut sent_by_alice: Vec<String> = Vec::new();
    let mut sent_by_bob: Vec<String> = Vec::new();
    let mut shown: Vec<fetch::Item> = Vec::new();
    for round in 0..3 {
        for i in 0..6 {
            let text = format!("alice r{round} m{i}");
            alice.send_channel_message(&dm, &text).await;
            sent_by_alice.push(text);
        }
        let handle = handle_for(&bob_p.id, &dm, "dm").await;

        let app = async {
            for j in 0..3 {
                let text = format!("bob r{round} m{j}");
                bob.send_channel_message(&dm, &text).await;
                bob.fetch_dm_messages(&dm).await;
            }
        };
        let extension = headless(&bob, Some(handle), Duration::from_secs(20));
        let ((), outcome) = tokio::join!(app, extension);
        shown.extend(items(&outcome));
        for j in 0..3 {
            sent_by_bob.push(format!("bob r{round} m{j}"));
        }
    }

    // Bob holds every one of alice's messages, once each.
    let bob_view = contents(&bob.fetch_dm_messages(&dm).await);
    for text in &sent_by_alice {
        let n = bob_view.iter().filter(|(_, c)| c == text).count();
        assert_eq!(n, 1, "bob holds {text:?} exactly once: {bob_view:?}");
    }
    let ids: HashSet<&String> = bob_view.iter().map(|(id, _)| id).collect();
    assert_eq!(ids.len(), bob_view.len(), "no duplicate rows");

    // Alice decrypts everything bob sent while the extension was decrypting: bob's sender
    // ratchet was never rolled back under her.
    let alice_view = contents(&alice.fetch_dm_messages(&dm).await);
    for text in &sent_by_bob {
        assert!(
            alice_view.iter().any(|(_, c)| c == text),
            "alice decrypts {text:?}: {alice_view:?}"
        );
    }

    // Both directions still work after the race.
    alice.send_channel_message(&dm, "after the race").await;
    bob.send_channel_message(&dm, "still here").await;
    assert!(contents(&bob.fetch_dm_messages(&dm).await)
        .iter()
        .any(|(_, c)| c == "after the race"));
    assert!(contents(&alice.fetch_dm_messages(&dm).await)
        .iter()
        .any(|(_, c)| c == "still here"));

    // What the extension showed is alice's text, attributed to her, never bob's own, each once.
    assert!(!shown.is_empty(), "the extension showed something across three rounds");
    let ids: HashSet<&String> = shown.iter().map(|i| &i.message_id).collect();
    assert_eq!(ids.len(), shown.len(), "no message previewed twice: {shown:?}");
    for item in &shown {
        assert_eq!(item.conversation_id, dm);
        assert!(item.is_dm);
        assert_eq!(item.sender_name, alice_p.username);
        assert!(item.body.starts_with("alice r"), "{item:?}");
        assert!(!item.muted);
        assert_eq!(item.conversation_title, None);
    }
}

/// Deterministic exclusion: while the app is parked INSIDE an MLS section (its post-join
/// self-update staged, not yet submitted), the extension cannot touch the state; it waits out
/// its share of the budget and reports `lock_busy`. Once the app's section ends, it proceeds.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_extension_cannot_enter_while_the_app_is_mid_mls_step() {
    use pollis_core::commands::mls::rendezvous::{self, Point};
    wipe().await;
    let _mobile = MobileBuild::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    set_content(&bob, "full").await;

    let dm = alice.create_dm(&[&bob_p.id]).await;
    let mut parked = rendezvous::arm(Point::SelfUpdateBeforeSubmit);
    let poll = {
        let state = bob.state.clone();
        let user = bob_p.id.clone();
        bob.activate();
        tokio::spawn(async move { pollis_core::commands::mls::poll_mls_welcomes(&state, user).await })
    };
    let release = tokio::time::timeout(Duration::from_secs(30), parked.recv())
        .await
        .expect("bob never staged his post-join self-update")
        .expect("rendezvous closed");

    alice.send_channel_message(&dm, "sent while bob was mid-commit").await;
    let handle = handle_for(&bob_p.id, &dm, "dm").await;
    let out = headless(&bob, Some(handle.clone()), Duration::from_millis(1500)).await;
    assert_eq!(
        out,
        Outcome::Failed(Reason::LockBusy),
        "the extension must not run while the app holds a staged commit"
    );

    rendezvous::disarm(Point::SelfUpdateBeforeSubmit);
    let _ = release.send(());
    poll.await.expect("poll task").expect("poll_mls_welcomes");

    let out = headless(&bob, Some(handle), Duration::from_secs(20)).await;
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    bob.accept_dm_request(&dm).await;
    assert!(contents(&bob.fetch_dm_messages(&dm).await)
        .iter()
        .any(|(_, c)| c == "sent while bob was mid-commit"));
}

/// #1610: Android's FCM service runs inside the app's process. While the app is unlocked there,
/// a push is fetched on the app's own state, sharing its hold on `state.lock` like any of its
/// ingests. The headless path would take the lock exclusively, so the app's own ingest of the
/// same message (started by the realtime wake-up that comes with the push) waited seconds
/// behind it, and the message reached the open chat that much later.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_in_process_fetch_shares_the_apps_lock_instead_of_excluding_it() {
    wipe().await;
    let _mobile = MobileBuild::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    set_content(&bob, "full").await;
    let dm = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm).await;
    alice.fetch_dm_messages(&dm).await;
    bob.fetch_dm_messages(&dm).await;

    alice.send_channel_message(&dm, "while the app is mid-step").await;
    let handle = handle_for(&bob_p.id, &dm, "dm").await;

    // The app is inside an MLS section: it holds its share of the cross-process lock.
    bob.activate();
    let app_section = bob.state.cross_process_lock().await.expect("the app's share");

    // A headless fetch is locked out until the app's section ends...
    let out = headless(&bob, Some(handle.clone()), Duration::from_millis(1500)).await;
    assert_eq!(out, Outcome::Failed(Reason::LockBusy));

    // ...but the in-process fetch runs alongside it, and stores and shows the message.
    bob.activate();
    let out = fetch::run_in_app(&bob.state, Some(&handle), Duration::from_secs(20))
        .await
        .expect("the app is unlocked in this process");
    let shown = items(&out);
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(shown[0].body, "while the app is mid-step");
    assert_eq!(shown[0].sender_name, alice_p.username);
    drop(app_section);

    // Stored once, and the app reads it from its own store.
    let view = contents(&bob.fetch_dm_messages(&dm).await);
    assert_eq!(
        view.iter().filter(|(_, c)| c == "while the app is mid-step").count(),
        1,
        "{view:?}"
    );

    // A locked app has no state to share: the host falls back to the headless fetch.
    bob.activate();
    pollis_core::commands::pin::lock(&bob.state).await.expect("lock");
    assert!(fetch::run_in_app(&bob.state, Some(&handle), Duration::from_secs(5))
        .await
        .is_none());
}

/// #1610: the in-process fetch and the app's own ingest of the same conversation, truly at the
/// same time (the push and its realtime wake-up arrive together), several rounds with alice
/// sending between them. Every message is stored exactly once, none is a placeholder, nothing is
/// recorded as lost (the harness asserts that on drop), and both directions still decrypt after,
/// so neither path rolled the shared MLS state back under the other.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_in_process_fetch_and_the_apps_ingest_race_on_one_conversation() {
    wipe().await;
    let _mobile = MobileBuild::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    set_content(&bob, "full").await;
    let dm = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm).await;
    alice.fetch_dm_messages(&dm).await;
    bob.fetch_dm_messages(&dm).await;

    let mut sent: Vec<String> = Vec::new();
    for round in 0..4 {
        for i in 0..3 {
            let text = format!("race r{round} m{i}");
            alice.send_channel_message(&dm, &text).await;
            sent.push(text);
        }
        let handle = handle_for(&bob_p.id, &dm, "dm").await;
        bob.activate();
        let app_ingest = bob.invoke_json(
            "ingest_dm_envelopes",
            serde_json::json!({ "userId": bob_p.id, "dmChannelId": dm }),
        );
        let in_app = fetch::run_in_app(&bob.state, Some(&handle), Duration::from_secs(20));
        let (_, outcome) = tokio::join!(app_ingest, in_app);
        let outcome = outcome.expect("the app is unlocked in this process");
        assert!(matches!(outcome, Outcome::Ok { .. }), "round {round}: {outcome:?}");
    }

    let page = bob.fetch_dm_messages(&dm).await;
    let undecryptable = page.iter().filter(|m| m["undecryptable"] == true).count();
    assert_eq!(undecryptable, 0, "a placeholder means an MLS failure: {page:#?}");
    let view = contents(&page);
    for text in &sent {
        let n = view.iter().filter(|(_, c)| c == text).count();
        assert_eq!(n, 1, "bob holds {text:?} exactly once: {view:?}");
    }
    let ids: HashSet<&String> = view.iter().map(|(id, _)| id).collect();
    assert_eq!(ids.len(), view.len(), "no duplicate rows");

    // Both directions still work: no MLS state was rolled back.
    bob.send_channel_message(&dm, "bob after the race").await;
    alice.send_channel_message(&dm, "alice after the race").await;
    assert!(contents(&alice.fetch_dm_messages(&dm).await)
        .iter()
        .any(|(_, c)| c == "bob after the race"));
    assert!(contents(&bob.fetch_dm_messages(&dm).await)
        .iter()
        .any(|(_, c)| c == "alice after the race"));
}

/// The fallback reasons, and the three settings.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn fallbacks_and_modes() {
    wipe().await;
    let _mobile = MobileBuild::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let dm = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm).await;
    alice.fetch_dm_messages(&dm).await;
    bob.fetch_dm_messages(&dm).await;

    // The first unlock on a phone escrows under the default setting (sign-up's set_pin here).
    assert!(escrowed(&bob).await, "the default setting escrows at unlock");
    assert_eq!(
        escrow::get_notification_content(&bob.state).await.unwrap(),
        NotificationContent::Full
    );

    // Without an escrow (an install not yet unlocked on this build): nothing to decrypt with.
    escrow::delete_escrow(bob.state.keystore.as_ref(), &bob_p.id).await.unwrap();
    let none = headless(&bob, None, Duration::from_secs(5)).await;
    assert_eq!(
        none,
        Outcome::Ok { mode: NotificationContent::None, items: vec![] },
        "no escrow yet"
    );
    set_content(&bob, "full").await;

    // not_logged_in: no active account.
    pollis_core::accounts::clear_last_active_user().expect("clear active user");
    let out = fetch::run(Params {
        keystore: bob.state.keystore.clone(),
        config: world().await.config.clone(),
        handle: None,
        budget: Duration::from_secs(5),
        local_dir: Some(bob.state.local_data_dir()),
    })
    .await;
    assert_eq!(out, Outcome::Failed(Reason::NotLoggedIn));

    // lock_busy: the app is mid-step for longer than the extension may wait.
    alice.send_channel_message(&dm, "while locked").await;
    let handle = handle_for(&bob_p.id, &dm, "dm").await;
    {
        let _app_holds = StateLock::try_acquire_in(&bob.state.local_data_dir())
            .expect("lock file")
            .expect("free");
        let out = headless(&bob, Some(handle.clone()), Duration::from_millis(300)).await;
        assert_eq!(out, Outcome::Failed(Reason::LockBusy));
    }

    // timeout: a budget too small for any network round trip, and nothing new stored.
    let out = headless(&bob, Some(handle.clone()), Duration::from_millis(1)).await;
    assert_eq!(out, Outcome::Failed(Reason::Timeout));

    // decrypt (a stale escrow): this harness links a plain sqlite, so a wrong key cannot fail
    // here; `a_wrong_key_is_refused_and_never_wipes` in pollis-core covers it on SQLCipher.

    // full: the message, with its sender.
    let out = headless(&bob, Some(handle), Duration::from_secs(20)).await;
    let shown = items(&out);
    assert!(matches!(out, Outcome::Ok { mode: NotificationContent::Full, .. }));
    let item = shown.iter().find(|i| i.body == "while locked").expect("the message is shown");
    assert_eq!(item.sender_name, alice_p.username);
    // ...and it is already in the app's history, before the app ran.
    assert!(contents(&bob.fetch_dm_messages(&dm).await)
        .iter()
        .any(|(_, c)| c == "while locked"));

    // name: sender only.
    set_content(&bob, "name").await;
    alice.send_channel_message(&dm, "name only please").await;
    let out = headless(&bob, Some(handle_for(&bob_p.id, &dm, "dm").await), Duration::from_secs(20)).await;
    assert!(matches!(out, Outcome::Ok { mode: NotificationContent::Name, .. }));
    let shown = items(&out);
    assert!(!shown.is_empty());
    for i in &shown {
        assert_eq!(i.body, "", "name-only never carries the text");
        assert_eq!(i.body_key, None);
        assert_eq!(i.sender_name, alice_p.username);
    }

    // none: the escrow is gone and nothing is decrypted.
    set_content(&bob, "none").await;
    assert!(!escrowed(&bob).await, "Nothing deletes the escrow");
    alice.send_channel_message(&dm, "nothing to see").await;
    let out = headless(&bob, Some(handle_for(&bob_p.id, &dm, "dm").await), Duration::from_secs(5)).await;
    assert_eq!(out, Outcome::Ok { mode: NotificationContent::None, items: vec![] });
    assert_eq!(
        escrow::get_notification_content(&bob.state).await.unwrap(),
        NotificationContent::None
    );

    // "Nothing" with an escrow that outlived it (a failed delete): the extension still fetches
    // and decrypts nothing, and drops the escrow itself.
    bob.state
        .keystore
        .store_for_user(ESCROW_SLOT, &bob_p.id, &good_key(&bob).await)
        .await
        .unwrap();
    alice.send_channel_message(&dm, "still nothing").await;
    let out = headless(&bob, Some(handle_for(&bob_p.id, &dm, "dm").await), Duration::from_secs(20)).await;
    assert_eq!(out, Outcome::Ok { mode: NotificationContent::None, items: vec![] });
    assert!(!escrowed(&bob).await, "the extension deletes a stale escrow under Nothing");
    assert!(
        !stored_contents(&bob).await.iter().any(|c| c == "still nothing"),
        "Nothing never decrypts in the extension"
    );
}

/// The DB key bob's state holds while unlocked, to plant a stale escrow with.
async fn good_key(client: &TestClient) -> Vec<u8> {
    client
        .state
        .unlock
        .lock()
        .await
        .as_ref()
        .expect("unlocked")
        .db_key
        .to_vec()
}

/// What bob's local database holds, read directly (no ingest).
async fn stored_contents(client: &TestClient) -> Vec<String> {
    let guard = client.state.local_db.lock().await;
    let db = guard.as_ref().expect("db open");
    let mut stmt = db
        .conn()
        .prepare("SELECT content FROM message WHERE content IS NOT NULL")
        .unwrap();
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    rows
}

/// A message is previewed by one fetch only: a second push for the same conversation shows
/// nothing new, and a message the app already ingested is offered once, then never again.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_message_is_never_previewed_twice() {
    wipe().await;
    let _mobile = MobileBuild::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let dm = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm).await;
    alice.fetch_dm_messages(&dm).await;
    bob.fetch_dm_messages(&dm).await;

    alice.send_channel_message(&dm, "first").await;
    let out = headless(&bob, Some(handle_for(&bob_p.id, &dm, "dm").await), Duration::from_secs(20)).await;
    let bodies: Vec<String> = items(&out).into_iter().map(|i| i.body).collect();
    assert_eq!(bodies, vec!["first".to_string()]);
    let again = headless(&bob, Some(handle_for(&bob_p.id, &dm, "dm").await), Duration::from_secs(20)).await;
    assert!(items(&again).is_empty(), "the same message is not shown twice: {again:?}");

    // The app ingests first (foreground, or a sibling fetch): offered once as the fallback.
    alice.send_channel_message(&dm, "second").await;
    bob.fetch_dm_messages(&dm).await;
    let out = headless(&bob, Some(handle_for(&bob_p.id, &dm, "dm").await), Duration::from_secs(20)).await;
    let bodies: Vec<String> = items(&out).into_iter().map(|i| i.body).collect();
    assert_eq!(bodies, vec!["second".to_string()]);
    let again = headless(&bob, Some(handle_for(&bob_p.id, &dm, "dm").await), Duration::from_secs(20)).await;
    assert!(items(&again).is_empty(), "the fallback is offered once: {again:?}");
}

/// Creating and forgetting a group are MLS mutations too: they wait for the extension.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_creation_and_deletion_wait_for_the_extension() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let dm = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm).await;
    bob.fetch_dm_messages(&dm).await;

    for step in ["init", "forget"] {
        let extension = StateLock::try_acquire_in(&bob.state.local_data_dir())
            .expect("lock file")
            .expect("free");
        bob.activate();
        let state = bob.state.clone();
        let user = bob_p.id.clone();
        let conv = dm.clone();
        let task = tokio::spawn(async move {
            if step == "init" {
                let _ = pollis_core::commands::mls::init_mls_group(&state, "01JTRICHPUSHINITGROUP00000", &user).await;
            } else {
                let _ = pollis_core::commands::mls::forget_local_mls_group(&state, &conv).await;
            }
        });
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!task.is_finished(), "{step} must wait while the extension holds the lock");
        drop(extension);
        tokio::time::timeout(Duration::from_secs(30), task)
            .await
            .expect("proceeds once the extension lets go")
            .expect("task");
    }
    // The forgotten DM's history placeholders are not this test's concern.
    crate::harness::expect_losses();
}

/// A muted channel's message is marked muted; a direct @mention pierces the mute, as the DS's
/// wake-up rule does.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_muted_channel_is_marked_unless_it_mentions_you() {
    wipe().await;
    let _mobile = MobileBuild::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    set_content(&bob, "full").await;

    let group_id = alice.create_group("Rich Push").await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite = bob.first_pending_invite().await.expect("pending invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    alice.poll().await;
    let general = alice.general_channel_id(&group_id).await;
    bob.ingest_channel(&general).await;

    bob.invoke_json(
        "set_channel_muted",
        serde_json::json!({ "userId": bob_p.id, "channelId": general, "muted": true }),
    )
    .await;

    alice.send_channel_message(&general, "ordinary chatter").await;
    let out = headless(&bob, Some(handle_for(&bob_p.id, &general, "channel").await), Duration::from_secs(20)).await;
    let shown = items(&out);
    let chatter = shown.iter().find(|i| i.body == "ordinary chatter").expect("decrypted");
    assert!(chatter.muted, "a muted channel's message is muted: {chatter:?}");
    assert!(!chatter.mention);
    assert!(!chatter.is_dm);

    // `@all` alone does not pierce a mute (the DS does not wake a muted member for it either).
    alice.send_channel_message(&general, "@all standup").await;
    let out = headless(&bob, Some(handle_for(&bob_p.id, &general, "channel").await), Duration::from_secs(20)).await;
    let everyone = items(&out).into_iter().find(|i| i.body == "@all standup").expect("decrypted");
    assert!(everyone.muted && !everyone.mention, "{everyone:?}");

    let mention = format!("@{} look", bob_p.username);
    alice.send_channel_message(&general, &mention).await;
    let out = headless(&bob, Some(handle_for(&bob_p.id, &general, "channel").await), Duration::from_secs(20)).await;
    let shown = items(&out);
    let pinged = shown.iter().find(|i| i.body == mention).expect("decrypted");
    assert!(!pinged.muted, "a direct mention pierces the mute: {pinged:?}");
    assert!(pinged.mention, "and is flagged as a mention: {pinged:?}");

    // The same message's push really was sent to bob despite the mute: the two rules agree.
    let pushed = crate::harness::sent_push_audience(&pinged.message_id).expect("send reached the DS");
    let audience = pollis_delivery::push::PushAudience::from_send(pushed.push_to, pushed.push_mentions)
        .expect("a channel send asks for a push");
    assert!(audience.mentioned.contains(&bob_p.id));
}

/// Every path that must drop the escrow does: logout, and the PIN-lockout nuke. ("Nothing" is
/// covered above; the wipe by `a_wipe_deletes_the_escrow` in pollis-core.)
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn logout_and_a_pin_lockout_delete_the_escrow() {
    wipe().await;
    let _mobile = MobileBuild::on();

    let mut bob = TestClient::new().await;
    bob.sign_up("bob@test.local").await;
    set_content(&bob, "full").await;
    assert!(escrowed(&bob).await);
    let bob_id = bob.user_id().to_string();
    bob.invoke_json("logout", serde_json::json!({ "deleteData": false })).await;
    assert!(
        bob.state.keystore.load_for_user(ESCROW_SLOT, &bob_id).await.unwrap().is_none(),
        "logout drops the escrow"
    );

    let mut carol = TestClient::new().await;
    carol.sign_up("carol@test.local").await;
    set_content(&carol, "full").await;
    assert!(escrowed(&carol).await);
    carol.invoke_json("lock", serde_json::json!({})).await;
    let wrong = if TEST_PIN == "1111" { "2222" } else { "1111" };
    for _ in 0..pollis_core::commands::pin::MAX_FAILED_ATTEMPTS {
        let _ = carol
            .invoke_try("unlock", serde_json::json!({ "userId": carol.user_id(), "pin": wrong }))
            .await;
    }
    assert!(!escrowed(&carol).await, "a PIN lockout drops the escrow with the wrapped keys");
}

/// #1605: a safety-number change found by the headless fetch reaches the user. The extension
/// re-pins the peer's changed key (clearing `verified`) and records the durable notice; when the
/// app opens the DM afterwards, its own ingest sees the key already pinned, so there is exactly
/// one notice, not one per process.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_key_change_seen_headless_shows_once_in_the_app() {
    use serde_json::json;
    wipe().await;
    let _mobile = MobileBuild::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    set_content(&bob, "full").await;

    let dm = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm).await;
    alice.send_channel_message(&dm, "before the change").await;
    bob.fetch_dm_messages(&dm).await;

    let ask = json!({ "peerUserIds": [alice_p.id] });
    let notices = bob.invoke_json("list_key_change_notices", ask.clone()).await;
    assert_eq!(
        notices.as_array().map(|a| a.len()),
        Some(0),
        "first sight of a key is a pin, not a change: {notices}"
    );
    bob.invoke_json(
        "set_contact_verified",
        json!({ "peerUserId": alice_p.id, "verified": true }),
    )
    .await;

    // The server swaps alice's account key.
    let conn = world().await.remote.conn().await.expect("remote conn");
    conn.execute(
        "UPDATE users SET account_id_pub = ?1, identity_version = identity_version + 1 WHERE id = ?2",
        libsql::params![vec![9u8; 32], alice_p.id.clone()],
    )
    .await
    .expect("swap alice account_id_pub");

    // A push arrives; the extension's ingest re-pins with no live listener at all.
    let out = headless(&bob, Some(handle_for(&bob_p.id, &dm, "dm").await), Duration::from_secs(20)).await;
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");

    // The app opens: the notice is there, and verified is cleared.
    let found = bob.invoke_json("list_key_change_notices", ask.clone()).await;
    let found = found.as_array().cloned().expect("array");
    assert_eq!(found.len(), 1, "the headless re-pin left one notice: {found:?}");
    assert_eq!(found[0]["peer_user_id"], alice_p.id);
    let listed = bob.invoke_json("list_peer_verifications", json!({})).await;
    let entry = listed
        .as_array()
        .expect("array")
        .iter()
        .find(|e| e["peer_user_id"] == alice_p.id)
        .cloned()
        .expect("alice is pinned");
    assert_eq!(entry["verified"], false, "the change cleared verified");

    // The app's own ingest of the same DM sees the key already pinned: still one notice.
    bob.fetch_dm_messages(&dm).await;
    bob.fetch_dm_messages(&dm).await;
    let again = bob.invoke_json("list_key_change_notices", ask).await;
    let again = again.as_array().cloned().expect("array");
    assert_eq!(again.len(), 1, "one change, one notice: {again:?}");
    assert_eq!(again[0]["id"], found[0]["id"]);
}
