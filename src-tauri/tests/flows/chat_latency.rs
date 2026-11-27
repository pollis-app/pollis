//! DS requests per chat action (#2141): send, edit, delete, pin, list pins and react, each in a
//! warm DM and a warm channel.
//!
//! Two counts per action. `waited` is what the command waits on: the DS requests made in its
//! own task (`pollis_core::perf::counted`), so its critical path. `all` is every request this
//! device made at the in-process DS while the command ran, background work included (the
//! watermark, the applied-epoch report, room wake-ups), printed for the record. Each run prints
//! `[perf]` lines; the asserts keep the critical path from growing back.

use std::sync::Arc;

use crate::harness::{ds_request_mark, ds_requests_since, wipe, TestClient};
use pollis_core::perf::counted;
use pollis_core::state::AppState;
use serde_json::json;
use serial_test::serial;

async fn device_of(client: &TestClient) -> String {
    client.state.device_id.lock().await.clone().expect("device id")
}

/// Run `action` as `client`, returning its result, the requests it waited on and every path
/// the device asked for meanwhile.
async fn measured<F, Fut, T>(client: &TestClient, label: &str, action: F) -> (T, u32, Vec<String>)
where
    F: FnOnce(Arc<AppState>) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    client.activate();
    let device = device_of(client).await;
    let mark = ds_request_mark();
    let (out, waited) = counted(action(Arc::clone(&client.state))).await;
    let paths = ds_requests_since(mark, &device);
    println!("[perf] {label}: waited on {waited} DS requests; {} in all {paths:?}", paths.len());
    (out, waited, paths)
}

/// A DM between alice and bob and a channel they share, each with one message already sent
/// both ways, so every group is warm: what follows is the steady state.
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
    // One send more each, so the conversations are where a steady state leaves them.
    alice.send_channel_message(&channel, "steady").await;
    alice.send_channel_message(&dm, "steady").await;
    alice.state.settle_background().await;
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

/// Every action's critical-path DS request count in one conversation.
#[derive(Debug, Default)]
struct Counts {
    send: u32,
    send_mention: u32,
    edit: u32,
    delete: u32,
    pin: u32,
    list_pins: u32,
    react: u32,
}

async fn measure(alice: &TestClient, bob: &TestClient, conversation: &str, kind: &str) -> Counts {
    use pollis_core::commands::{messages, pinned_messages};
    let me = alice.user_id().to_string();
    let conv = conversation.to_string();
    let mut counts = Counts::default();

    let (sent, n, _) = measured(alice, &format!("{kind} send"), |s| {
        let (conv, me) = (conv.clone(), me.clone());
        async move { messages::send_message(conv, me, "plain chatter".into(), None, None, None, &s).await }
    })
    .await;
    let id = sent.expect("send").id;
    counts.send = n;
    alice.state.settle_background().await;

    let mention = format!("hello @{}", bob.profile.as_ref().expect("bob").username);
    let (_, n, _) = measured(alice, &format!("{kind} send @mention"), |s| {
        let (conv, me, text) = (conv.clone(), me.clone(), mention.clone());
        async move { messages::send_message(conv, me, text, None, None, None, &s).await.expect("send") }
    })
    .await;
    counts.send_mention = n;
    alice.state.settle_background().await;

    let (_, n, _) = measured(alice, &format!("{kind} edit"), |s| {
        let (conv, me, id) = (conv.clone(), me.clone(), id.clone());
        async move {
            messages::edit_message(conv, id, me, "plain chatter, edited".into(), &s).await.expect("edit")
        }
    })
    .await;
    counts.edit = n;
    alice.state.settle_background().await;

    let (_, n, _) = measured(alice, &format!("{kind} pin"), |s| {
        let (conv, me, id) = (conv.clone(), me.clone(), id.clone());
        async move { pinned_messages::pin_message(conv, id, me, &s).await.expect("pin") }
    })
    .await;
    counts.pin = n;

    let (pins, n, _) = measured(alice, &format!("{kind} list pins"), |s| {
        let (conv, me) = (conv.clone(), me.clone());
        async move { pinned_messages::list_pinned_messages(conv, me, &s).await.expect("list pins") }
    })
    .await;
    counts.list_pins = n;
    assert!(pins.iter().any(|p| p.message_id == id), "the pin is listed");

    let (_, n, _) = measured(alice, &format!("{kind} react"), |s| {
        let (me, id) = (me.clone(), id.clone());
        async move { messages::add_reaction(id, me, "👍".into(), &s).await.expect("react") }
    })
    .await;
    counts.react = n;

    let doomed = alice.send_channel_message_id(conversation, "to be deleted").await;
    alice.state.settle_background().await;
    let (_, n, _) = measured(alice, &format!("{kind} delete"), |s| {
        let me = me.clone();
        async move { messages::delete_message(doomed, me, &s).await.expect("delete") }
    })
    .await;
    counts.delete = n;
    alice.state.settle_background().await;

    // Bob still gets all of it, in its final form.
    let seen = if kind == "dm" {
        bob.fetch_dm_messages(conversation).await
    } else {
        bob.fetch_channel_messages(conversation).await
    };
    let texts: Vec<&str> = seen.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(texts.contains(&"plain chatter, edited"), "{texts:?}");
    assert!(texts.contains(&mention.as_str()), "{texts:?}");
    assert!(!texts.contains(&"to be deleted"), "{texts:?}");

    println!("[perf] {kind} waited on: {counts:?}");
    counts
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_requests_per_chat_action() {
    let (alice, bob, dm, channel) = warm_world().await;
    let dm = measure(&alice, &bob, &dm, "dm").await;
    let channel = measure(&alice, &bob, &channel, "channel").await;

    // A steady-state send waits on (catch-up ∥ Welcome poll), the post, and in a DM the block
    // check between them: 2 round trips in a channel, 3 in a DM (it was 9 requests each).
    assert!(channel.send <= 3, "channel send: {channel:?}");
    assert!(dm.send <= 4, "dm send: {dm:?}");
    // The roster is read only for a text that names someone.
    assert_eq!(channel.send_mention, channel.send + 1, "{channel:?}");
    // An edit: (catch-up ∥ Welcome poll), the post.
    assert!(channel.edit <= 3 && dm.edit <= 3, "edit: {dm:?} {channel:?}");
    // A delete: (catch-up ∥ Welcome poll), the redaction, the envelope removal; no second resolve.
    assert!(channel.delete <= 4 && dm.delete <= 4, "delete: {dm:?} {channel:?}");
    // Pins name their group from the memo: the DS call alone.
    assert_eq!((dm.pin, channel.pin), (1, 1));
    assert_eq!((dm.list_pins, channel.list_pins), (1, 1));
    assert_eq!((dm.react, channel.react), (1, 1));
}
