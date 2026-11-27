//! DS requests per chat action (#2141): send, edit, delete, pin, list pins and react, each
//! in a warm DM and a warm channel, counted at the in-process DS.
//!
//! The count is what a command waits on: the requests this device made between the call
//! and its return. Work the command hands to a background task is not counted, and is
//! checked separately to still happen. Each run prints `[perf]` lines, so a change that
//! saves or adds a round trip shows in the test output too.

use crate::harness::{ds_request_mark, ds_requests_since, wipe, TestClient};
use serde_json::json;
use serial_test::serial;

async fn device_of(client: &TestClient) -> String {
    client.state.device_id.lock().await.clone().expect("device id")
}

/// The DS paths `client` requested while `action` ran.
async fn requests_of<F, Fut, T>(client: &TestClient, label: &str, action: F) -> (T, Vec<String>)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let device = device_of(client).await;
    let mark = ds_request_mark();
    let out = action().await;
    let paths = ds_requests_since(mark, &device);
    println!("[perf] {label}: {} DS requests {paths:?}", paths.len());
    (out, paths)
}

/// A DM between alice and bob and a channel they share, each with one message already
/// sent both ways, so every group is warm: what follows is the steady state.
async fn warm_world() -> (TestClient, TestClient, String, String) {
    wipe().await;
    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("latency-alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("latency-bob@test.local").await;

    let dm = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm).await;
    alice.send_channel_message(&dm, "warm dm").await;
    bob.fetch_dm_messages(&dm).await;
    bob.send_channel_message(&dm, "warm dm back").await;
    alice.fetch_dm_messages(&dm).await;

    let group_id = alice.create_group("Latency").await;
    let channel = alice.general_channel_id(&group_id).await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite = bob.first_pending_invite().await.expect("bob has an invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    alice.process_commits_for(&channel).await;
    alice.send_channel_message(&channel, "warm channel").await;
    bob.ingest_channel(&channel).await;
    bob.send_channel_message(&channel, "warm channel back").await;
    alice.ingest_channel(&channel).await;
    // The first pin of a conversation mints its pin key; do it here, outside the counts.
    let first = alice.send_channel_message_id(&channel, "first pin").await;
    pin(&alice, &channel, &first).await;
    let first_dm = alice.send_channel_message_id(&dm, "first dm pin").await;
    pin(&alice, &dm, &first_dm).await;
    (alice, bob, dm, channel)
}

async fn pin(client: &TestClient, conversation: &str, message_id: &str) {
    client
        .invoke_json(
            "pin_message",
            json!({ "conversationId": conversation, "messageId": message_id, "userId": client.user_id() }),
        )
        .await;
}

/// Every action's DS request count in one conversation.
#[derive(Debug, Default)]
struct Counts {
    send: usize,
    send_mention: usize,
    edit: usize,
    delete: usize,
    pin: usize,
    list_pins: usize,
    react: usize,
}

async fn measure(alice: &TestClient, bob: &TestClient, conversation: &str, kind: &str) -> Counts {
    let mut counts = Counts::default();

    let (id, paths) = requests_of(alice, &format!("{kind} send"), || {
        alice.send_channel_message_id(conversation, "plain chatter")
    })
    .await;
    counts.send = paths.len();

    let mention = format!("hello @{}", bob.profile.as_ref().expect("bob").username);
    let (_, paths) = requests_of(alice, &format!("{kind} send @mention"), || {
        alice.send_channel_message_id(conversation, &mention)
    })
    .await;
    counts.send_mention = paths.len();

    let (_, paths) = requests_of(alice, &format!("{kind} edit"), || {
        alice.edit_message(conversation, &id, "plain chatter, edited")
    })
    .await;
    counts.edit = paths.len();

    let (_, paths) = requests_of(alice, &format!("{kind} pin"), || pin(alice, conversation, &id)).await;
    counts.pin = paths.len();

    let (pins, paths) = requests_of(alice, &format!("{kind} list pins"), || {
        alice.invoke_json(
            "list_pinned_messages",
            json!({ "conversationId": conversation, "userId": alice.user_id() }),
        )
    })
    .await;
    counts.list_pins = paths.len();
    assert!(
        pins.as_array().expect("pins").iter().any(|p| p["message_id"] == id.as_str()),
        "the pin is listed: {pins:?}"
    );

    let (_, paths) = requests_of(alice, &format!("{kind} react"), || {
        alice.invoke_json(
            "add_reaction",
            json!({ "messageId": id, "userId": alice.user_id(), "emoji": "👍" }),
        )
    })
    .await;
    counts.react = paths.len();

    let doomed = alice.send_channel_message_id(conversation, "to be deleted").await;
    let (_, paths) = requests_of(alice, &format!("{kind} delete"), || alice.delete_message(&doomed)).await;
    counts.delete = paths.len();

    // Bob still gets all of it.
    let seen = if kind == "dm" {
        bob.fetch_dm_messages(conversation).await
    } else {
        bob.fetch_channel_messages(conversation).await
    };
    let texts: Vec<&str> = seen.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(texts.contains(&"plain chatter, edited"), "{texts:?}");
    assert!(texts.contains(&mention.as_str()), "{texts:?}");
    assert!(!texts.contains(&"to be deleted"), "{texts:?}");

    println!("[perf] {kind} totals: {counts:?}");
    counts
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_requests_per_chat_action() {
    let (alice, bob, dm, channel) = warm_world().await;
    let _dm = measure(&alice, &bob, &dm, "dm").await;
    let _channel = measure(&alice, &bob, &channel, "channel").await;
}
