//! The durable outbox (#1335): a message the user saw accepted is either
//! delivered or visibly unsent — never silently gone.
//!
//! The bug: the local row was written only after the slow part of a send
//! (catch-up, seal), and nothing recorded a send in flight, so a process killed
//! mid-send lost the message without a trace. A kill is simulated with
//! `test_hooks::crash_next_send`, which ends the send at a chosen point without
//! marking anything — exactly what a dead process leaves behind — and a restart
//! with `lock` + `unlock`, the path every cold start takes.

use crate::harness::{wipe, writable_remote, TestClient, TEST_PIN};
use pollis_core::commands::messages::test_hooks::{self, Crash};
use serde_json::json;
use serial_test::serial;

/// Alice and Bob in one group; returns (alice, bob, channel_id).
async fn pair() -> (TestClient, TestClient, String) {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("outbox-alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_profile = bob.sign_up("outbox-bob@test.local").await;

    let group_id = alice.create_group("Outbox").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite = bob.first_pending_invite().await.expect("bob has an invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;
    (alice, bob, channel_id)
}

/// Alice's own stored copy of `id`, as the renderer reads it.
async fn own_row(client: &TestClient, channel_id: &str, id: &str) -> Option<serde_json::Value> {
    let page = client
        .invoke_json("read_channel_messages", json!({ "channelId": channel_id, "limit": 50 }))
        .await;
    page["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["id"] == id)
        .cloned()
}

/// How many copies of `content` a member can read.
async fn copies(client: &TestClient, channel_id: &str, content: &str) -> usize {
    client
        .fetch_channel_messages(channel_id)
        .await
        .iter()
        .filter(|m| m["content"].as_str() == Some(content))
        .count()
}

/// How many envelopes the DS holds under `id`.
async fn envelopes_with_id(id: &str) -> i64 {
    let conn = writable_remote().await.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM message_envelope WHERE id = ?1",
            libsql::params![id.to_string()],
        )
        .await
        .expect("count envelopes");
    rows.next().await.expect("row").expect("a row").get(0).expect("count")
}

async fn restart(client: &TestClient) {
    let uid = client.user_id().to_string();
    client.invoke_json("lock", json!({})).await;
    client
        .invoke_json("unlock", json!({ "userId": uid, "pin": TEST_PIN }))
        .await;
}

async fn send_with_id(
    client: &TestClient,
    channel_id: &str,
    id: &str,
    content: &str,
    thread_id: Option<&str>,
) -> Result<serde_json::Value, String> {
    client
        .invoke_try(
            "send_message",
            json!({
                "conversationId": channel_id,
                "senderId": client.user_id(),
                "content": content,
                "replyToId": null,
                "threadId": thread_id,
                "senderUsername": null,
                "messageId": id,
            }),
        )
        .await
}

/// The ticket's scenario: the process dies after the message was accepted but
/// before it reached the DS. It must survive the restart as unsent, and the
/// drain after unlock must deliver it — a thread reply included.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_send_killed_after_the_local_insert_is_delivered_after_unlock() {
    let (alice, bob, channel_id) = pair().await;
    let root = alice.send_channel_message_id(&channel_id, "root").await;

    let plain: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    test_hooks::crash_next_send(Crash::AfterEnqueue);
    let err = send_with_id(&alice, &channel_id, &plain, "sent before the kill", None)
        .await
        .expect_err("the simulated kill ends the send");
    assert!(err.contains("simulated process death"), "{err}");

    let reply: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    test_hooks::crash_next_send(Crash::AfterEnqueue);
    send_with_id(&alice, &channel_id, &reply, "a thread reply before the kill", Some(&root))
        .await
        .expect_err("the simulated kill ends the send");

    // The message survived the kill, stored and visibly unsent; nobody else
    // has it.
    let row = own_row(&alice, &channel_id, &plain).await.expect("the message was stored first");
    assert_eq!(row["content"], "sent before the kill");
    assert_eq!(row["send_state"], "pending");
    assert_eq!(envelopes_with_id(&plain).await, 0);
    assert_eq!(copies(&bob, &channel_id, "sent before the kill").await, 0);

    // Restart: the unlock drains the outbox in the background; an explicit
    // drain waits behind it, so this is deterministic.
    restart(&alice).await;
    alice.invoke_json("flush_outbox", json!({})).await;

    let row = own_row(&alice, &channel_id, &plain).await.expect("still there");
    assert!(row.get("send_state").is_none(), "delivered: {row}");
    assert_eq!(copies(&bob, &channel_id, "sent before the kill").await, 1);
    let replies = bob.read_thread(&root).await;
    assert!(
        replies.iter().any(|m| m["content"] == "a thread reply before the kill"),
        "the thread reply reached its thread: {replies:?}"
    );
    drop((alice, bob));
}

/// The other half of the kill window: the DS took the envelope, then the
/// process died before recording that. The retry re-posts the same id; the DS
/// answers it as a replay, so it is delivered exactly once.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_send_killed_after_the_post_is_not_delivered_twice() {
    let (alice, bob, channel_id) = pair().await;
    let id: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();

    test_hooks::crash_next_send(Crash::AfterPost);
    send_with_id(&alice, &channel_id, &id, "landed, then the kill", None)
        .await
        .expect_err("the simulated kill ends the send");
    assert_eq!(envelopes_with_id(&id).await, 1, "the envelope had landed");
    let row = own_row(&alice, &channel_id, &id).await.expect("stored");
    assert_eq!(row["send_state"], "pending", "this device never recorded the post");

    restart(&alice).await;
    let report = alice.invoke_json("flush_outbox", json!({})).await;
    assert!(report["failed"].as_array().expect("failed").is_empty(), "{report}");

    assert_eq!(envelopes_with_id(&id).await, 1, "the replay stored nothing new");
    assert!(own_row(&alice, &channel_id, &id).await.expect("stored").get("send_state").is_none());
    assert_eq!(copies(&bob, &channel_id, "landed, then the kill").await, 1);
    drop((alice, bob));
}

/// A send that fails is marked failed — not dropped — and Retry delivers it;
/// Discard throws an unsent one away without it ever reaching anyone.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_failed_send_is_kept_as_failed_and_retry_or_discard_settles_it() {
    let (alice, bob, channel_id) = pair().await;
    let outdated = alice.state.update_required.clone();

    // Any failure will do; an outdated client is one the harness can produce.
    outdated.store(true, std::sync::atomic::Ordering::SeqCst);
    let keep: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    send_with_id(&alice, &channel_id, &keep, "retry me", None)
        .await
        .expect_err("an outdated client cannot send");
    let toss: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    send_with_id(&alice, &channel_id, &toss, "discard me", None)
        .await
        .expect_err("an outdated client cannot send");
    assert_eq!(own_row(&alice, &channel_id, &keep).await.expect("kept")["send_state"], "failed");

    // An unsent message has nothing for an edit to target.
    let edit = alice
        .invoke_try(
            "edit_message",
            json!({
                "conversationId": channel_id,
                "messageId": keep,
                "userId": alice.user_id(),
                "newContent": "edited",
            }),
        )
        .await;
    assert!(edit.is_err(), "editing an unsent message must be refused");

    outdated.store(false, std::sync::atomic::Ordering::SeqCst);
    let sent = alice.invoke_json("retry_message", json!({ "messageId": keep })).await;
    assert_eq!(sent["id"], keep.as_str(), "the retry reuses the message id");
    let discarded = alice
        .invoke_json("discard_unsent_message", json!({ "messageId": toss }))
        .await;
    assert_eq!(discarded, true);

    assert!(own_row(&alice, &channel_id, &keep).await.expect("kept").get("send_state").is_none());
    assert!(own_row(&alice, &channel_id, &toss).await.is_none(), "discarded");
    assert_eq!(copies(&bob, &channel_id, "retry me").await, 1);
    assert_eq!(copies(&bob, &channel_id, "discard me").await, 0);
    assert_eq!(envelopes_with_id(&toss).await, 0);

    // A delivered message is not "unsent": discarding it does nothing.
    let not_discarded = alice
        .invoke_json("discard_unsent_message", json!({ "messageId": keep }))
        .await;
    assert_eq!(not_discarded, false);
    drop((alice, bob));
}

/// Clears the upload stub however the test ends, so no other test inherits it.
struct StubbedUploads;

impl StubbedUploads {
    fn on() -> Self {
        test_hooks::stub_attachment_uploads(true);
        StubbedUploads
    }
}

impl Drop for StubbedUploads {
    fn drop(&mut self) {
        test_hooks::stub_attachment_uploads(false);
    }
}

/// The attachment half of the ticket: the row is on screen from the moment the
/// user sends, so a kill during the UPLOAD is the same bug. The outbox takes the
/// bytes with the row, before any network work, and the drain after unlock
/// uploads them, builds the envelope and posts it.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_attachment_send_killed_before_its_upload_is_delivered_after_unlock() {
    let (alice, bob, channel_id) = pair().await;
    let _stub = StubbedUploads::on();

    // A paste: bytes staged in memory, as desktop holds them.
    let bytes = b"\x89PNG not really, but bytes all the same".to_vec();
    let staged = pollis_core::commands::staging::stage_attachment(bytes.clone())
        .expect("stage")
        .id;
    let id: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();

    test_hooks::crash_next_send(Crash::AfterEnqueue);
    alice
        .invoke_try(
            "send_message",
            json!({
                "conversationId": channel_id,
                "senderId": alice.user_id(),
                "content": "the screenshot",
                "replyToId": null,
                "senderUsername": null,
                "messageId": id,
                "attachments": [{
                    "stagedId": staged,
                    "filename": "shot.png",
                    "contentType": "image/png",
                }],
            }),
        )
        .await
        .expect_err("the simulated kill ends the send before the upload");

    // The bytes moved into the outbox; the in-memory copy is gone.
    assert!(pollis_core::commands::staging::peek_staged(&staged).is_none());
    let row = own_row(&alice, &channel_id, &id).await.expect("stored before the upload");
    assert_eq!(row["send_state"], "pending");
    let provisional = row["content"].as_str().expect("content");
    assert!(provisional.contains("\"name\":\"shot.png\""), "{provisional}");
    assert!(!provisional.contains("\"key\""), "nothing uploaded yet: {provisional}");
    assert_eq!(envelopes_with_id(&id).await, 0);

    restart(&alice).await;
    alice.invoke_json("flush_outbox", json!({})).await;

    let row = own_row(&alice, &channel_id, &id).await.expect("still there");
    assert!(row.get("send_state").is_none(), "delivered: {row}");
    let content = row["content"].as_str().expect("content").to_string();
    let envelope: serde_json::Value = serde_json::from_str(&content).expect("an _att envelope");
    let att = &envelope["_att"][0];
    assert_eq!(att["name"], "shot.png");
    assert_eq!(att["size"], bytes.len());
    let hash = att["hash"].as_str().expect("uploaded: has a content hash");
    assert_eq!(att["key"], format!("media/{hash}.enc"));
    assert_eq!(envelope["_txt"], "the screenshot");

    // Bob received exactly the finished envelope, once.
    let got: Vec<_> = bob
        .fetch_channel_messages(&channel_id)
        .await
        .into_iter()
        .filter(|m| m["id"] == id.as_str())
        .collect();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["content"].as_str(), Some(content.as_str()));
    drop((alice, bob));
}
