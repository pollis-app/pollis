//! Sealed sender v1 (issue #331, `docs/metadata-minimization-design.md` §2).
//!
//! The reader half — attributing every message from the MLS credential inside
//! the ciphertext rather than the server-writable `message_envelope.sender_id`
//! column — shipped as release N and is exercised by the whole existing flows
//! suite. These tests cover the SENDING half, which is now **unconditional** —
//! `POLLIS_SEAL_SENDER` is gone, and there is no code path that writes a real
//! `sender_id` into `message_envelope`:
//!
//!   - **sealed positive** — the stored envelope is blinded (`sealed = 1`,
//!     sentinel `sender_id`) yet the recipient still attributes the message to
//!     the real sender via the credential.
//!   - **non-member rejected** — a sealed send from a non-member is still
//!     refused by the DS membership gate (sealing relaxes the
//!     `sender_id == auth-user` binding, NOT the membership authz).
//!   - **no opt-out** — an ordinary client, constructed the ordinary way, seals.
//!     This is the invariant test: it fails if anyone reintroduces a way to send
//!     an envelope that names its author.

use std::sync::Arc;

use crate::harness::{signed_post_status, wipe, writable_remote, TestClient};
use pollis_lib::db::remote::RemoteDb;
use serial_test::serial;

/// Read `(sealed, sender_id)` for one stored `message_envelope` row straight from
/// the "remote Turso" handle — the server's at-rest view. This is exactly the
/// artifact sealed sender minimizes: a breach / subpoena dump of this table.
async fn envelope_sealed_and_sender(remote: &Arc<RemoteDb>, msg_id: &str) -> (i64, String) {
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

/// Stand up a two-member group channel and return `(group_id, channel_id)`.
/// `sender` is the creator/inviter; `receiver` accepts and applies the Welcome.
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

/// HEADLINE PROOF: sealing on blinds the server-stored sender while attribution
/// still works.
///
/// Alice sends an ordinary message. The stored `message_envelope`
/// row carries `sealed = 1` and the sentinel `sender_id` (`"sealed"`), NOT
/// Alice's real id — so a Turso breach reveals nothing about who sent it. Yet
/// Bob ingests the message and attributes it to Alice's REAL id, because the
/// reader takes the sender from the MLS credential inside the ciphertext, not
/// the (now-blinded) envelope column.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sealed_send_blinds_server_but_recipient_attributes_correctly() {
    wipe().await;

    // Alice's client has sealing ON; Bob's is a default (sealing off) client.
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let (_group_id, channel_id) = two_member_channel(&alice, &bob, &bob_profile.username).await;

    let msg_id = alice.send_channel_message_id(&channel_id, "sealed hello").await;

    // ── Server-stored envelope is blinded ──
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

    // ── Recipient attributes correctly from the MLS credential ──
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

    drop(alice);
    drop(bob);
}

/// A sealed send from a NON-member is still rejected by the DS membership gate.
/// Sealing relaxes only the `sender_id == auth-user` binding (the stored sender
/// is a sentinel, not the auth user); it does NOT relax "the authenticated writer
/// must be a member of the conversation". Mallory is authenticated (validly
/// signed) but not a member, so her sealed send gets a 403 (proved identity,
/// lacking permission), not a 401.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sealed_send_from_non_member_is_rejected() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let _mallory_profile = mallory.sign_up("mallory@test.local").await;

    let (_group_id, channel_id) = two_member_channel(&alice, &bob, &bob_profile.username).await;

    // Mallory signs a sealed send to alice+bob's channel. She is a real,
    // authenticated device but not a member — the membership gate must reject.
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

    // And nothing landed: no envelope row was written for mallory's attempt.
    let remote = writable_remote().await;
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

    drop(alice);
    drop(bob);
    drop(mallory);
}

/// INVARIANT: sealing has no opt-out. A client built the ordinary way — no
/// special constructor, no flag — must still blind the envelope. Before #331's
/// second release this same construction produced a real `sender_id`, so this
/// test is what catches a reintroduced escape hatch (a config field, an env
/// var, a conditional) that would silently restore per-message sender exposure
/// at rest.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn sealing_has_no_opt_out_for_an_ordinary_client() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let (_group_id, channel_id) = two_member_channel(&alice, &bob, &bob_profile.username).await;

    let msg_id = alice.send_channel_message_id(&channel_id, "plain hello").await;

    // The at-rest view must never name the author.
    let remote = writable_remote().await;
    let (sealed, envelope_sender) = envelope_sealed_and_sender(&remote, &msg_id).await;
    assert_eq!(sealed, 1, "an ordinary client's send must be sealed");
    assert_ne!(
        envelope_sender, alice_profile.id,
        "the stored envelope must not carry alice's real sender_id"
    );

    // Attribution is unaffected — it comes from the MLS credential.
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_msg = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("bob should ingest the message");
    assert_eq!(bob_msg["content"].as_str(), Some("plain hello"));
    assert_eq!(
        bob_msg["sender_id"].as_str(),
        Some(alice_profile.id.as_str()),
        "bob must still attribute the message to alice via the credential"
    );

    drop(alice);
    drop(bob);
}
