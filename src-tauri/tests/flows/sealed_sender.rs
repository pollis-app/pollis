//! Sealed sender (#331), unconditional since #607.

use std::sync::Arc;

use crate::harness::{signed_post_status, wipe, writable_remote, TestClient, TEST_PIN};
use pollis_delivery::db::Db;
use serial_test::serial;

/// `(sealed, sender_id)` of one stored envelope, as a breach would see it.
async fn envelope_sealed_and_sender(remote: &Arc<Db>, msg_id: &str) -> (i64, String) {
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT sealed, sender_id FROM message_envelope WHERE id = ?1",
            libsql::params![msg_id.to_string()],
        )
        .await
        .expect("envelope query");
    let row = rows
        .next()
        .await
        .expect("envelope row")
        .expect("envelope row should exist for the sent message");
    (
        row.get::<i64>(0).expect("sealed"),
        row.get::<String>(1).expect("sender_id"),
    )
}

async fn two_member_channel(sender: &TestClient, receiver: &TestClient, receiver_username: &str) -> (String, String) {
    let group_id = sender.create_group("Sealed").await;
    sender.invite(&group_id, receiver_username).await;
    let invite_id = receiver
        .first_pending_invite()
        .await
        .expect("pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    receiver.accept_invite(&invite_id).await;
    receiver.poll().await;
    sender.process_commits_for(&sender.general_channel_id(&group_id).await).await;
    let channel_id = sender.general_channel_id(&group_id).await;
    (group_id, channel_id)
}

/// Blinded sender with correct attribution, a refused non-member, and no unsealed row anywhere.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn every_send_is_sealed_attributed_and_membership_gated() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let _mallory_profile = mallory.sign_up("mallory@test.local").await;

    let (_group_id, channel_id) = two_member_channel(&alice, &bob, &bob_profile.username).await;

    let msg_id = alice.send_channel_message_id(&channel_id, "sealed hello").await;

    let remote = writable_remote().await;
    let (sealed, envelope_sender) = envelope_sealed_and_sender(&remote, &msg_id).await;
    assert_eq!(sealed, 1, "sealed send must store sealed = 1");
    assert_eq!(
        envelope_sender, "sealed",
        "sealed send must store the sentinel, not a real id"
    );
    assert_ne!(
        envelope_sender, alice_profile.id,
        "the server-stored sender must NOT be alice's real id"
    );

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_msg = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("bob should ingest the sealed message");
    assert_eq!(
        bob_msg["content"].as_str(),
        Some("sealed hello"),
        "bob should decrypt the sealed message"
    );
    assert_eq!(
        bob_msg["sender_id"].as_str(),
        Some(alice_profile.id.as_str()),
        "bob must attribute the sealed message to alice's REAL id (from the MLS \
         credential), even though the envelope column was blinded to the sentinel"
    );

    // Mallory is authenticated but not a member.
    let body = serde_json::json!({
        "id": "01JSEALEDNONMEMBER00000000",
        "conversation_id": channel_id,
        "sender_id": "sealed",
        "sealed": 1,
        "ciphertext": "mls:00",
        "sent_at": "2024-01-01T00:00:00+00:00",
    });
    let body_bytes = serde_json::to_vec(&body).expect("serialize body");
    let status = signed_post_status(&mallory, "/v1/messages/send", &body_bytes).await;
    assert_eq!(
        status, 403,
        "a sealed send from a non-member must be rejected by the DS membership gate"
    );

    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM message_envelope WHERE id = ?1",
            libsql::params!["01JSEALEDNONMEMBER00000000".to_string()],
        )
        .await
        .expect("count query");
    let count: i64 = rows
        .next()
        .await
        .expect("row")
        .expect("some row")
        .get(0)
        .expect("count");
    assert_eq!(count, 0, "the rejected sealed send must not have been persisted");

    // Redactions ride the same sealed send path.
    let doomed_id = alice.send_channel_message_id(&channel_id, "doomed message").await;
    alice.delete_message(&doomed_id).await;

    // Load-bearing: no `type='message'` row in the whole table is unsealed.
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM message_envelope \
             WHERE type = 'message' AND (sealed <> 1 OR sender_id <> 'sealed')",
            libsql::params![],
        )
        .await
        .expect("count query");
    let bad: i64 = rows
        .next()
        .await
        .expect("row")
        .expect("some row")
        .get(0)
        .expect("count");
    assert_eq!(
        bad, 0,
        "every posted message/redaction envelope must be sealed=1 with the \
         sentinel sender_id — found {bad} unsealed/real-sender rows"
    );

    drop(alice);
    drop(bob);
    drop(mallory);
}

/// Sealed attachments are ref-counted: authorship is proven by the deletion capability (#1165, #1086).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_sealed_messages_attachment_is_reference_counted() {
    wipe().await;

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let group_id = alice.create_group("Attachments").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    let hash = "ab".repeat(32);
    let content = serde_json::json!({
        "_att": [{
            "hash": hash,
            "key": format!("media/{hash}.enc"),
            "filename": "photo.png",
            "content_type": "image/png",
            "size": 1,
        }]
    })
    .to_string();
    let msg_id = alice.send_channel_message_id(&channel_id, &content).await;

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM attachment_ref ar \
             JOIN message_envelope me ON me.id = ar.message_id \
             WHERE ar.content_hash = ?1 AND ar.message_id = ?2 AND me.sender_id = 'sealed'",
            libsql::params![hash.clone(), msg_id.clone()],
        )
        .await
        .expect("attachment_ref query");
    let counted: i64 = rows
        .next()
        .await
        .expect("row")
        .expect("some row")
        .get(0)
        .expect("count");
    assert_eq!(
        counted, 1,
        "the sealed message's attachment must hold a live reference on the DS"
    );

    drop(alice);
}

/// Pre-#1330 attachments are re-declared once after a PIN unlock.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_unlock_declares_the_references_older_sends_missed() {
    wipe().await;

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let group_id = alice.create_group("Backfill").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    let hash = "cd".repeat(32);
    let content = serde_json::json!({
        "_att": [{
            "hash": hash,
            "key": format!("media/{hash}.enc"),
            "filename": "photo.png",
            "content_type": "image/png",
            "size": 1,
        }]
    })
    .to_string();
    let msg_id = alice.send_channel_message_id(&channel_id, &content).await;

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    conn.execute(
        "DELETE FROM attachment_ref WHERE content_hash = ?1",
        libsql::params![hash.clone()],
    )
    .await
    .expect("drop the reference");

    let uid = alice.user_id().to_string();
    pollis_lib::test_harness::invoke::<()>(&alice.webview, "lock", serde_json::json!({}))
        .await
        .expect("lock");
    pollis_lib::test_harness::invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        serde_json::json!({ "userId": uid, "pin": TEST_PIN }),
    )
    .await
    .expect("unlock");

    let mut counted = 0i64;
    for _ in 0..100 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM attachment_ref WHERE content_hash = ?1 AND message_id = ?2",
                libsql::params![hash.clone(), msg_id.clone()],
            )
            .await
            .expect("attachment_ref query");
        counted = rows.next().await.expect("row").expect("some row").get(0).expect("count");
        if counted == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(counted, 1, "the unlock must re-declare the missed reference");

    drop(alice);
}
