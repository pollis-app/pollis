//! #2109: each distinct file is stored once, across every account, through the real
//! pipeline — the outbox's upload, the DS claim/presign/register, an in-process R2
//! (`fake_r2`), the reference declarations and the DS's reclaim.
//!
//! Every message or Vault entry carrying the file is a reference; the object goes with
//! the last one, and never while someone else still holds it. v1 objects (random id,
//! conversation key ring, #1382) keep downloading. Nothing the DS or the bucket holds
//! opens a file.

use crate::harness::{set_shared_attachments, wipe, writable_remote, TestClient};
use pollis_core::commands::r2::{download_media, AttachmentKeyScope};
use serde_json::json;
use serial_test::serial;

const PHOTO: &[u8] = b"\x89PNG the same photo, byte for byte, from everyone";

/// Alice and Bob in one group channel, plus a DM between them, with the DS's
/// shared-upload switch ON (the state after the follow-up flips it).
async fn world() -> (TestClient, TestClient, String, String) {
    let w = world_switch_off().await;
    set_shared_attachments(true);
    w
}

/// [`world`] with the switch as production ships it: off, so uploads are v1.
async fn world_switch_off() -> (TestClient, TestClient, String, String) {
    wipe().await;
    let mut alice = TestClient::new().await;
    let a = alice.sign_up("dedup-alice@test.local").await;
    let mut bob = TestClient::new().await;
    let b = bob.sign_up("dedup-bob@test.local").await;

    let group_id = alice.create_group("Dedup").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    alice.invite(&group_id, &b.username).await;
    let invite = bob.first_pending_invite().await.expect("bob has an invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;

    let dm_id = alice.create_dm(&[a.id.as_str(), b.id.as_str()]).await;
    bob.accept_dm_request(&dm_id).await;
    bob.poll().await;
    bob.process_commits_for(&dm_id).await;
    alice.process_commits_for(&dm_id).await;
    (alice, bob, channel_id, dm_id)
}

/// Send `bytes` as a photo to `conversation`; returns the message id and its `_att` entry.
async fn send_photo(client: &TestClient, conversation: &str, bytes: &[u8]) -> (String, serde_json::Value) {
    let staged = pollis_core::commands::staging::stage_attachment(bytes.to_vec())
        .expect("stage")
        .id;
    let id = client.invoke_json("new_message_id", json!({})).await.as_str().expect("id").to_string();
    client
        .invoke_try(
            "send_message",
            json!({
                "conversationId": conversation,
                "senderId": client.user_id(),
                "content": "",
                "replyToId": null,
                "senderUsername": null,
                "messageId": id,
                "attachments": [{ "stagedId": staged, "filename": "photo.png", "contentType": "image/png" }],
            }),
        )
        .await
        .expect("the photo sends");
    let page = client
        .invoke_json("read_channel_messages", json!({ "channelId": conversation, "limit": 50 }))
        .await;
    let row = page["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["id"] == id.as_str())
        .cloned()
        .expect("own row");
    assert!(row.get("send_state").is_none(), "delivered: {row}");
    let content: serde_json::Value =
        serde_json::from_str(row["content"].as_str().expect("content")).expect("_att envelope");
    (id, content["_att"][0].clone())
}

fn media_keys() -> Vec<String> {
    crate::fake_r2::keys().into_iter().filter(|k| k.starts_with("media/")).collect()
}

async fn count(sql: &str) -> i64 {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let mut rows = conn.query(sql, ()).await.expect("query");
    rows.next().await.expect("row").expect("one row").get::<i64>(0).expect("count")
}

/// Wait (bounded) for the DS's background reclaim to settle on `want` stored objects.
async fn settle_on(want: usize) {
    for _ in 0..100 {
        if media_keys().len() == want {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(media_keys().len(), want, "stored objects: {:?}", media_keys());
}

async fn open(client: &TestClient, att: &serde_json::Value) -> Result<Vec<u8>, pollis_core::error::Error> {
    download_media(
        att["key"].as_str().expect("key").to_string(),
        att["hash"].as_str().expect("hash").to_string(),
        att["sha"].as_str().expect("sha").to_string(),
        AttachmentKeyScope::Content,
        &client.state,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_same_file_from_two_users_is_one_object_and_two_references() {
    let (alice, bob, channel_id, dm_id) = world().await;

    let (_, a_att) = send_photo(&alice, &channel_id, PHOTO).await;
    let (_, b_att) = send_photo(&bob, &dm_id, PHOTO).await;

    assert_eq!(a_att["v"], 2, "a new upload is a shared object: {a_att}");
    assert!(a_att.get("kv").is_none(), "a v2 reference names no ring key: {a_att}");
    assert_eq!(a_att["hash"], b_att["hash"], "the same file is the same object");
    let id = a_att["hash"].as_str().unwrap();
    assert!(id.starts_with("v2-"));
    assert_eq!(media_keys(), vec![format!("media/{id}.enc")], "stored once");
    assert_eq!(count(&format!("SELECT COUNT(*) FROM attachment_object WHERE content_hash = '{id}'")).await, 1);
    assert_eq!(count(&format!("SELECT COUNT(*) FROM attachment_ref WHERE content_hash = '{id}'")).await, 2);
    assert_eq!(count("SELECT COUNT(*) FROM attachment_lease").await, 0, "every claim settled");

    // Each opens the other's copy: the key rides in the reference, not in a ring.
    assert_eq!(open(&bob, &a_att).await.expect("bob opens alice's"), PHOTO);
    assert_eq!(open(&alice, &b_att).await.expect("alice opens bob's"), PHOTO);
    drop((alice, bob));
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn deleting_one_reference_keeps_the_object_and_the_last_removes_it() {
    let (alice, bob, channel_id, dm_id) = world().await;
    let (a_msg, a_att) = send_photo(&alice, &channel_id, PHOTO).await;
    let (b_msg, _) = send_photo(&bob, &dm_id, PHOTO).await;
    let id = a_att["hash"].as_str().unwrap().to_string();

    alice.delete_message(&a_msg).await;
    settle_on(1).await;
    assert_eq!(open(&bob, &a_att).await.expect("bob's message still holds it"), PHOTO);

    bob.delete_message(&b_msg).await;
    settle_on(0).await;
    assert_eq!(count(&format!("SELECT COUNT(*) FROM attachment_object WHERE content_hash = '{id}'")).await, 0);
    assert_eq!(count(&format!("SELECT COUNT(*) FROM attachment_ref WHERE content_hash = '{id}'")).await, 0);
    drop((alice, bob));
}

/// #2100's share into several chats, plus the Vault: one object, one reference each.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn one_file_to_several_chats_and_the_vault_is_one_object() {
    let (alice, bob, channel_id, dm_id) = world().await;
    let group2 = alice.create_group("Second").await;
    let channel2 = alice.general_channel_id(&group2).await;

    let mut ids = Vec::new();
    for conversation in [&channel_id, &dm_id, &channel2] {
        ids.push(send_photo(&alice, conversation, PHOTO).await.1["hash"].clone());
    }
    // The Vault's queue uploads it (#2100), as the outbox does a chat's.
    let staged = pollis_core::commands::staging::stage_attachment(PHOTO.to_vec()).expect("stage").id;
    let saved = alice
        .invoke_json(
            "send_vault_message",
            json!({
                "userId": alice.user_id(),
                "content": "",
                "attachments": [{ "stagedId": staged, "filename": "photo.png", "contentType": "image/png" }],
            }),
        )
        .await;
    let saved: serde_json::Value = serde_json::from_str(saved["content"].as_str().expect("content")).expect("envelope");
    assert_eq!(saved["_att"][0]["v"], 2, "a shared object in the Vault too");
    ids.push(saved["_att"][0]["hash"].clone());
    assert!(ids.iter().all(|h| *h == ids[0]), "{ids:?}");
    let id = ids[0].as_str().expect("id").to_string();

    assert_eq!(media_keys().len(), 1, "stored once: {:?}", media_keys());
    assert_eq!(count(&format!("SELECT COUNT(*) FROM attachment_ref WHERE content_hash = '{id}'")).await, 3);
    assert_eq!(count(&format!("SELECT COUNT(*) FROM vault_attachment_ref WHERE content_hash = '{id}'")).await, 1);
    assert_eq!(count("SELECT COUNT(*) FROM attachment_lease").await, 0);
    drop((alice, bob));
}

/// Opens `att` the way a build without v2 support does: only a `kv` reference, under the ring.
async fn open_as_v1_reader(client: &TestClient, conversation: &str, att: &serde_json::Value) -> Vec<u8> {
    let kv = att["kv"].as_i64().expect("an old-format reader needs kv");
    download_media(
        att["key"].as_str().unwrap().to_string(),
        att["hash"].as_str().unwrap().to_string(),
        att["sha"].as_str().unwrap().to_string(),
        AttachmentKeyScope::Conversation { conversation_id: conversation.to_string(), key_version: kv },
        &client.state,
    )
    .await
    .expect("the old-format reader opens it under the ring")
}

/// The switch OFF (as shipped): uploads are v1, exactly what every build in the
/// stores can open, and no shared object is stored. The same file twice is two
/// objects, as before #2109. Flipping the switch on later leaves them opening.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn with_the_switch_off_uploads_stay_v1_for_old_readers() {
    let (alice, bob, channel_id, _) = world_switch_off().await;
    let (_, a_att) = send_photo(&alice, &channel_id, PHOTO).await;
    let (_, b_att) = send_photo(&bob, &channel_id, PHOTO).await;
    for att in [&a_att, &b_att] {
        assert!(att.get("v").is_none(), "no v2 reference while the switch is off: {att}");
        assert!(att["kv"].as_i64().unwrap_or(0) >= 1, "a ring key version: {att}");
        assert_eq!(att["hash"].as_str().unwrap().len(), 64, "a random v1 file id: {att}");
    }
    assert_ne!(a_att["hash"], b_att["hash"], "v1 is not deduplicated");
    assert_eq!(media_keys().len(), 2);
    assert_eq!(count("SELECT COUNT(*) FROM attachment_lease").await, 0, "no claims while off");
    assert!(media_keys().iter().all(|k| !k.starts_with("media/v2-")));

    // A reader that only understands `kv` (a build in the stores today) opens both.
    alice.process_commits_for(&channel_id).await;
    assert_eq!(open_as_v1_reader(&bob, &channel_id, &a_att).await, PHOTO);
    assert_eq!(open_as_v1_reader(&alice, &channel_id, &b_att).await, PHOTO);

    // The switch flips: new uploads are shared, and the v1 files still open.
    set_shared_attachments(true);
    let (_, c_att) = send_photo(&alice, &channel_id, PHOTO).await;
    assert_eq!(c_att["v"], 2);
    assert_eq!(open(&bob, &c_att).await.expect("v2 opens"), PHOTO);
    assert_eq!(open_as_v1_reader(&bob, &channel_id, &a_att).await, PHOTO);
    drop((alice, bob));
}

/// The client asks the DS before every upload, so the switch takes effect at once.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_client_follows_the_ds_switch() {
    let (alice, bob, _, _) = world_switch_off().await;
    let ask = || pollis_core::commands::r2::shared_uploads_enabled(&alice.state);
    assert!(!ask().await.expect("ask"), "off as shipped");
    set_shared_attachments(true);
    assert!(ask().await.expect("ask"), "on once flipped");
    set_shared_attachments(false);
    assert!(!ask().await.expect("ask"), "and off again");
    drop((alice, bob));
}

/// The #1382 property, kept: the bucket plus everything the DS stores opens nothing.
/// Neither the file key nor the plaintext digest it comes from appears in any server-side
/// row or stored object, and the stored object does not open under keys computed from them.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_bucket_and_the_ds_database_alone_cannot_open_a_file() {
    let (alice, bob, channel_id, _) = world().await;
    let (_, att) = send_photo(&alice, &channel_id, PHOTO).await;
    let id = att["hash"].as_str().unwrap().to_string();
    let sha = att["sha"].as_str().unwrap().to_string();
    let h = hex::decode(&sha).unwrap();

    // Everything the bucket holds.
    let mut server_bytes: Vec<Vec<u8>> = crate::fake_r2::keys()
        .into_iter()
        .flat_map(|k| [k.clone().into_bytes(), crate::fake_r2::get(&k).unwrap()])
        .collect();
    // Every cell of every table the DS stores.
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let mut tables = Vec::new();
    let mut rows = conn
        .query("SELECT name FROM sqlite_master WHERE type = 'table'", ())
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        tables.push(row.get::<String>(0).unwrap());
    }
    for table in tables {
        let mut rows = conn.query(&format!("SELECT * FROM \"{table}\""), ()).await.unwrap();
        while let Some(row) = rows.next().await.unwrap() {
            for i in 0..row.column_count() {
                match row.get_value(i).unwrap() {
                    libsql::Value::Text(s) => server_bytes.push(s.into_bytes()),
                    libsql::Value::Blob(b) => server_bytes.push(b),
                    _ => {}
                }
            }
        }
    }
    let contains = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    for blob in &server_bytes {
        assert!(!contains(blob, &h), "the plaintext digest is stored server-side");
        assert!(!contains(blob, sha.as_bytes()), "the plaintext digest (hex) is stored server-side");
        assert!(!contains(blob, PHOTO), "the plaintext is stored server-side");
    }

    // The stored object does not open under the id or anything else the server holds.
    let stored = crate::fake_r2::get(&format!("media/{id}.enc")).expect("stored");
    let digest = pollis_api::messages::shared_object_digest(&id).unwrap();
    let wrong = download_media(
        format!("media/{id}.enc"),
        id.clone(),
        hex::encode(digest),
        AttachmentKeyScope::Content,
        &bob.state,
    )
    .await;
    assert!(wrong.is_err(), "the object id is not a key");
    assert!(stored.len() > PHOTO.len());
    // Only the reference, which travels end to end encrypted, opens it.
    assert_eq!(open(&bob, &att).await.expect("opens with the reference"), PHOTO);
    drop((alice, bob));
}

/// Operator removal (#2109): the stored file goes for everyone, every client says so,
/// and sending the identical file again cannot bring it back.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_operator_removed_file_stays_removed() {
    let (alice, bob, channel_id, dm_id) = world().await;
    let (_, att) = send_photo(&alice, &channel_id, PHOTO).await;
    let id = att["hash"].as_str().unwrap().to_string();

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    pollis_delivery::messages::remove_stored_object(&conn, &OpsBucket, &id)
        .await
        .expect("removal");
    assert!(media_keys().is_empty(), "the bytes are gone");

    let err = open(&bob, &att).await.expect_err("a removed file does not open");
    assert!(matches!(err, pollis_core::error::Error::AttachmentRemoved), "{err}");
    assert_eq!(err.to_string(), "attachment_removed", "the renderers match this exact text");

    // The same file again is refused, not silently restored.
    let staged = pollis_core::commands::staging::stage_attachment(PHOTO.to_vec()).expect("stage").id;
    let id2 = bob.invoke_json("new_message_id", json!({})).await.as_str().unwrap().to_string();
    let _ = bob
        .invoke_try(
            "send_message",
            json!({
                "conversationId": dm_id, "senderId": bob.user_id(), "content": "again",
                "replyToId": null, "senderUsername": null, "messageId": id2,
                "attachments": [{ "stagedId": staged, "filename": "photo.png", "contentType": "image/png" }],
            }),
        )
        .await;
    assert!(media_keys().is_empty(), "the removed file was stored again");
    assert_eq!(count(&format!("SELECT COUNT(*) FROM attachment_object WHERE content_hash = '{id}'")).await, 0);
    drop((alice, bob));
}

/// The DS's own bucket handle, for the removal: it deletes from the flows' R2.
struct OpsBucket;

impl pollis_delivery::broker::ObjectStore for OpsBucket {
    async fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        crate::fake_r2::delete(key);
        Ok(())
    }
    async fn object_exists(&self, key: &str) -> anyhow::Result<bool> {
        Ok(crate::fake_r2::get(key).is_some())
    }
}

/// Two senders uploading the same file at the same moment store one object.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn two_senders_racing_the_same_file_store_one_object() {
    let (alice, bob, channel_id, dm_id) = world().await;
    let ((_, a_att), (_, b_att)) = tokio::join!(
        send_photo(&alice, &channel_id, PHOTO),
        send_photo(&bob, &dm_id, PHOTO),
    );
    assert_eq!(a_att["hash"], b_att["hash"]);
    assert_eq!(media_keys().len(), 1, "{:?}", media_keys());
    let id = a_att["hash"].as_str().unwrap();
    assert_eq!(count(&format!("SELECT COUNT(*) FROM attachment_object WHERE content_hash = '{id}'")).await, 1);
    assert_eq!(count(&format!("SELECT COUNT(*) FROM attachment_ref WHERE content_hash = '{id}'")).await, 2);
    assert_eq!(open(&bob, &a_att).await.expect("opens"), PHOTO);
    drop((alice, bob));
}
