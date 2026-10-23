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
