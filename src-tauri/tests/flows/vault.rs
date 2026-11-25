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
    // Live rows only: a released file waits for the DS to delete its R2
    // object, and this harness has no R2 (#1881).
    let object_rows = || async {
        let conn = remote.conn().await.expect("conn");
        let mut rows = conn
            .query(
                "SELECT 1 FROM attachment_object WHERE content_hash = ?1 AND reclaim_state IS NULL",
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

// The Vault's durable queue (#2100): a save behaves like a message through the outbox.

async fn remote_vault_rows(id: &str) -> i64 {
    let conn = writable_remote().await.conn().await.expect("conn");
    let mut rows = conn
        .query("SELECT COUNT(*) FROM vault_message WHERE id = ?1", libsql::params![id.to_string()])
        .await
        .expect("count");
    rows.next().await.expect("row").expect("a row").get(0).expect("count")
}

async fn entries(client: &TestClient) -> Vec<serde_json::Value> {
    client
        .invoke_json("get_vault_messages", serde_json::json!({ "userId": client.user_id() }))
        .await
        .as_array()
        .expect("entries")
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_double_submitted_vault_save_saves_once() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let p = alice.sign_up("vault-twice@test.local").await;
    let id = alice.invoke_json("new_message_id", serde_json::json!({})).await;
    let id = id.as_str().expect("id").to_string();
    let args = serde_json::json!({ "userId": p.id, "content": "only once", "messageId": id });
    let (a, b) = tokio::join!(
        alice.invoke_json("send_vault_message", args.clone()),
        alice.invoke_json("send_vault_message", args.clone()),
    );
    assert_eq!(a["id"], id.as_str());
    assert_eq!(b["id"], id.as_str());
    // And again after it landed.
    alice.invoke_json("send_vault_message", args).await;
    let all = entries(&alice).await;
    assert_eq!(all.iter().filter(|e| e["content"] == "only once").count(), 1, "{all:?}");
    assert!(all.iter().all(|e| e.get("send_state").is_none()), "nothing left queued: {all:?}");
    assert_eq!(remote_vault_rows(&id).await, 1);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_interrupted_vault_save_is_finished_by_the_app_once() {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("vault-interrupted@test.local").await;
    alice.activate();
    // Stored, then the process stops before any network work.
    let queued = pollis_core::commands::messages::vault_queue::enqueue(
        &alice.state,
        None,
        "left behind".into(),
        Vec::new(),
    )
    .await
    .expect("enqueue");
    let pollis_core::commands::messages::vault_queue::Enqueued::Unsent(id) = queued else {
        panic!("a fresh save is unsent");
    };
    let shown = entries(&alice).await;
    let row = shown.iter().find(|e| e["id"] == id.as_str()).expect("the Vault shows the queued save");
    assert_eq!(row["send_state"], "pending");
    assert_eq!(remote_vault_rows(&id).await, 0);

    // The app's drain (unlock, foreground, reconnect) finishes it, once.
    alice.flush_outbox().await;
    alice.flush_outbox().await;
    assert_eq!(remote_vault_rows(&id).await, 1);
    let shown = entries(&alice).await;
    let row = shown.iter().find(|e| e["id"] == id.as_str()).expect("saved");
    assert!(row.get("send_state").is_none(), "{row}");
    assert_eq!(shown.iter().filter(|e| e["content"] == "left behind").count(), 1);

    // A new device reads it from the server.
    wipe_local_vault_cache(&alice).await;
    let rebuilt = entries(&alice).await;
    assert_eq!(rebuilt.iter().filter(|e| e["content"] == "left behind").count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_vault_file_goes_through_the_queue_under_the_vault_key() {
    pollis_core::commands::messages::test_hooks::stub_attachment_uploads(true);
    wipe().await;
    let mut alice = TestClient::new().await;
    let p = alice.sign_up("vault-file@test.local").await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.pdf");
    let bytes: Vec<u8> = (0..(5 * 1024 * 1024 + 3)).map(|i| (i % 241) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    let saved = alice
        .invoke_try(
            "send_vault_message",
            serde_json::json!({
                "userId": p.id,
                "content": "the contract",
                "attachments": [{ "path": path.display().to_string(), "filename": "note.pdf", "contentType": "application/pdf" }],
            }),
        )
        .await;
    pollis_core::commands::messages::test_hooks::stub_attachment_uploads(false);
    let saved = saved.expect("saved");
    let content: serde_json::Value = serde_json::from_str(saved["content"].as_str().unwrap()).unwrap();
    assert_eq!(content["_txt"], "the contract");
    let att = &content["_att"][0];
    assert_eq!(att["size"], bytes.len());
    assert_eq!(att["kv"], 0, "sealed under the Vault's file key");
    use sha2::Digest;
    assert_eq!(att["sha"], hex::encode(sha2::Sha256::digest(&bytes)).as_str());
    // The media view reads the same envelope from the list.
    let all = entries(&alice).await;
    assert_eq!(all.iter().filter(|e| e["id"] == saved["id"]).count(), 1);
    // The file's bytes left the queue with it.
    let left: i64 = {
        let guard = alice.state.local_db.lock().await;
        guard
            .as_ref()
            .unwrap()
            .conn()
            .query_row("SELECT COUNT(*) FROM outbox_attachment", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(left, 0);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_queued_vault_save_can_be_discarded() {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("vault-discard@test.local").await;
    alice.activate();
    let pollis_core::commands::messages::vault_queue::Enqueued::Unsent(id) =
        pollis_core::commands::messages::vault_queue::enqueue(&alice.state, None, "never mind".into(), Vec::new())
            .await
            .expect("enqueue")
    else {
        panic!("unsent");
    };
    let gone = alice
        .invoke_json("discard_unsent_vault_message", serde_json::json!({ "id": id }))
        .await;
    assert_eq!(gone, true);
    alice.flush_outbox().await;
    assert_eq!(remote_vault_rows(&id).await, 0);
    assert!(entries(&alice).await.iter().all(|e| e["id"] != id.as_str()));
}
