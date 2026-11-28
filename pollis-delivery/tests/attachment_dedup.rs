//! One stored object per distinct file, across every account (#2109).
//!
//! A shared object's id is `v2-` + the SHA-256 of its stored bytes. Senders
//! claim it before sending: the claim says whether the bytes still need
//! uploading and leases the object until the sender's reference lands. Every
//! message and Vault entry holding the file is a reference; the object goes
//! when the last one does, and never while another account still holds it.
//!
//! R2 is an in-memory fake that behaves like the presigned PUT the DS signs:
//! create-only, and only bytes whose SHA-256 is the id.

use std::collections::HashSet;
use std::sync::Mutex;

use libsql::Connection;
use pollis_api::messages::{
    shared_object_key, AttachmentClaimStatus, AttachmentRegisterBody, DeleteMessageBody,
};
use pollis_api::vault::{DeleteVaultMessageBody, SaveVaultMessageBody};
use pollis_api::messages::shared_object_put_headers;
use pollis_delivery::broker::ObjectStore;
use pollis_delivery::messages::{
    apply_claim_attachment, apply_delete_message, ensure_shared_object, object_is_referenced,
    reclaim_released_attachments, register_attachment_in, sweep_envelope_gc, SharedObject,
};
use pollis_delivery::writes::WriteOutcome;
use sha2::{Digest, Sha256};

const STALE: &str = pollis_delivery::messages::DEFAULT_WATERMARK_STALE_MODIFIER;

/// The ciphertext every sender of "the same file" produces: convergent
/// encryption makes it identical, so its id is too.
const BYTES: &[u8] = b"identical ciphertext of one file";

fn id_of(bytes: &[u8]) -> String {
    format!("v2-{}", hex::encode(Sha256::digest(bytes)))
}

fn file() -> String {
    id_of(BYTES)
}

/// An in-memory bucket with R2's create-only, checksummed PUT.
#[derive(Default)]
struct FakeR2 {
    objects: Mutex<HashSet<String>>,
}

impl FakeR2 {
    /// A PUT as the presigned URL allows it: `If-None-Match: *` and the id's checksum.
    fn put(&self, id: &str, bytes: &[u8]) -> Result<(), &'static str> {
        if id_of(bytes) != id {
            return Err("checksum mismatch");
        }
        let key = shared_object_key(id);
        let mut objects = self.objects.lock().unwrap();
        if !objects.insert(key) {
            return Err("412 precondition failed");
        }
        Ok(())
    }
    fn holds(&self, id: &str) -> bool {
        self.objects.lock().unwrap().contains(&shared_object_key(id))
    }
}

impl ObjectStore for FakeR2 {
    async fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        self.objects.lock().unwrap().remove(key);
        Ok(())
    }
    async fn object_exists(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.objects.lock().unwrap().contains(key))
    }
}

async fn conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
    pollis_schema::apply::single_db(&c).await.unwrap();
    c
}

async fn exec(c: &Connection, sql: &str) {
    c.execute(sql, ()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn scalar(c: &Connection, sql: &str) -> i64 {
    let mut rows = c.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

async fn users(c: &Connection, names: &[&str]) {
    for u in names {
        exec(c, &format!(
            "INSERT INTO users (id, email, username, account_id_pub) VALUES ('{u}', '{u}@x', '{u}', X'01')"
        ))
        .await;
    }
}

fn capability_hash(token: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(token.as_bytes()))
}

fn token_of(msg: &str) -> String {
    format!("tok-{msg}")
}

/// The client's upload of shared object `id`: claim, and PUT + register only
/// when the claim says nothing is stored. Returns what the claim answered.
async fn upload(c: &Connection, r2: &FakeR2, who: &str, id: &str, bytes: &[u8]) -> AttachmentClaimStatus {
    let status = apply_claim_attachment(c, who, id).await.unwrap();
    if status == AttachmentClaimStatus::Upload {
        // A racer's 412 means the bytes are there: register all the same.
        let _ = r2.put(id, bytes);
        let out = register_attachment_in(
            c,
            r2,
            Some(who),
            &AttachmentRegisterBody::ObjectOnly { content_hash: id.into(), r2_key: shared_object_key(id) },
        )
        .await
        .unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "register after upload: {out:?}");
    }
    status
}

/// A sealed message `msg` in `conv` by `who`, declaring `id`.
async fn send(c: &Connection, r2: &FakeR2, who: &str, msg: &str, conv: &str, id: &str) -> WriteOutcome {
    // `conv` is a DM `who` is in, so their delete is allowed.
    exec(c, &format!("INSERT OR IGNORE INTO conversation (id, kind) VALUES ('{conv}', 'dm')")).await;
    exec(c, &format!("INSERT OR IGNORE INTO dm_channel (id, created_by) VALUES ('{conv}', '{who}')")).await;
    exec(c, &format!(
        "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('{conv}', '{who}', '{who}')"
    ))
    .await;
    exec(c, &format!(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, sealed, delete_token_hash) \
         VALUES ('{msg}', '{conv}', 'sealed', 'ct', '2026-01-01', 1, '{}')",
        capability_hash(&token_of(msg))
    ))
    .await;
    register_attachment_in(
        c,
        r2,
        Some(who),
        &AttachmentRegisterBody::ForMessage {
            content_hash: id.into(),
            r2_key: shared_object_key(id),
            message_id: msg.into(),
            delete_token: Some(token_of(msg)),
        },
    )
    .await
    .unwrap()
}

async fn delete(c: &Connection, who: &str, msg: &str, conv: &str) {
    let out = apply_delete_message(
        c,
        Some(who),
        &DeleteMessageBody {
            message_id: msg.into(),
            conversation_id: conv.into(),
            msg_sender_id: Some(who.into()),
            actor_id: None,
            delete_token: Some(token_of(msg)),
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
}

async fn save_vault(c: &Connection, who: &str, entry: &str, ids: &[&str]) {
    use base64::Engine as _;
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let out = pollis_delivery::vault::apply_save_vault_message(
        c,
        Some(who),
        &SaveVaultMessageBody {
            id: entry.into(),
            user_id: who.into(),
            nonce: b64(&[0u8; 12]),
            blob: b64(&[1u8; 32]),
            created_at: "2026-01-01T00:00:00Z".into(),
            attachment_hashes: ids.iter().map(|h| h.to_string()).collect(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, pollis_delivery::vault::VaultOutcome::Ok));
}

async fn rows(c: &Connection, id: &str) -> i64 {
    scalar(c, &format!("SELECT COUNT(*) FROM attachment_object WHERE content_hash = '{id}'")).await
}

async fn refs(c: &Connection, id: &str) -> i64 {
    scalar(c, &format!("SELECT COUNT(*) FROM attachment_ref WHERE content_hash = '{id}'")).await
        + scalar(c, &format!("SELECT COUNT(*) FROM vault_attachment_ref WHERE content_hash = '{id}'")).await
}

async fn leases(c: &Connection, id: &str) -> i64 {
    scalar(c, &format!("SELECT COUNT(*) FROM attachment_lease WHERE content_hash = '{id}'")).await
}

async fn reclaim(c: &Connection, r2: &FakeR2) {
    reclaim_released_attachments(c, r2).await.unwrap();
}

async fn assert_kept(c: &Connection, r2: &FakeR2, id: &str, why: &str) {
    reclaim(c, r2).await;
    assert_eq!(rows(c, id).await, 1, "{why}: the row went");
    assert!(r2.holds(id), "{why}: the bytes went");
}

async fn assert_removed(c: &Connection, r2: &FakeR2, id: &str, why: &str) {
    reclaim(c, r2).await;
    assert_eq!(rows(c, id).await, 0, "{why}: the row outlived its last reference");
    assert!(!r2.holds(id), "{why}: the bytes outlived their last reference");
    assert_eq!(refs(c, id).await, 0, "{why}: references outlived the object");
}

// ---- one object, many references -------------------------------------------

#[tokio::test]
async fn the_same_file_from_two_users_is_one_object_and_two_references() {
    let c = conn().await;
    users(&c, &["alice", "bob"]).await;
    let r2 = FakeR2::default();
    let id = file();

    assert_eq!(upload(&c, &r2, "alice", &id, BYTES).await, AttachmentClaimStatus::Upload);
    assert_eq!(upload(&c, &r2, "bob", &id, BYTES).await, AttachmentClaimStatus::Stored, "bob uploads nothing");
    assert!(matches!(send(&c, &r2, "alice", "a1", "ca", &id).await, WriteOutcome::Ok));
    assert!(matches!(send(&c, &r2, "bob", "b1", "cb", &id).await, WriteOutcome::Ok));

    assert_eq!(rows(&c, &id).await, 1);
    assert_eq!(refs(&c, &id).await, 2);
    assert_eq!(leases(&c, &id).await, 0, "each landed reference settled its sender's claim");
}

#[tokio::test]
async fn deleting_one_reference_keeps_the_object_and_the_last_removes_it() {
    let c = conn().await;
    users(&c, &["alice", "bob"]).await;
    let r2 = FakeR2::default();
    let id = file();
    upload(&c, &r2, "alice", &id, BYTES).await;
    upload(&c, &r2, "bob", &id, BYTES).await;
    send(&c, &r2, "alice", "a1", "ca", &id).await;
    send(&c, &r2, "bob", "b1", "cb", &id).await;

    delete(&c, "alice", "a1", "ca").await;
    assert_kept(&c, &r2, &id, "bob's message still holds the file").await;
    delete(&c, "bob", "b1", "cb").await;
    assert_removed(&c, &r2, &id, "after the last message was deleted").await;
}

/// #2100's share-into-Pollis: one file into several chats and the Vault.
#[tokio::test]
async fn one_file_to_five_chats_and_the_vault_is_one_object() {
    let c = conn().await;
    users(&c, &["alice"]).await;
    let r2 = FakeR2::default();
    let id = file();
    let chats = ["c1", "c2", "c3", "c4", "c5"];
    for (n, chat) in chats.iter().enumerate() {
        let status = upload(&c, &r2, "alice", &id, BYTES).await;
        let expected = if n == 0 { AttachmentClaimStatus::Upload } else { AttachmentClaimStatus::Stored };
        assert_eq!(status, expected);
        assert!(matches!(send(&c, &r2, "alice", &format!("m{n}"), chat, &id).await, WriteOutcome::Ok));
    }
    assert_eq!(upload(&c, &r2, "alice", &id, BYTES).await, AttachmentClaimStatus::Stored);
    save_vault(&c, "alice", "v1", &[&id]).await;

    assert_eq!(rows(&c, &id).await, 1);
    assert_eq!(refs(&c, &id).await, 6);
    assert_eq!(leases(&c, &id).await, 0);

    for (n, chat) in chats.iter().enumerate() {
        delete(&c, "alice", &format!("m{n}"), chat).await;
    }
    assert_kept(&c, &r2, &id, "the Vault entry still holds it").await;
    pollis_delivery::vault::apply_delete_vault_message(
        &c,
        Some("alice"),
        &DeleteVaultMessageBody { id: "v1".into(), user_id: "alice".into() },
    )
    .await
    .unwrap();
    assert_removed(&c, &r2, &id, "after the last chat message and the Vault entry").await;
}

#[tokio::test]
async fn two_uploaders_racing_store_one_object() {
    let c = conn().await;
    users(&c, &["alice", "bob"]).await;
    let r2 = FakeR2::default();
    let id = file();
    // Both claim before either uploads: both are told to upload.
    assert_eq!(apply_claim_attachment(&c, "alice", &id).await.unwrap(), AttachmentClaimStatus::Upload);
    assert_eq!(apply_claim_attachment(&c, "bob", &id).await.unwrap(), AttachmentClaimStatus::Upload);
    assert!(r2.put(&id, BYTES).is_ok());
    assert_eq!(r2.put(&id, BYTES), Err("412 precondition failed"), "the second PUT cannot replace the first");
    for who in ["alice", "bob"] {
        let out = register_attachment_in(
            &c,
            &r2,
            Some(who),
            &AttachmentRegisterBody::ObjectOnly { content_hash: id.clone(), r2_key: shared_object_key(&id) },
        )
        .await
        .unwrap();
        assert!(matches!(out, WriteOutcome::Ok));
    }
    assert!(matches!(send(&c, &r2, "alice", "a1", "ca", &id).await, WriteOutcome::Ok));
    assert!(matches!(send(&c, &r2, "bob", "b1", "cb", &id).await, WriteOutcome::Ok));
    assert_eq!(rows(&c, &id).await, 1);
    assert_eq!(refs(&c, &id).await, 2);
}

// ---- the claim holds the file until the reference lands ---------------------

/// Bob is told the file is stored and skips the upload; before his message
/// lands, Alice deletes the only message holding it. The bytes must survive.
#[tokio::test]
async fn a_claim_holds_the_file_against_another_accounts_delete() {
    let c = conn().await;
    users(&c, &["alice", "bob"]).await;
    let r2 = FakeR2::default();
    let id = file();
    upload(&c, &r2, "alice", &id, BYTES).await;
    send(&c, &r2, "alice", "a1", "ca", &id).await;

    assert_eq!(upload(&c, &r2, "bob", &id, BYTES).await, AttachmentClaimStatus::Stored);
    delete(&c, "alice", "a1", "ca").await;
    assert_kept(&c, &r2, &id, "bob's claim holds it until his message lands").await;

    assert!(matches!(send(&c, &r2, "bob", "b1", "cb", &id).await, WriteOutcome::Ok));
    assert_eq!(leases(&c, &id).await, 0);
    delete(&c, "bob", "b1", "cb").await;
    assert_removed(&c, &r2, &id, "after bob's message, the last holder, was deleted").await;
}

/// A file a delete released but the DS has not yet removed is revived by a claim.
#[tokio::test]
async fn a_claim_revives_a_released_file() {
    let c = conn().await;
    users(&c, &["alice", "bob"]).await;
    let r2 = FakeR2::default();
    let id = file();
    upload(&c, &r2, "alice", &id, BYTES).await;
    send(&c, &r2, "alice", "a1", "ca", &id).await;
    delete(&c, "alice", "a1", "ca").await;
    assert_eq!(
        scalar(&c, &format!("SELECT COUNT(*) FROM attachment_object WHERE content_hash = '{id}' AND reclaim_state = 'released'")).await,
        1
    );
    assert_eq!(upload(&c, &r2, "bob", &id, BYTES).await, AttachmentClaimStatus::Stored);
    assert_kept(&c, &r2, &id, "the claim revived it").await;
}

#[tokio::test]
async fn a_claim_while_the_ds_deletes_the_file_is_told_to_wait() {
    let c = conn().await;
    users(&c, &["alice", "bob"]).await;
    let r2 = FakeR2::default();
    let id = file();
    upload(&c, &r2, "alice", &id, BYTES).await;
    exec(&c, &format!("UPDATE attachment_object SET reclaim_state = 'deleting' WHERE content_hash = '{id}'")).await;
    assert_eq!(apply_claim_attachment(&c, "bob", &id).await.unwrap(), AttachmentClaimStatus::Busy);
}

/// A claim whose message never comes holds the file for a bounded time only.
#[tokio::test]
async fn a_lapsed_claim_releases_the_file() {
    let c = conn().await;
    users(&c, &["alice"]).await;
    let r2 = FakeR2::default();
    let id = file();
    upload(&c, &r2, "alice", &id, BYTES).await;
    assert!(object_is_referenced(&c, &id).await.unwrap(), "a fresh claim holds the file");

    exec(&c, "UPDATE attachment_lease SET updated_at = datetime('now', '-31 days')").await;
    sweep_envelope_gc(&c, STALE).await.unwrap();
    assert_eq!(leases(&c, &id).await, 0);
    assert_removed(&c, &r2, &id, "after its only claim lapsed").await;
}

/// The same sender with the file pending in two messages: the first landing
/// must not drop the hold the second still needs.
#[tokio::test]
async fn one_senders_two_pending_sends_each_keep_the_file() {
    let c = conn().await;
    users(&c, &["alice", "bob"]).await;
    let r2 = FakeR2::default();
    let id = file();
    upload(&c, &r2, "bob", &id, BYTES).await;
    send(&c, &r2, "bob", "b1", "cb", &id).await;
    upload(&c, &r2, "alice", &id, BYTES).await;
    upload(&c, &r2, "alice", &id, BYTES).await;
    send(&c, &r2, "alice", "a1", "ca", &id).await;
    // Bob and Alice's first message go; Alice's second is still sending.
    delete(&c, "bob", "b1", "cb").await;
    delete(&c, "alice", "a1", "ca").await;
    assert_kept(&c, &r2, &id, "alice's second send still holds it").await;
    assert!(matches!(send(&c, &r2, "alice", "a2", "ca", &id).await, WriteOutcome::Ok));
    assert_eq!(leases(&c, &id).await, 0);
}

// ---- no row without its bytes, no bytes but the right ones ------------------

#[tokio::test]
async fn a_reference_to_a_file_that_was_never_stored_writes_nothing() {
    let c = conn().await;
    users(&c, &["mallory"]).await;
    let r2 = FakeR2::default();
    let id = file();
    let out = register_attachment_in(
        &c,
        &r2,
        Some("mallory"),
        &AttachmentRegisterBody::ObjectOnly { content_hash: id.clone(), r2_key: "media/elsewhere.enc".into() },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Invalid(_)), "{out:?}");
    assert_eq!(rows(&c, &id).await, 0, "no row may name bytes that are not stored");
    assert!(matches!(send(&c, &r2, "mallory", "m1", "cm", &id).await, WriteOutcome::Invalid(_)));
    assert_eq!(rows(&c, &id).await, 0);
    // So the real upload is still asked for.
    assert_eq!(apply_claim_attachment(&c, "alice", &id).await.unwrap(), AttachmentClaimStatus::Upload);
}

#[tokio::test]
async fn bytes_that_do_not_hash_to_the_id_are_never_stored() {
    let r2 = FakeR2::default();
    assert_eq!(r2.put(&file(), b"something else"), Err("checksum mismatch"));
    // The presign signs exactly that checksum and create-only.
    let headers = shared_object_put_headers(&file());
    use base64::Engine as _;
    let want = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(BYTES));
    assert!(headers.contains(&("x-amz-checksum-sha256", want)));
    assert!(headers.contains(&("if-none-match", "*".to_string())));
    assert!(shared_object_put_headers("1f1e1d1c1b1a19181716151413121110f0e0d0c0b0a090807060504030201000").is_empty());
}

#[tokio::test]
async fn the_schema_refuses_a_shared_object_row_naming_another_key() {
    let c = conn().await;
    let id = file();
    let wrong = c
        .execute(
            "INSERT INTO attachment_object (content_hash, r2_key) VALUES (?1, 'media/other.enc')",
            libsql::params![id.clone()],
        )
        .await;
    assert!(wrong.is_err(), "a shared object row must name media/<its id>.enc");
    c.execute(
        "INSERT INTO attachment_object (content_hash, r2_key) VALUES (?1, ?2)",
        libsql::params![id.clone(), shared_object_key(&id)],
    )
    .await
    .unwrap();
    let moved = c
        .execute(
            "UPDATE attachment_object SET r2_key = 'media/other.enc' WHERE content_hash = ?1",
            libsql::params![id.clone()],
        )
        .await;
    assert!(moved.is_err());
    let lease = c
        .execute("INSERT INTO attachment_lease (content_hash, user_id) VALUES ('deadbeef', 'u')", ())
        .await;
    assert!(lease.is_err(), "a lease names only a shared object");
}

/// The bucket lost the bytes between the first look and the row: the row goes.
#[tokio::test]
async fn a_row_is_taken_back_if_its_bytes_vanish_while_it_is_written() {
    struct Flaky(Mutex<u32>);
    impl ObjectStore for Flaky {
        async fn delete_object(&self, _key: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn object_exists(&self, _key: &str) -> anyhow::Result<bool> {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            Ok(*n == 1)
        }
    }
    let c = conn().await;
    let id = file();
    let out = ensure_shared_object(&c, &Flaky(Mutex::new(0)), &id).await.unwrap();
    assert_eq!(out, SharedObject::Missing);
    assert_eq!(rows(&c, &id).await, 0);
}

/// v1 objects (random file ids) are untouched by any of this.
#[tokio::test]
async fn a_v1_object_registers_as_before() {
    let c = conn().await;
    let r2 = FakeR2::default();
    let v1 = "1f1e1d1c1b1a19181716151413121110f0e0d0c0b0a090807060504030201000";
    let out = register_attachment_in(
        &c,
        &r2,
        Some("alice"),
        &AttachmentRegisterBody::ObjectOnly { content_hash: v1.into(), r2_key: format!("media/{v1}.enc") },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
    assert_eq!(rows(&c, v1).await, 1);
}

// ---- operator removal of a stored file ----------------------------------------

#[tokio::test]
async fn operator_removal_takes_the_file_from_every_holder_and_sticks() {
    use pollis_delivery::messages::{object_is_removed, remove_stored_object};
    let c = conn().await;
    users(&c, &["alice", "bob"]).await;
    let r2 = FakeR2::default();
    let id = file();
    upload(&c, &r2, "alice", &id, BYTES).await;
    send(&c, &r2, "alice", "a1", "ca", &id).await;
    upload(&c, &r2, "bob", &id, BYTES).await;
    send(&c, &r2, "bob", "b1", "cb", &id).await;
    save_vault(&c, "bob", "v1", &[&id]).await;
    // Someone mid-send holds a lease.
    apply_claim_attachment(&c, "carol", &id).await.unwrap();

    let report = remove_stored_object(&c, &r2, &id).await.unwrap();
    assert_eq!(report.references, 3);
    assert!(!r2.holds(&id), "the bytes are gone");
    assert_eq!(rows(&c, &id).await, 0);
    assert_eq!(refs(&c, &id).await, 0);
    assert_eq!(leases(&c, &id).await, 0);
    assert!(object_is_removed(&c, &id).await.unwrap());

    // Sending the identical file again cannot bring it back.
    assert_eq!(apply_claim_attachment(&c, "dave", &id).await.unwrap(), AttachmentClaimStatus::Removed);
    r2.put(&id, BYTES).unwrap();
    let out = register_attachment_in(
        &c,
        &r2,
        Some("dave"),
        &AttachmentRegisterBody::ObjectOnly { content_hash: id.clone(), r2_key: shared_object_key(&id) },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Invalid(_)), "{out:?}");
    assert!(matches!(send(&c, &r2, "dave", "d1", "cd", &id).await, WriteOutcome::Invalid(_)));
    assert_eq!(rows(&c, &id).await, 0, "no row comes back");
    // A Vault edit of an entry that held it still saves, without it.
    save_vault(&c, "bob", "v1", &[&id]).await;
    assert_eq!(refs(&c, &id).await, 0);
}

#[tokio::test]
async fn operator_removal_of_a_v1_file_deletes_only_its_own_key() {
    use pollis_delivery::messages::remove_stored_object;
    let c = conn().await;
    let r2 = FakeR2::default();
    let v1 = "1f1e1d1c1b1a19181716151413121110f0e0d0c0b0a090807060504030201000";
    let key = format!("media/{v1}.enc");
    r2.objects.lock().unwrap().insert(key.clone());
    r2.objects.lock().unwrap().insert("media/unrelated.enc".into());
    exec(&c, &format!("INSERT INTO attachment_object (content_hash, r2_key) VALUES ('{v1}', 'emoji/elsewhere.webp')")).await;
    remove_stored_object(&c, &r2, v1).await.unwrap();
    assert!(!r2.objects.lock().unwrap().contains(&key));
    assert!(r2.objects.lock().unwrap().contains("media/unrelated.enc"));
    assert_eq!(rows(&c, v1).await, 0);
}
