//! Pinned messages (#99), end to end through the real command pipeline.
//!
//! The design's client-visible promises, each pinned by a scenario:
//!
//!   * a member who joins AFTER a pin was created — by Welcome or by
//!     external-join recovery — still reads it (the wrapped-Kpin model's whole
//!     point versus per-epoch keys);
//!   * unpinning by any member removes the pin for everyone;
//!   * a removed member can no longer list pins at all;
//!   * the server-side rows are ciphertext: neither the pin content nor the
//!     pin key ever appears in what a Turso breach would dump.

use crate::harness::{arm_ds_fault, ds_fault_armed, wipe, writable_remote, DsFault, TestClient};
use serial_test::serial;

/// Read the pin rows as a Turso breach would.
async fn stored_pin_blobs(conversation_id: &str) -> Vec<Vec<u8>> {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT encrypted_content FROM pinned_message WHERE conversation_id = ?1",
            libsql::params![conversation_id],
        )
        .await
        .expect("query pins");
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("row") {
        out.push(row.get::<Vec<u8>>(0).expect("blob"));
    }
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The issue's headline scenario: pin first, join second, read anyway — plus
/// the at-rest check that the server holds only ciphertext.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_member_joining_after_the_pin_still_reads_it() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Pins").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    let msg_id = alice
        .send_channel_message_id(&channel_id, "the load-bearing decision")
        .await;

    // Pin BEFORE bob exists in the tree.
    alice
        .invoke_json(
            "pin_message",
            serde_json::json!({
                "conversationId": channel_id,
                "messageId": msg_id,
                "userId": alice_p.id,
            }),
        )
        .await;

    // At rest: the pin row must carry neither the words nor the key.
    let blobs = stored_pin_blobs(&channel_id).await;
    assert_eq!(blobs.len(), 1);
    assert!(
        !contains(&blobs[0], b"the load-bearing decision"),
        "pin plaintext appears in the server-side row"
    );

    // Bob joins via the normal Welcome path. Alice's invite commit advanced
    // the epoch, and her committer hook re-wrapped Kpin under the new
    // exporter — the same epoch bob's Welcome lands him at.
    alice.invite(&group_id, &bob_p.username).await;
    let invite_id = bob.first_pending_invite().await.expect("invite")["id"]
        .as_str()
        .expect("id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    bob.process_commits_for(&channel_id).await;

    let pins = bob
        .invoke_json(
            "list_pinned_messages",
            serde_json::json!({ "conversationId": channel_id, "userId": bob_p.id }),
        )
        .await;
    let pins = pins.as_array().expect("array");
    assert_eq!(pins.len(), 1, "bob must see the pin created before he joined");
    assert_eq!(
        pins[0]["content"].as_str().unwrap(),
        "the load-bearing decision"
    );
    assert_eq!(pins[0]["message_id"].as_str().unwrap(), msg_id);
    assert_eq!(pins[0]["pinned_by"].as_str().unwrap(), alice_p.id);
    assert_eq!(pins[0]["sender_id"].as_str().unwrap(), alice_p.id);

    drop(alice);
    drop(bob);
}

/// The recovery path: bob's Welcome is dropped, he external-joins, and reads
/// the pre-existing pin once a key-holding member has processed his join
/// (whose re-wrap is exactly what an external joiner waits on).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_external_joiner_reads_existing_pins_after_a_member_rewraps() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("PinRecovery").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    let msg_id = alice
        .send_channel_message_id(&channel_id, "pinned before the join")
        .await;
    alice
        .invoke_json(
            "pin_message",
            serde_json::json!({
                "conversationId": channel_id,
                "messageId": msg_id,
                "userId": alice_p.id,
            }),
        )
        .await;

    // Drop bob's Welcome so his only way in is the external-join recovery.
    arm_ds_fault(DsFault::DropWelcome);
    alice.invite(&group_id, &bob_p.username).await;
    assert!(!ds_fault_armed(), "DropWelcome should have fired on bob's add");

    let invite_id = bob.first_pending_invite().await.expect("invite")["id"]
        .as_str()
        .expect("id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    bob.process_commits_for(&channel_id).await;

    // Bob's own external commit advanced the epoch past the stored wrap, and
    // he has never held Kpin — so the pins are UNAVAILABLE to him until a
    // key-holding member processes his join and re-wraps. That is the honest
    // intermediate state, not a failure.
    // Alice processes bob's join; her backstop re-wraps at the new epoch.
    alice.process_commits_for(&channel_id).await;

    let pins = bob
        .invoke_json(
            "list_pinned_messages",
            serde_json::json!({ "conversationId": channel_id, "userId": bob_p.id }),
        )
        .await;
    let pins = pins.as_array().expect("array");
    assert_eq!(
        pins.len(),
        1,
        "an external joiner must read pre-existing pins once a member re-wrapped"
    );
    assert_eq!(pins[0]["content"].as_str().unwrap(), "pinned before the join");

    drop(alice);
    drop(bob);
}

/// Any member may unpin, and the unpin is for everyone.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn any_member_can_unpin_for_everyone() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Unpin").await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite_id = bob.first_pending_invite().await.expect("invite")["id"]
        .as_str()
        .expect("id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    let channel_id = alice.general_channel_id(&group_id).await;
    let msg_id = alice.send_channel_message_id(&channel_id, "ephemeral").await;
    alice
        .invoke_json(
            "pin_message",
            serde_json::json!({
                "conversationId": channel_id,
                "messageId": msg_id,
                "userId": alice_p.id,
            }),
        )
        .await;
    bob.process_commits_for(&channel_id).await;

    // Bob — who did not pin it — unpins it.
    bob.invoke_json(
        "unpin_message",
        serde_json::json!({
            "conversationId": channel_id,
            "messageId": msg_id,
            "userId": bob_p.id,
        }),
    )
    .await;

    let pins = alice
        .invoke_json(
            "list_pinned_messages",
            serde_json::json!({ "conversationId": channel_id, "userId": alice_p.id }),
        )
        .await;
    assert!(
        pins.as_array().unwrap().is_empty(),
        "bob's unpin must remove the pin for alice too"
    );

    drop(alice);
    drop(bob);
}

/// A removed member's pin reads are refused outright — the DS's membership
/// gate is what stands between an ex-member's retained key and future pin
/// ciphertext.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_removed_member_can_no_longer_list_pins() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Removal").await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite_id = bob.first_pending_invite().await.expect("invite")["id"]
        .as_str()
        .expect("id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    let channel_id = alice.general_channel_id(&group_id).await;
    let msg_id = alice.send_channel_message_id(&channel_id, "before removal").await;
    alice
        .invoke_json(
            "pin_message",
            serde_json::json!({
                "conversationId": channel_id,
                "messageId": msg_id,
                "userId": alice_p.id,
            }),
        )
        .await;
    bob.process_commits_for(&channel_id).await;

    // While a member, bob reads pins fine.
    let pins = bob
        .invoke_json(
            "list_pinned_messages",
            serde_json::json!({ "conversationId": channel_id, "userId": bob_p.id }),
        )
        .await;
    assert_eq!(pins.as_array().unwrap().len(), 1);

    alice.remove_member(&group_id, &bob_p.id).await;

    let result = bob
        .invoke_try(
            "list_pinned_messages",
            serde_json::json!({ "conversationId": channel_id, "userId": bob_p.id }),
        )
        .await;
    assert!(
        result.is_err(),
        "a removed member's pin list must be refused, got: {result:?}"
    );

    drop(alice);
    drop(bob);
}

/// The pin key state at rest: the server row is a ~48-byte wrap, not the key.
/// A breach dump of `pin_keystate` must not contain any 32-byte value that
/// decrypts the pins (we can't test decryption directly here, but we can pin
/// the row's shape: wrapped blob = key + AEAD tag, never the bare key size).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_stored_keystate_is_a_wrap_not_a_key() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let group_id = alice.create_group("Keystate").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    let msg_id = alice.send_channel_message_id(&channel_id, "x").await;
    alice
        .invoke_json(
            "pin_message",
            serde_json::json!({
                "conversationId": channel_id,
                "messageId": msg_id,
                "userId": alice_p.id,
            }),
        )
        .await;

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let mut rows = conn
        .query(
            "SELECT wrapped_kpin, nonce, epoch FROM pin_keystate WHERE conversation_id = ?1",
            libsql::params![group_id],
        )
        .await
        .expect("query");
    let row = rows
        .next()
        .await
        .expect("row")
        .expect("keystate row must exist after the first pin");
    let wrapped = row.get::<Vec<u8>>(0).expect("wrap");
    let nonce = row.get::<Vec<u8>>(1).expect("nonce");
    assert_eq!(
        wrapped.len(),
        48,
        "the stored blob must be key+tag (a wrap), not a bare 32-byte key"
    );
    assert_eq!(nonce.len(), 12);

    drop(alice);
}
