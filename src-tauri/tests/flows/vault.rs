//! The Vault (#107), end to end through the real command pipeline.
//!
//! The product promise under test: the vault behaves like cloud storage —
//! every entry persists across devices (the stated inversion of "a new device
//! starts empty", achieved by deriving the vault key from the ACCOUNT
//! identity key), pin/unpin and edits sync, deletes are gone everywhere, and
//! the server holds ciphertext it can neither read nor tell a pin flip from
//! an edit.

use crate::harness::{wipe, writable_remote, TestClient};
use serial_test::serial;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Wipe the local decrypted cache, simulating what a brand-new device holds
/// locally: nothing. The next `get_vault_messages` must rebuild everything
/// from the remote ciphertext using only the account identity key — which is
/// exactly the new-device story, exercised without a second enrollment
/// (harness devices of one user share the local DB file, so a real second
/// device would vacuously share the cache too).
async fn wipe_local_vault_cache(client: &TestClient) {
    let guard = client.state.local_db.lock().await;
    let db = guard.as_ref().expect("signed in");
    db.conn()
        .execute("DELETE FROM vault_entry_cache", [])
        .expect("wipe vault cache");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn vault_entries_rebuild_from_remote_ciphertext_alone() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;

    let sent = alice
        .invoke_json(
            "send_vault_message",
            serde_json::json!({ "userId": alice_p.id, "content": "remember the milk" }),
        )
        .await;
    let entry_id = sent["id"].as_str().expect("id").to_string();
    alice
        .invoke_json(
            "send_vault_message",
            serde_json::json!({ "userId": alice_p.id, "content": "second note" }),
        )
        .await;

    // Pin the first entry — on the wire this is indistinguishable from an
    // edit, and the flag must survive the round trip.
    alice
        .invoke_json(
            "set_vault_message_pinned",
            serde_json::json!({ "id": entry_id, "pinned": true, "userId": alice_p.id }),
        )
        .await;

    // The breach view: rows exist, and carry neither the words nor the flag.
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let mut rows = conn
        .query(
            "SELECT blob FROM vault_message WHERE user_id = ?1",
            libsql::params![alice_p.id.clone()],
        )
        .await
        .expect("query");
    let mut blobs = Vec::new();
    while let Some(row) = rows.next().await.expect("row") {
        blobs.push(row.get::<Vec<u8>>(0).expect("blob"));
    }
    assert_eq!(blobs.len(), 2);
    for blob in &blobs {
        assert!(
            !contains(blob, b"remember the milk") && !contains(blob, b"second note"),
            "vault plaintext appears in the server-side row"
        );
        assert!(
            !contains(blob, b"pinned"),
            "the pinned flag leaked into the stored bytes"
        );
    }

    // The new-device moment: local cache gone, remote ciphertext + account
    // key are everything. The vault must come back whole, pin flag included.
    wipe_local_vault_cache(&alice).await;
    let listed = alice
        .invoke_json(
            "get_vault_messages",
            serde_json::json!({ "userId": alice_p.id }),
        )
        .await;
    let listed = listed.as_array().expect("array");
    assert_eq!(listed.len(), 2, "every entry must survive a device with no local state");
    let first = listed
        .iter()
        .find(|m| m["id"].as_str() == Some(entry_id.as_str()))
        .expect("first entry");
    assert_eq!(first["content"].as_str().unwrap(), "remember the milk");
    assert_eq!(first["pinned"].as_bool(), Some(true), "the pin flag must sync");

    drop(alice);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn edits_and_deletes_are_authoritative_on_the_server() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;

    let a = alice
        .invoke_json(
            "send_vault_message",
            serde_json::json!({ "userId": alice_p.id, "content": "draft one" }),
        )
        .await;
    let a_id = a["id"].as_str().unwrap().to_string();
    let b = alice
        .invoke_json(
            "send_vault_message",
            serde_json::json!({ "userId": alice_p.id, "content": "doomed" }),
        )
        .await;
    let b_id = b["id"].as_str().unwrap().to_string();

    let edited = alice
        .invoke_json(
            "edit_vault_message",
            serde_json::json!({ "id": a_id, "newContent": "draft two", "userId": alice_p.id }),
        )
        .await;
    assert_eq!(edited["content"].as_str().unwrap(), "draft two");
    assert_eq!(
        edited["created_at"].as_str().unwrap(),
        a["created_at"].as_str().unwrap(),
        "an edit must not move the entry's creation time"
    );

    alice
        .invoke_json(
            "delete_vault_message",
            serde_json::json!({ "id": b_id, "userId": alice_p.id }),
        )
        .await;

    // Rebuild from remote: the edit stuck, the delete stuck.
    wipe_local_vault_cache(&alice).await;
    let listed = alice
        .invoke_json(
            "get_vault_messages",
            serde_json::json!({ "userId": alice_p.id }),
        )
        .await;
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 1, "the deleted entry must be gone from the remote");
    assert_eq!(listed[0]["content"].as_str().unwrap(), "draft two");

    // And search finds the edited text, not the original.
    let hits = alice
        .invoke_json(
            "search_vault_messages",
            serde_json::json!({ "query": "DRAFT", "userId": alice_p.id }),
        )
        .await;
    assert_eq!(hits.as_array().unwrap().len(), 1);
    let misses = alice
        .invoke_json(
            "search_vault_messages",
            serde_json::json!({ "query": "doomed", "userId": alice_p.id }),
        )
        .await;
    assert!(misses.as_array().unwrap().is_empty());

    drop(alice);
}

/// The vault is PERSONAL: another account neither reads nor writes it, and
/// its own vault stays empty.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn another_account_cannot_touch_the_vault() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let entry = alice
        .invoke_json(
            "send_vault_message",
            serde_json::json!({ "userId": alice_p.id, "content": "mine alone" }),
        )
        .await;
    let entry_id = entry["id"].as_str().unwrap().to_string();

    // Bob asking for ALICE's vault: the DS refuses the sync (signed identity
    // wins over the claimed user), and the command then serves BOB's local
    // cache — which is empty. Either surface is fine; alice's content must
    // not be.
    let stolen = bob
        .invoke_try(
            "get_vault_messages",
            serde_json::json!({ "userId": alice_p.id }),
        )
        .await;
    if let Ok(v) = &stolen {
        assert!(
            v.as_array().is_some_and(|a| a.is_empty()),
            "bob read alice's vault: {stolen:?}"
        );
    }

    // Bob deleting alice's entry by replaying its id is a silent no-op.
    let _ = bob
        .invoke_try(
            "delete_vault_message",
            serde_json::json!({ "id": entry_id, "userId": bob_p.id }),
        )
        .await;

    let listed = alice
        .invoke_json(
            "get_vault_messages",
            serde_json::json!({ "userId": alice_p.id }),
        )
        .await;
    assert_eq!(listed.as_array().unwrap().len(), 1, "alice's entry must survive");

    let bobs = bob
        .invoke_json(
            "get_vault_messages",
            serde_json::json!({ "userId": bob_p.id }),
        )
        .await;
    assert!(bobs.as_array().unwrap().is_empty());

    drop(alice);
    drop(bob);
}

/// A Vault file is collected when the last entry holding it goes (#1491) —
/// and not a moment before. Until #1491 deleting an entry released its DS
/// references and nothing ever collected the file: its `attachment_object`
/// row (and its stored object) stayed for the life of the account.
///
/// The flows world has no object storage, so the object row stands in for the
/// upload: it is seeded server-side exactly as `/v1/attachments/register`
/// leaves it, and the presigned DELETE the client then asks for fails
/// harmlessly (no broker). The live-storage half is the vault soak's
/// (`tests/vault_soak.rs`): object 404, rows gone, cache entry gone.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn deleting_the_last_entry_that_holds_a_file_collects_it() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;

    let file_id = "ab".repeat(32);
    let key = format!("media/{file_id}.enc");
    let remote = writable_remote().await;
    remote
        .conn()
        .await
        .expect("conn")
        .execute(
            "INSERT INTO attachment_object (content_hash, r2_key) VALUES (?1, ?2)",
            libsql::params![file_id.clone(), key.clone()],
        )
        .await
        .expect("seed attachment_object");
    let object_rows = || async {
        let conn = remote.conn().await.expect("conn");
        let mut rows = conn
            .query(
                "SELECT 1 FROM attachment_object WHERE content_hash = ?1",
                libsql::params![file_id.clone()],
            )
            .await
            .expect("query");
        let mut n = 0;
        while rows.next().await.expect("row").is_some() {
            n += 1;
        }
        n
    };

    let body = serde_json::json!({ "_att": [{
        "key": key, "name": "a.bin", "ct": "application/octet-stream", "size": 3,
        "hash": file_id, "kv": 0, "sha": "cd".repeat(32),
    }]})
    .to_string();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let e = alice
            .invoke_json(
                "send_vault_message",
                serde_json::json!({ "userId": alice_p.id, "content": body }),
            )
            .await;
        ids.push(e["id"].as_str().unwrap().to_string());
    }

    // Two entries hold the file; deleting one must leave it.
    alice
        .invoke_json(
            "delete_vault_message",
            serde_json::json!({ "id": ids[0], "userId": alice_p.id }),
        )
        .await;
    assert_eq!(object_rows().await, 1, "a file another entry still holds was collected");

    // An edit that drops it from the last entry releases it — collected.
    alice
        .invoke_json(
            "edit_vault_message",
            serde_json::json!({ "id": ids[1], "newContent": "just text now", "userId": alice_p.id }),
        )
        .await;
    assert_eq!(object_rows().await, 0, "the file outlived the last entry that held it");

    drop(alice);
}
