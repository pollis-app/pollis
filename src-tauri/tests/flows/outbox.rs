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
