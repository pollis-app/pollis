//! The attachment key ring (#1382), end to end through the real MLS pipeline.
//!
//! Attachments are sealed under a per-conversation ring of keys that only MLS
//! members can unwrap. The promises, each pinned here:
//!
//!   * the ring exists from group creation, and a member who joins later opens
//!     it (so files posted before they joined stay readable to them);
//!   * removing a member rotates the ring, and the remaining members pick up
//!     the new key;
//!   * the removed member cannot reach the ring any more;
//!   * what the server stores is a wrap of the ring, never a bare key.

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

    // Joining after the ring was minted: both open the same ring.
    join(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;
    join(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;
    assert_eq!(probe(&bob, &channel_id).await, Some(1), "a late joiner opens the ring");
    assert_eq!(probe(&carol, &channel_id).await, Some(1));

    // At rest: one wrapped entry, never a bare key.
    let (version, wrap_len) = stored_ring(&group_id).await;
    assert_eq!(version, 1);
    assert_eq!(wrap_len, 48 + 16, "the stored ring is a wrap (entry + tag), not a key");

    // Removing carol rotates the ring.
    alice.remove_member(&group_id, &carol_p.id).await;
    assert_eq!(probe(&alice, &channel_id).await, Some(2), "the remover rotates");
    let (version, wrap_len) = stored_ring(&group_id).await;
    assert_eq!(version, 2);
    assert_eq!(wrap_len, 2 * 48 + 16);

    bob.process_commits_for(&channel_id).await;
    assert_eq!(probe(&bob, &channel_id).await, Some(2), "remaining members pick up the new key");

    // Carol kept her copy of v1, but the server refuses her the ring and she
    // has no exporter for the epoch it is wrapped at: v2 is out of reach.
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

// ── A DM member without the ring ─────────────────────────────────────────────
//
// Production report: a photo in a 1:1 DM failed with "this conversation's
// file key is not available on this device yet", while text in the same DM
// went through. The ring is wrapped under ONE epoch's exporter, and a device
// that was not holding the ring could open it only while sitting at exactly
// that epoch.

/// Clears the outbox upload stub however the test ends.
struct StubbedUploads;

impl StubbedUploads {
    fn on() -> Self {
        pollis_core::commands::messages::test_hooks::stub_attachment_uploads(true);
        StubbedUploads
    }
}

impl Drop for StubbedUploads {
    fn drop(&mut self) {
        pollis_core::commands::messages::test_hooks::stub_attachment_uploads(false);
    }
}

/// Alice and Bob in an accepted 1:1 DM, both caught up. Returns the DM id.
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

/// Make the DM look like one created before the ring existed (#1382 shipped
/// in 1.0.11): no ring on the DS, and no ring and no kept KEKs on either
/// device. The first upload then mints it.
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

/// Drop everything this device knows about the ring: its cached copy and any
/// exporter KEKs it kept for past epochs.
async fn wipe_local_ring(client: &TestClient, dm_id: &str) {
    let guard = client.state.local_db.lock().await;
    let db = guard.as_ref().expect("signed in");
    db.conn()
        .execute("DELETE FROM file_key_cache WHERE conversation_id = ?1", [dm_id])
        .expect("clear cache");
    // A DM is generation 0, whose MLS group id is the DM id.
    db.conn()
        .execute("DELETE FROM file_kek_history WHERE mls_group_id = ?1", [dm_id])
        .expect("clear kept KEKs");
}

/// Send a one-attachment message through the outbox. Returns the message id
/// and the send's result.
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

/// The sender's own copy's `send_state` (`None` once delivered).
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

/// The captions of every attachment message `client` can read in the DM.
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

/// The peer minted the ring and then moved the epoch on; this device has not
/// processed that commit yet. The upload runs before the send's catch-up, so
/// it met a wrap from an epoch it had not reached — and every Retry did too.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_dm_member_behind_the_rings_epoch_still_sends_a_photo() {
    wipe().await;
    let _stub = StubbedUploads::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let dm_id = dm_pair(&mut alice, &mut bob).await;
    forget_the_ring(&dm_id, &[&alice, &bob]).await;

    let (_, sent) = send_photo(&alice, &dm_id, "alice mints").await;
    sent.expect("alice's photo mints the ring");
    assert!(alice.self_update(&dm_id).await, "alice moves the epoch on");

    let (id, sent) = send_photo(&bob, &dm_id, "The icon is so tiny on my phone!").await;
    sent.expect("bob, one commit behind, still sends the photo");
    assert_eq!(own_send_state(&bob, &dm_id, &id).await, None, "delivered");
    assert!(received_captions(&alice, &dm_id)
        .await
        .contains(&"The icon is so tiny on my phone!".to_string()));
    drop((alice, bob));
}

/// The peer minted the ring; this device then committed (a self-update) before
/// the peer processed it, so the only wrap is at an epoch this device has
/// left. Its exporter for that epoch is gone after the merge, so it could not
/// open the wrap until the peer came online and re-wrapped.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_dm_member_past_the_rings_epoch_still_sends_a_photo() {
    wipe().await;
    let _stub = StubbedUploads::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let dm_id = dm_pair(&mut alice, &mut bob).await;
    forget_the_ring(&dm_id, &[&alice, &bob]).await;

    let (_, sent) = send_photo(&alice, &dm_id, "alice mints").await;
    sent.expect("alice's photo mints the ring");
    // Bob moves past the ring's epoch while alice is offline: she does not
    // process his commit, so she cannot re-wrap at his epoch.
    assert!(bob.self_update(&dm_id).await, "bob moves the epoch on");

    let (id, sent) = send_photo(&bob, &dm_id, "The icon is so tiny on my phone!").await;
    sent.expect("bob, past the ring's epoch, still sends the photo");
    assert_eq!(own_send_state(&bob, &dm_id, &id).await, None, "delivered");
    assert!(received_captions(&alice, &dm_id)
        .await
        .contains(&"The icon is so tiny on my phone!".to_string()));
    drop((alice, bob));
}

/// When the key genuinely is not reachable yet (only an offline peer can wrap
/// it at an epoch this device can open), the photo is not a failure: it stays
/// queued, and the outbox delivers it once the peer's re-wrap lands.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_photo_waiting_for_the_file_key_stays_queued_and_goes_out_once_it_arrives() {
    wipe().await;
    let _stub = StubbedUploads::on();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let dm_id = dm_pair(&mut alice, &mut bob).await;
    forget_the_ring(&dm_id, &[&alice, &bob]).await;

    let (_, sent) = send_photo(&alice, &dm_id, "alice mints").await;
    sent.expect("alice's photo mints the ring");
    assert!(bob.self_update(&dm_id).await, "bob moves the epoch on");
    // And has no way to open the epoch the ring is wrapped at: a device that
    // never sat at that epoch, or whose build kept no KEKs.
    wipe_local_ring(&bob, &dm_id).await;

    let (id, _) = send_photo(&bob, &dm_id, "waits for the key").await;
    assert_eq!(
        own_send_state(&bob, &dm_id, &id).await.as_deref(),
        Some("pending"),
        "a missing file key is a wait, not a failure"
    );

    // Alice comes online, processes bob's commit and re-wraps at his epoch.
    alice.process_commits_for(&dm_id).await;
    bob.invoke_json("flush_outbox", serde_json::json!({})).await;
    assert_eq!(own_send_state(&bob, &dm_id, &id).await, None, "delivered by the drain");
    assert!(received_captions(&alice, &dm_id)
        .await
        .contains(&"waits for the key".to_string()));
    drop((alice, bob));
}
