//! The attachment key ring (#1382), end to end through the real MLS pipeline.

use crate::harness::{wipe, writable_remote, TestClient};
use serial_test::serial;

async fn probe(client: &TestClient, conversation_id: &str) -> Option<i64> {
    match client
        .invoke_try(
            "test_file_key_version",
            serde_json::json!({ "conversationId": conversation_id }),
        )
        .await
    {
        Ok(v) => v.as_i64(),
        Err(_) => None,
    }
}

async fn stored_ring(group_id: &str) -> (i64, usize) {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let mut rows = conn
        .query(
            "SELECT key_version, wrapped_keyring FROM file_keystate WHERE conversation_id = ?1",
            libsql::params![group_id.to_string()],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("a ring row must exist");
    (
        row.get::<i64>(0).expect("key_version"),
        row.get::<Vec<u8>>(1).expect("wrap").len(),
    )
}

async fn join(owner: &TestClient, joiner: &TestClient, group_id: &str, channel_id: &str, username: &str) {
    owner.invite(group_id, username).await;
    let invite_id = joiner.first_pending_invite().await.expect("invite")["id"]
        .as_str()
        .expect("id")
        .to_string();
    joiner.accept_invite(&invite_id).await;
    joiner.poll().await;
    joiner.process_commits_for(channel_id).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn late_joiners_open_the_ring_and_a_removal_rotates_it_out_of_reach() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Files").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    assert_eq!(probe(&alice, &channel_id).await, Some(1), "creation mints the ring");

    join(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    join(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;
    assert_eq!(probe(&bob, &channel_id).await, Some(1), "a late joiner opens the ring");
    assert_eq!(probe(&carol, &channel_id).await, Some(1));

    let (version, wrap_len) = stored_ring(&group_id).await;
    assert_eq!(version, 1);
    assert_eq!(wrap_len, 48 + 16, "the stored ring is a wrap (entry + tag), not a key");

    alice.remove_member(&group_id, &carol_p.id).await;
    assert_eq!(probe(&alice, &channel_id).await, Some(2), "the remover rotates");
    let (version, wrap_len) = stored_ring(&group_id).await;
    assert_eq!(version, 2);
    assert_eq!(wrap_len, 2 * 48 + 16);

    bob.process_commits_for(&channel_id).await;
    assert_eq!(probe(&bob, &channel_id).await, Some(2), "remaining members pick up the new key");

    // Carol kept v1, but has neither the ring nor the exporter for its epoch.
    carol.poll().await;
    let carol_sees = probe(&carol, &channel_id).await;
    assert!(
        carol_sees.unwrap_or(0) < 2,
        "a removed member must not reach the key added at her removal (saw {carol_sees:?})"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

// A DM member without the ring: it is wrapped under ONE epoch's exporter.

async fn dm_pair(alice: &mut TestClient, bob: &mut TestClient) -> String {
    let a = alice.sign_up("fk-dm-alice@test.local").await;
    let b = bob.sign_up("fk-dm-bob@test.local").await;
    let dm_id = alice.create_dm(&[a.id.as_str(), b.id.as_str()]).await;
    bob.accept_dm_request(&dm_id).await;
    bob.poll().await;
    bob.process_commits_for(&dm_id).await;
    alice.process_commits_for(&dm_id).await;
    bob.process_commits_for(&dm_id).await;
    dm_id
}

/// A DM from before the ring existed (pre-1.0.11): the first upload mints it.
async fn forget_the_ring(dm_id: &str, clients: &[&TestClient]) {
    let remote = writable_remote().await;
    remote
        .conn()
        .await
        .expect("conn")
        .execute(
            "DELETE FROM file_keystate WHERE conversation_id = ?1",
            libsql::params![dm_id.to_string()],
        )
        .await
        .expect("delete ring row");
    for c in clients {
        wipe_local_ring(c, dm_id).await;
    }
}

/// Drop the cached ring and any kept exporter KEKs.
async fn wipe_local_ring(client: &TestClient, dm_id: &str) {
    let guard = client.state.local_db.lock().await;
    let db = guard.as_ref().expect("signed in");
    db.conn()
        .execute("DELETE FROM file_key_cache WHERE conversation_id = ?1", [dm_id])
        .expect("clear cache");
    db.conn()
        .execute("DELETE FROM file_kek_history WHERE mls_group_id = ?1", [dm_id])
        .expect("clear kept KEKs");
}

async fn send_photo(
    client: &TestClient,
    dm_id: &str,
    caption: &str,
) -> (String, Result<serde_json::Value, String>) {
    let staged = pollis_core::commands::staging::stage_attachment(b"\x89PNG photo bytes".to_vec())
        .expect("stage")
        .id;
    let id = client
        .invoke_json("new_message_id", serde_json::json!({}))
        .await
        .as_str()
        .expect("id")
        .to_string();
    let result = client
        .invoke_try(
            "send_message",
            serde_json::json!({
                "conversationId": dm_id,
                "senderId": client.user_id(),
                "content": caption,
                "replyToId": null,
                "senderUsername": null,
                "messageId": id,
                "attachments": [{
                    "stagedId": staged,
                    "filename": "photo.png",
                    "contentType": "image/png",
                }],
            }),
        )
        .await;
    (id, result)
}

/// `send_state` of the sender's copy (`None` once delivered).
async fn own_send_state(client: &TestClient, dm_id: &str, id: &str) -> Option<String> {
    let page = client
        .invoke_json(
            "read_channel_messages",
            serde_json::json!({ "channelId": dm_id, "limit": 50 }),
        )
        .await;
    let row = page["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["id"] == id)
        .cloned()
        .expect("own row");
    row.get("send_state").and_then(|s| s.as_str()).map(str::to_string)
}

async fn received_captions(client: &TestClient, dm_id: &str) -> Vec<String> {
    client
        .fetch_dm_messages(dm_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str())
        .filter_map(|c| serde_json::from_str::<serde_json::Value>(c).ok())
        .filter_map(|v| v["_txt"].as_str().map(str::to_string))
        .collect()
}

/// #2109: a new photo is a shared object keyed by its own reference, so it needs no
/// conversation file key at all. A DM whose ring this device cannot open (none stored,
/// none cached) still sends at once, and the peer opens it.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_photo_needs_no_conversation_file_key() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let dm_id = dm_pair(&mut alice, &mut bob).await;
    forget_the_ring(&dm_id, &[&alice, &bob]).await;
    wipe_local_ring(&bob, &dm_id).await;

    let (id, sent) = send_photo(&bob, &dm_id, "no ring needed").await;
    sent.expect("the photo sends without a file key");
    assert_eq!(own_send_state(&bob, &dm_id, &id).await, None, "delivered at once");
    assert!(received_captions(&alice, &dm_id)
        .await
        .contains(&"no ring needed".to_string()));
    drop((alice, bob));
}
