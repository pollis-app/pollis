//! The Vault (#107), end to end through the real command pipeline.
//!
//! The vault key derives from the account identity, so entries persist across devices.

use crate::harness::{wipe, writable_remote, TestClient};
use serial_test::serial;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Simulates a new device (harness devices share the local DB, so a real one would not).
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

    // A pin is indistinguishable from an edit on the wire.
    alice
        .invoke_json(
            "set_vault_message_pinned",
            serde_json::json!({ "id": entry_id, "pinned": true, "userId": alice_p.id }),
        )
        .await;

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

    // The DS refuses the sync and the command serves bob's own (empty) cache.
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

/// A Vault file is collected when its last entry goes (#1491); the live-storage half is `tests/vault_soak.rs`.
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

    alice
        .invoke_json(
            "delete_vault_message",
            serde_json::json!({ "id": ids[0], "userId": alice_p.id }),
        )
        .await;
    assert_eq!(object_rows().await, 1, "a file another entry still holds was collected");

    alice
        .invoke_json(
            "edit_vault_message",
            serde_json::json!({ "id": ids[1], "newContent": "just text now", "userId": alice_p.id }),
        )
        .await;
    assert_eq!(object_rows().await, 0, "the file outlived the last entry that held it");

    drop(alice);
}
