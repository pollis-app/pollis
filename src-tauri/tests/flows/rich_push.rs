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

    // What the extension showed is alice's text, attributed to her, never bob's own.
    assert!(!shown.is_empty(), "the extension showed something across three rounds");
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
    assert!(!chatter.is_dm);

    let mention = format!("@{} look", bob_p.username);
    alice.send_channel_message(&general, &mention).await;
    let out = headless(&bob, Some(handle_for(&bob_p.id, &general, "channel").await), Duration::from_secs(20)).await;
    let shown = items(&out);
    let pinged = shown.iter().find(|i| i.body == mention).expect("decrypted");
    assert!(!pinged.muted, "a direct mention pierces the mute: {pinged:?}");
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
