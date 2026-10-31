//! The durable outbox (#1335): a message the user saw accepted is either
//! delivered or visibly unsent — never silently gone.
//!
//! A kill is `test_hooks::crash_next_send`; a restart is `lock` + `unlock`.

use crate::harness::{wipe, writable_remote, TestClient, TEST_PIN};
use pollis_core::commands::messages::test_hooks::{self, Crash};
use serde_json::json;
use serial_test::serial;

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

async fn copies(client: &TestClient, channel_id: &str, content: &str) -> usize {
    client
        .fetch_channel_messages(channel_id)
        .await
        .iter()
        .filter(|m| m["content"].as_str() == Some(content))
        .count()
}

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

/// Killed after acceptance, before the DS: survives as unsent and the drain delivers it.
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

    let row = own_row(&alice, &channel_id, &plain).await.expect("the message was stored first");
    assert_eq!(row["content"], "sent before the kill");
    assert_eq!(row["send_state"], "pending");
    assert_eq!(envelopes_with_id(&plain).await, 0);
    assert_eq!(copies(&bob, &channel_id, "sent before the kill").await, 0);

    // An explicit drain waits behind the unlock's, so this is deterministic.
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

/// Killed after the DS took it: the retry is a replay, delivered exactly once.
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
    let sent_at = row["sent_at"].clone();

    restart(&alice).await;
    let report = alice.invoke_json("flush_outbox", json!({})).await;
    assert!(report["failed"].as_array().expect("failed").is_empty(), "{report}");

    assert_eq!(envelopes_with_id(&id).await, 1, "the replay stored nothing new");
    let row = own_row(&alice, &channel_id, &id).await.expect("stored");
    assert!(row.get("send_state").is_none());
    // The sender's copy keeps the first post's stamp, as recipients show.
    assert_eq!(row["sent_at"], sent_at, "the retry must not move the message");
    let theirs: Vec<_> = bob
        .fetch_channel_messages(&channel_id)
        .await
        .into_iter()
        .filter(|m| m["id"] == id.as_str())
        .collect();
    assert_eq!(theirs.len(), 1, "delivered exactly once");
    assert_eq!(theirs[0]["sent_at"], sent_at, "sender and recipient agree on when");
    drop((alice, bob));
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_failed_send_is_kept_as_failed_and_retry_or_discard_settles_it() {
    let (alice, bob, channel_id) = pair().await;
    let outdated = alice.state.update_required.clone();

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

    let not_discarded = alice
        .invoke_json("discard_unsent_message", json!({ "messageId": keep }))
        .await;
    assert_eq!(not_discarded, false);
    drop((alice, bob));
}

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

/// A kill during the UPLOAD: the outbox holds the bytes and the drain finishes the send.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_attachment_send_killed_before_its_upload_is_delivered_after_unlock() {
    let (alice, bob, channel_id) = pair().await;
    let _stub = StubbedUploads::on();

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

/// #1568: `sens` lives only inside the ciphertext, and is absent when unmarked.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_sensitive_attachment_reaches_the_receiver_marked() {
    let (alice, bob, channel_id) = pair().await;
    let _stub = StubbedUploads::on();

    let stage = |b: &[u8]| {
        pollis_core::commands::staging::stage_attachment(b.to_vec())
            .expect("stage")
            .id
    };
    let (marked, plain) = (stage(b"spoiler bytes"), stage(b"plain bytes"));
    let id: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    alice
        .invoke_try(
            "send_message",
            json!({
                "conversationId": channel_id,
                "senderId": alice.user_id(),
                "content": "the finale",
                "replyToId": null,
                "senderUsername": null,
                "messageId": id,
                "attachments": [
                    { "stagedId": marked, "filename": "a.png", "contentType": "image/png", "sensitive": true },
                    { "stagedId": plain, "filename": "b.png", "contentType": "image/png" },
                ],
            }),
        )
        .await
        .expect("sent");

    let got: Vec<_> = bob
        .fetch_channel_messages(&channel_id)
        .await
        .into_iter()
        .filter(|m| m["id"] == id.as_str())
        .collect();
    assert_eq!(got.len(), 1);
    let envelope: serde_json::Value =
        serde_json::from_str(got[0]["content"].as_str().expect("content")).expect("an _att envelope");
    assert_eq!(envelope["_att"][0]["sens"], true);
    assert!(envelope["_att"][1].get("sens").is_none(), "{envelope}");
    drop((alice, bob));
}

/// Discarding a message that DID reach the DS withdraws it from everyone.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn discarding_a_message_that_reached_the_ds_withdraws_it_from_everyone() {
    let (alice, bob, channel_id) = pair().await;
    let outdated = alice.state.update_required.clone();
    let id: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();

    test_hooks::crash_next_send(Crash::AfterPost);
    send_with_id(&alice, &channel_id, &id, "landed, then discarded", None)
        .await
        .expect_err("the simulated kill ends the send");
    assert_eq!(copies(&bob, &channel_id, "landed, then discarded").await, 1);

    outdated.store(true, std::sync::atomic::Ordering::SeqCst);
    alice
        .invoke_try("retry_message", json!({ "messageId": id }))
        .await
        .expect_err("an outdated client cannot send");
    outdated.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(own_row(&alice, &channel_id, &id).await.expect("kept")["send_state"], "failed");

    let discarded = alice
        .invoke_json("discard_unsent_message", json!({ "messageId": id }))
        .await;
    assert_eq!(discarded, true);
    assert!(own_row(&alice, &channel_id, &id).await.is_none(), "gone here");
    assert_eq!(envelopes_with_id(&id).await, 0, "gone from the DS");
    assert_eq!(
        copies(&bob, &channel_id, "landed, then discarded").await,
        0,
        "and redacted for the recipient who already had it"
    );
    drop((alice, bob));
}

/// Drains are single-flight, but a trigger arriving mid-drain is never dropped.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_overlapping_drain_is_coalesced_into_a_rerun() {
    use pollis_lib::commands::mls::rendezvous::{self, Point};
    let (alice, bob, channel_id) = pair().await;
    let queue = |text: &'static str| {
        let alice = &alice;
        let channel_id = channel_id.clone();
        async move {
            let id: String = alice.invoke_json("new_message_id", json!({})).await
                .as_str().expect("id").to_string();
            test_hooks::crash_next_send(Crash::AfterEnqueue);
            send_with_id(alice, &channel_id, &id, text, None)
                .await
                .expect_err("the simulated kill ends the send");
            id
        }
    };

    let first = queue("queued first").await;
    let mut before_post = rendezvous::arm(Point::EnvelopeBeforePost);
    let before = test_hooks::delivery_attempts();
    let running = {
        let state = alice.state.clone();
        tokio::spawn(async move { pollis_core::commands::messages::flush_outbox(&state).await })
    };
    let release = tokio::time::timeout(std::time::Duration::from_secs(30), before_post.recv())
        .await
        .expect("the drain never reached its post")
        .expect("rendezvous closed");
    rendezvous::disarm(Point::EnvelopeBeforePost);

    let second = queue("queued mid-drain").await;
    let trigger = {
        let state = alice.state.clone();
        tokio::spawn(async move { pollis_core::commands::messages::flush_outbox(&state).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!trigger.is_finished(), "the trigger waits for the running drain");
    let _ = release.send(());

    running.await.unwrap().expect("drain");
    let report = trigger.await.unwrap().expect("trigger");
    assert_eq!(report.delivered, vec![second.clone()], "the rerun pass took the new message");
    assert_eq!(test_hooks::delivery_attempts(), before + 2, "one attempt each, no overlap");
    for id in [&first, &second] {
        assert!(own_row(&alice, &channel_id, id).await.expect("row").get("send_state").is_none());
    }

    let third = queue("queued while held").await;
    let held = alice.state.outbox_drain.clone().lock_owned().await;
    let waiting = {
        let state = alice.state.clone();
        tokio::spawn(async move { pollis_core::commands::messages::flush_outbox(&state).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!waiting.is_finished(), "it waits rather than overlapping");
    drop(held);
    let report = waiting.await.unwrap().expect("flush");
    assert_eq!(report.delivered, vec![third.clone()], "its trigger was not dropped");

    for text in ["queued first", "queued mid-drain", "queued while held"] {
        assert_eq!(copies(&bob, &channel_id, text).await, 1, "{text}");
    }
    drop((alice, bob));
}

/// A permanent refusal is `rejected`: shown, retryable, never re-attempted by a drain.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_permanent_refusal_is_rejected_and_not_drained_again() {
    let (alice, bob, _) = pair().await;
    let theirs = bob.create_group("Not yours").await;
    let channel_id = bob.general_channel_id(&theirs).await;
    let id: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    let err = send_with_id(&alice, &channel_id, &id, "nobody can take this", None)
        .await
        .expect_err("a non-member cannot send");
    assert_eq!(
        own_row(&alice, &channel_id, &id).await.expect("kept")["send_state"],
        "rejected",
        "refused with: {err}"
    );

    let before = test_hooks::delivery_attempts();
    let report = alice.invoke_json("flush_outbox", json!({})).await;
    assert!(report["failed"].as_array().expect("failed").is_empty(), "{report}");
    assert_eq!(test_hooks::delivery_attempts(), before, "the drain left it alone");

    alice
        .invoke_try("retry_message", json!({ "messageId": id }))
        .await
        .expect_err("still not a member");
    assert_eq!(test_hooks::delivery_attempts(), before + 1);
    assert_eq!(own_row(&alice, &channel_id, &id).await.expect("kept")["send_state"], "rejected");
    drop((alice, bob));
}

/// A `rejected` sealed message can still be discarded.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_sealed_rejected_message_can_still_be_discarded() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let alice_profile = alice.sign_up("outbox-gone-alice@test.local").await;
    let mut bob = TestClient::new().await;
    bob.sign_up("outbox-gone-bob@test.local").await;
    let group_id = bob.create_group("Leaving").await;
    let channel_id = bob.general_channel_id(&group_id).await;
    bob.invite(&group_id, &alice_profile.username).await;
    let invite = alice.first_pending_invite().await.expect("alice has an invite");
    alice.accept_invite(invite["id"].as_str().expect("invite id")).await;
    alice.poll().await;
    bob.process_commits_for(&channel_id).await;

    let id: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    test_hooks::crash_next_send(Crash::AfterPost);
    send_with_id(&alice, &channel_id, &id, "sent, then removed", None)
        .await
        .expect_err("the simulated kill ends the send");

    bob.remove_member(&group_id, alice.user_id()).await;
    alice
        .invoke_try("retry_message", json!({ "messageId": id }))
        .await
        .expect_err("not a member any more");
    assert_eq!(own_row(&alice, &channel_id, &id).await.expect("kept")["send_state"], "rejected");

    let discarded = alice
        .invoke_try("discard_unsent_message", json!({ "messageId": id }))
        .await
        .expect("a discard that can no longer withdraw still removes the row");
    assert_eq!(discarded, true);
    assert!(own_row(&alice, &channel_id, &id).await.is_none());
    drop((alice, bob));
}

/// Until sealed, every attempt refreshes the message's stamp.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_long_queued_message_lands_with_the_time_it_went_out() {
    let (alice, bob, channel_id) = pair().await;
    let id: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    test_hooks::crash_next_send(Crash::AfterEnqueue);
    send_with_id(&alice, &channel_id, &id, "typed offline", None)
        .await
        .expect_err("the simulated kill ends the send");
    let queued_at = own_row(&alice, &channel_id, &id).await.expect("row")["sent_at"]
        .as_str()
        .expect("sent_at")
        .to_string();

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    restart(&alice).await;
    alice.invoke_json("flush_outbox", json!({})).await;

    let row = own_row(&alice, &channel_id, &id).await.expect("row");
    assert!(row.get("send_state").is_none(), "delivered: {row}");
    let landed_at = row["sent_at"].as_str().expect("sent_at").to_string();
    assert!(landed_at > queued_at, "{landed_at} should be after {queued_at}");
    let theirs: Vec<_> = bob
        .fetch_channel_messages(&channel_id)
        .await
        .into_iter()
        .filter(|m| m["id"] == id.as_str())
        .collect();
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0]["sent_at"], landed_at.as_str(), "sender and recipient agree");
    drop((alice, bob));
}

/// The DS's delivery order (`seq`, #1087) of a conversation's envelopes among `ids`.
async fn posted_order(channel_id: &str, ids: &[String]) -> Vec<String> {
    let conn = writable_remote().await.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT id FROM message_envelope WHERE conversation_id = ?1 ORDER BY seq",
            libsql::params![channel_id.to_string()],
        )
        .await
        .expect("envelope order");
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("row") {
        let id: String = row.get(0).expect("id");
        if ids.contains(&id) {
            out.push(id);
        }
    }
    out
}

type LineRow = (String, String, Option<String>);

/// This client's own rows among `ids`, as `(id, sent_at, send_state)`, in display order.
async fn own_line(client: &TestClient, channel_id: &str, ids: &[String]) -> Vec<LineRow> {
    let page = client
        .invoke_json("read_channel_messages", json!({ "channelId": channel_id, "limit": 200 }))
        .await;
    let mut rows: Vec<LineRow> = page["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|m| ids.iter().any(|id| m["id"] == id.as_str()))
        .map(|m| {
            (
                m["id"].as_str().expect("id").to_string(),
                m["sent_at"].as_str().expect("sent_at").to_string(),
                m.get("send_state").and_then(|s| s.as_str()).map(str::to_string),
            )
        })
        .collect();
    rows.sort_by(|a, b| (a.1.as_str(), a.0.as_str()).cmp(&(b.1.as_str(), b.0.as_str())));
    rows
}

/// A recipient's copies of `ids`, in display order.
async fn their_line(client: &TestClient, channel_id: &str, ids: &[String]) -> Vec<String> {
    let mut rows: Vec<(String, String)> = client
        .fetch_channel_messages(channel_id)
        .await
        .into_iter()
        .filter(|m| ids.iter().any(|id| m["id"] == id.as_str()))
        .map(|m| {
            (
                m["sent_at"].as_str().expect("sent_at").to_string(),
                m["id"].as_str().expect("id").to_string(),
            )
        })
        .collect();
    rows.sort();
    rows.into_iter().map(|(_, id)| id).collect()
}

type SendTask = tokio::task::JoinHandle<
    pollis_core::error::Result<pollis_core::commands::messages::Message>,
>;

/// Start `n` sends to `channel_id` at once, as a burst of taps does (#1636).
async fn burst(client: &TestClient, channel_id: &str, n: usize) -> (Vec<String>, Vec<SendTask>) {
    let mut ids = Vec::new();
    for _ in 0..n {
        let id: String = client.invoke_json("new_message_id", json!({})).await
            .as_str().expect("id").to_string();
        ids.push(id);
    }
    let tasks = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let state = client.state.clone();
            let channel_id = channel_id.to_string();
            let sender = client.user_id().to_string();
            let id = id.clone();
            tokio::spawn(async move {
                pollis_core::commands::messages::send_message_with_id(
                    Some(id),
                    channel_id,
                    sender,
                    format!("burst {i}"),
                    Vec::new(),
                    None,
                    None,
                    None,
                    &state,
                )
                .await
            })
        })
        .collect();
    (ids, tasks)
}

/// Rapid sends (#1636): concurrent sends are all delivered once each, one at a time, in the
/// order the outbox stored them, and every reader shows that order.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_burst_of_concurrent_sends_is_delivered_once_each_in_stored_order() {
    let (alice, bob, channel_id) = pair().await;
    let (ids, tasks) = burst(&alice, &channel_id, 6).await;
    for task in tasks {
        task.await.expect("join").expect("each send is delivered");
    }

    let line = own_line(&alice, &channel_id, &ids).await;
    assert_eq!(line.len(), ids.len(), "every send is stored once");
    assert!(line.iter().all(|(_, _, s)| s.is_none()), "all delivered: {line:?}");
    let stored: Vec<String> = line.iter().map(|(id, _, _)| id.clone()).collect();
    assert_eq!(posted_order(&channel_id, &ids).await, stored, "posted in the order shown");
    for id in &ids {
        assert_eq!(envelopes_with_id(id).await, 1, "{id} posted exactly once");
    }
    assert_eq!(their_line(&bob, &channel_id, &ids).await, stored, "the recipient agrees");
    drop((alice, bob));
}

/// Rapid sends (#1636): a burst is stored before any of it reaches the network, only one
/// message is ever out at a time, and a kill mid-burst (every send future dropped, one of
/// them sealed and parked before its post) loses nothing and doubles nothing: after the
/// restart the drain delivers every message exactly once, in the stored order.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_burst_killed_mid_delivery_is_delivered_once_each_in_order_after_restart() {
    use pollis_lib::commands::mls::rendezvous::{self, Point};
    let (alice, bob, channel_id) = pair().await;
    let mut parked = rendezvous::arm(Point::EnvelopeBeforePost);
    let (ids, tasks) = burst(&alice, &channel_id, 5).await;

    let release = tokio::time::timeout(std::time::Duration::from_secs(60), parked.recv())
        .await
        .expect("no send reached its post")
        .expect("rendezvous closed");

    // Every row of the burst is in the outbox while the first is still out.
    let mut line = Vec::new();
    for _ in 0..200 {
        line = own_line(&alice, &channel_id, &ids).await;
        if line.len() == ids.len() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(line.len(), ids.len(), "the whole burst is stored: {line:?}");
    assert!(
        line.iter().all(|(_, _, s)| s.as_deref() == Some("pending")),
        "stored as pending: {line:?}"
    );
    for id in &ids {
        assert_eq!(envelopes_with_id(id).await, 0, "nothing posted yet");
    }
    // One at a time: nobody else reaches a post while the first is parked.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(parked.try_recv().is_err(), "a second send reached its post concurrently");
    let stored: Vec<String> = line.iter().map(|(id, _, _)| id.clone()).collect();

    // The kill: every in-flight send future is dropped.
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        let _ = task.await;
    }
    rendezvous::disarm(Point::EnvelopeBeforePost);
    drop(release);

    restart(&alice).await;
    alice.invoke_json("flush_outbox", json!({})).await;

    let line = own_line(&alice, &channel_id, &ids).await;
    assert!(line.iter().all(|(_, _, s)| s.is_none()), "all delivered: {line:?}");
    let shown: Vec<String> = line.iter().map(|(id, _, _)| id.clone()).collect();
    assert_eq!(shown, stored, "the sender's order is the stored order");
    for id in &ids {
        assert_eq!(envelopes_with_id(id).await, 1, "{id} posted exactly once");
    }
    assert_eq!(posted_order(&channel_id, &ids).await, stored, "posted in stored order");
    assert_eq!(their_line(&bob, &channel_id, &ids).await, stored, "the recipient agrees");
    drop((alice, bob));
}

/// Rapid sends (#1636): a failed message never holds up the ones behind it. A later send
/// goes straight out and does not retry it; Retry then sends it, and it lands at the time it
/// went, after the message that overtook it.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_failed_message_does_not_hold_up_the_next_and_lands_when_retried() {
    let (alice, bob, channel_id) = pair().await;
    let outdated = alice.state.update_required.clone();

    outdated.store(true, std::sync::atomic::Ordering::SeqCst);
    let first: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    send_with_id(&alice, &channel_id, &first, "first, failed", None)
        .await
        .expect_err("an outdated client cannot send");
    outdated.store(false, std::sync::atomic::Ordering::SeqCst);

    let before = test_hooks::delivery_attempts();
    let second: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    send_with_id(&alice, &channel_id, &second, "second, behind it", None)
        .await
        .expect("the failed one ahead does not block this one");
    assert_eq!(test_hooks::delivery_attempts(), before + 1, "the failed one was not retried");
    assert_eq!(own_row(&alice, &channel_id, &first).await.expect("kept")["send_state"], "failed");
    assert_eq!(copies(&bob, &channel_id, "second, behind it").await, 1);

    alice
        .invoke_try("retry_message", json!({ "messageId": first }))
        .await
        .expect("retry delivers it");
    let ids = vec![first.clone(), second.clone()];
    let line = own_line(&alice, &channel_id, &ids).await;
    let shown: Vec<String> = line.iter().map(|(id, _, _)| id.clone()).collect();
    assert_eq!(shown, vec![second.clone(), first.clone()], "it lands when it went: {line:?}");
    assert_eq!(their_line(&bob, &channel_id, &ids).await, shown, "the recipient agrees");
    assert_eq!(copies(&bob, &channel_id, "first, failed").await, 1);
    drop((alice, bob));
}

/// Rapid sends (#1636): a later send's pass over the earlier rows never changes what they show.
/// A transient failure there leaves the earlier row `pending`; only its own call, Retry or the
/// drain marks it `failed`. The drain then delivers both in stored order.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_later_send_never_marks_an_earlier_row_failed() {
    let (alice, bob, channel_id) = pair().await;
    let outdated = alice.state.update_required.clone();

    // An earlier row whose own call is gone (a kill after the insert).
    let first: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    test_hooks::crash_next_send(Crash::AfterEnqueue);
    send_with_id(&alice, &channel_id, &first, "earlier, still pending", None)
        .await
        .expect_err("the simulated kill ends the send");

    outdated.store(true, std::sync::atomic::Ordering::SeqCst);
    let before = test_hooks::delivery_attempts();
    let second: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    send_with_id(&alice, &channel_id, &second, "later, fails itself", None)
        .await
        .expect_err("an outdated client cannot send");
    assert_eq!(test_hooks::delivery_attempts(), before + 2, "the earlier row was tried first");
    assert_eq!(
        own_row(&alice, &channel_id, &first).await.expect("kept")["send_state"],
        "pending",
        "another send's pass left the earlier row as it was"
    );
    assert_eq!(
        own_row(&alice, &channel_id, &second).await.expect("kept")["send_state"],
        "failed",
        "its own call marks it"
    );

    outdated.store(false, std::sync::atomic::Ordering::SeqCst);
    alice.invoke_json("flush_outbox", json!({})).await;
    let ids = vec![first.clone(), second.clone()];
    let line = own_line(&alice, &channel_id, &ids).await;
    assert!(line.iter().all(|(_, _, s)| s.is_none()), "all delivered: {line:?}");
    let shown: Vec<String> = line.iter().map(|(id, _, _)| id.clone()).collect();
    assert_eq!(shown, ids, "stored order");
    assert_eq!(posted_order(&channel_id, &ids).await, ids);
    assert_eq!(their_line(&bob, &channel_id, &ids).await, ids);
    drop((alice, bob));
}

/// One attachment send, as a task, with `size` bytes from the staging buffer.
fn spawn_attachment_send(
    client: &TestClient,
    channel_id: &str,
    id: &str,
    size: usize,
) -> SendTask {
    let staged = pollis_core::commands::staging::stage_attachment(vec![0x5a; size])
        .expect("stage")
        .id;
    let attachment: pollis_core::commands::messages::OutgoingAttachment = serde_json::from_value(
        json!({ "stagedId": staged, "filename": "big.bin", "contentType": "application/octet-stream" }),
    )
    .expect("attachment");
    let state = client.state.clone();
    let channel_id = channel_id.to_string();
    let sender = client.user_id().to_string();
    let id = id.to_string();
    tokio::spawn(async move {
        pollis_core::commands::messages::send_message_with_id(
            Some(id),
            channel_id,
            sender,
            "the big one".to_string(),
            vec![attachment],
            None,
            None,
            None,
            &state,
        )
        .await
    })
}

/// Rapid sends (#1636): a text sent while an earlier attachment send is still writing its row
/// waits for that row, so it never overtakes it. The attachment's row write is held open, so
/// only the enqueue gate can keep the order.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_text_sent_right_after_an_attachment_never_overtakes_it() {
    use pollis_lib::commands::mls::rendezvous::{self, Point};
    let (alice, bob, channel_id) = pair().await;
    let _stub = StubbedUploads::on();

    let mut before_row = rendezvous::arm(Point::OutboxBeforeRow);
    let big: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    let attachment = spawn_attachment_send(&alice, &channel_id, &big, 4 * 1024 * 1024);
    let release = tokio::time::timeout(std::time::Duration::from_secs(30), before_row.recv())
        .await
        .expect("the attachment send never reached its row")
        .expect("rendezvous closed");
    rendezvous::disarm(Point::OutboxBeforeRow);
    assert!(own_row(&alice, &channel_id, &big).await.is_none(), "its row is not written yet");

    let text: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    let text_send = {
        let state = alice.state.clone();
        let channel_id = channel_id.clone();
        let sender = alice.user_id().to_string();
        let text = text.clone();
        tokio::spawn(async move {
            pollis_core::commands::messages::send_message_with_id(
                Some(text),
                channel_id,
                sender,
                "right after it".to_string(),
                Vec::new(),
                None,
                None,
                None,
                &state,
            )
            .await
        })
    };

    // The text is stored, and waits: the earlier send has not written its row.
    let mut stored = None;
    for _ in 0..200 {
        stored = own_row(&alice, &channel_id, &text).await;
        if stored.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(stored.expect("the text is stored at once")["send_state"], "pending");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!text_send.is_finished(), "the text waits for the attachment's row");
    assert_eq!(envelopes_with_id(&text).await, 0, "and has not been posted");

    let _ = release.send(());
    attachment.await.expect("join").expect("the attachment is delivered");
    text_send.await.expect("join").expect("the text is delivered");

    let ids = vec![big.clone(), text.clone()];
    assert_eq!(posted_order(&channel_id, &ids).await, ids, "posted in the order sent");
    let shown: Vec<String> = own_line(&alice, &channel_id, &ids)
        .await
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    assert_eq!(shown, ids);
    assert_eq!(their_line(&bob, &channel_id, &ids).await, ids, "the recipient agrees");
    assert_eq!(alice.state.outbox_enqueuing.in_progress(), 0);
    drop((alice, bob));
}

/// Rapid sends (#1636): a send that fails, or is dropped, before it writes its row releases its
/// place, so the sends after it are never held up by it.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_send_that_never_writes_its_row_does_not_hold_up_the_next() {
    use pollis_lib::commands::mls::rendezvous::{self, Point};
    let (alice, bob, channel_id) = pair().await;
    let _stub = StubbedUploads::on();
    let text_send = |content: &'static str| {
        let state = alice.state.clone();
        let channel_id = channel_id.clone();
        let sender = alice.user_id().to_string();
        async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                pollis_core::commands::messages::send_message_with_id(
                    None,
                    channel_id,
                    sender,
                    content.to_string(),
                    Vec::new(),
                    None,
                    None,
                    None,
                    &state,
                ),
            )
            .await
            .expect("held up by a send that never wrote its row")
            .expect("delivered")
        }
    };

    // Errors before its row: a staged attachment that does not exist.
    let missing: pollis_core::commands::messages::OutgoingAttachment = serde_json::from_value(
        json!({ "stagedId": "no-such-staged-id", "filename": "x.bin", "contentType": "application/octet-stream" }),
    )
    .expect("attachment");
    let err = pollis_core::commands::messages::send_message_with_id(
        None,
        channel_id.clone(),
        alice.user_id().to_string(),
        "never stored".to_string(),
        vec![missing],
        None,
        None,
        None,
        &alice.state,
    )
    .await;
    assert!(err.is_err(), "a missing attachment fails the send");
    assert_eq!(alice.state.outbox_enqueuing.in_progress(), 0, "its place is released");
    text_send("after an error").await;

    // Dropped before its row: the future is aborted while its bytes are stored and its row is not.
    let mut before_row = rendezvous::arm(Point::OutboxBeforeRow);
    let dropped: String = alice.invoke_json("new_message_id", json!({})).await
        .as_str().expect("id").to_string();
    let task = spawn_attachment_send(&alice, &channel_id, &dropped, 64 * 1024);
    let release = tokio::time::timeout(std::time::Duration::from_secs(30), before_row.recv())
        .await
        .expect("never reached its row")
        .expect("rendezvous closed");
    rendezvous::disarm(Point::OutboxBeforeRow);
    assert_eq!(alice.state.outbox_enqueuing.in_progress(), 1);
    task.abort();
    let _ = task.await;
    drop(release);
    assert_eq!(alice.state.outbox_enqueuing.in_progress(), 0, "dropping it released its place");
    text_send("after a drop").await;

    assert!(own_row(&alice, &channel_id, &dropped).await.is_none(), "never accepted, never stored");
    for text in ["after an error", "after a drop"] {
        assert_eq!(copies(&bob, &channel_id, text).await, 1, "{text}");
    }
    drop((alice, bob));
}
