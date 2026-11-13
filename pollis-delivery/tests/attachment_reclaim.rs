//! A file leaves R2 and Turso only once every message referring to it has been
//! DELETED — gone from everyone's history — never because its envelope was
//! collected (#1881).
//!
//! Envelope GC, the #720 staleness window, account deletion (including the
//! sealed-envelope withdraw) and group / channel / DM teardown remove envelopes,
//! but every member keeps those messages on their devices and fetches their
//! files from R2 lazily, so none of them may release a file. An explicit delete
//! (the author's, or an admin's tombstone) removes the message for everyone and
//! releases its reference; the DS then deletes the object from R2 and the row.
//!
//! Most tests share ONE file between two users' messages. R2 is an in-memory fake.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use libsql::Connection;
use pollis_api::account::{DeleteAccountBody, WithdrawEnvelope};
use pollis_api::groups::{DeleteChannelBody, DeleteGroupBody};
use pollis_api::messages::{AttachmentDeleteBody, AttachmentRegisterBody, DeleteMessageBody};
use pollis_api::profile::LeaveDmBody;
use pollis_api::vault::{DeleteVaultMessageBody, SaveVaultMessageBody};
use pollis_delivery::broker::ObjectStore;
use pollis_delivery::messages::{
    apply_delete_attachment, apply_delete_message, apply_register_attachment,
    object_is_referenced, reclaim_released_attachments, sweep_envelope_gc, ReclaimReport,
};
use pollis_delivery::writes::WriteOutcome;

/// The file both users' messages carry: a random id, as `upload_media` mints.
const FILE: &str = "1f1e1d1c1b1a19181716151413121110f0e0d0c0b0a090807060504030201000";
/// The DS's default staleness window.
const STALE: &str = pollis_delivery::messages::DEFAULT_WATERMARK_STALE_MODIFIER;

fn key() -> String {
    format!("media/{FILE}.enc")
}

/// An in-memory bucket. `fail` makes every delete error, as an R2 outage would.
#[derive(Default)]
struct FakeR2 {
    objects: Mutex<HashSet<String>>,
    deletes: Mutex<Vec<String>>,
    fail: AtomicBool,
}

impl FakeR2 {
    fn with(key: &str) -> Self {
        let r2 = Self::default();
        r2.objects.lock().unwrap().insert(key.to_string());
        r2
    }
    fn holds(&self, key: &str) -> bool {
        self.objects.lock().unwrap().contains(key)
    }
    fn delete_calls(&self) -> usize {
        self.deletes.lock().unwrap().len()
    }
}

impl ObjectStore for FakeR2 {
    async fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        self.deletes.lock().unwrap().push(key.to_string());
        if self.fail.load(Ordering::SeqCst) {
            anyhow::bail!("r2 unavailable");
        }
        self.objects.lock().unwrap().remove(key);
        Ok(())
    }
}

async fn conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    // Production parity: no cascades.
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

/// A user with one device.
async fn user(c: &Connection, u: &str) {
    exec(c, &format!(
        "INSERT INTO users (id, email, username, account_id_pub) VALUES ('{u}', '{u}@x', '{u}', X'01')"
    )).await;
    exec(c, &format!("INSERT INTO user_device (device_id, user_id) VALUES ('{u}_dev', '{u}')")).await;
}

/// Group `g` with channel `ch`; the first member is its admin.
async fn group(c: &Connection, g: &str, ch: &str, members: &[&str]) {
    exec(c, &format!("INSERT INTO conversation (id, kind) VALUES ('{g}', 'group')")).await;
    exec(c, &format!("INSERT INTO groups (id, name, owner_id) VALUES ('{g}', 'G', '{}')", members[0])).await;
    for (i, m) in members.iter().enumerate() {
        let role = if i == 0 { "admin" } else { "member" };
        exec(c, &format!(
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('{g}', '{m}', '{role}')"
        )).await;
    }
    exec(c, &format!("INSERT INTO conversation (id, kind) VALUES ('{ch}', 'channel')")).await;
    exec(c, &format!("INSERT INTO channels (id, group_id, name) VALUES ('{ch}', '{g}', 'general')")).await;
}

async fn dm(c: &Connection, d: &str, members: &[&str]) {
    exec(c, &format!("INSERT INTO conversation (id, kind) VALUES ('{d}', 'dm')")).await;
    exec(c, &format!("INSERT INTO dm_channel (id, created_by) VALUES ('{d}', '{}')", members[0])).await;
    for m in members {
        exec(c, &format!(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('{d}', '{m}', '{m}')"
        )).await;
    }
}

fn capability_hash(token: &str) -> String {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(token.as_bytes()))
}

fn token_of(msg: &str) -> String {
    format!("tok-{msg}")
}

/// A sealed message `id` in `conv` carrying `file`, declared through the real
/// registration path, proven by its deletion capability (#1086, #1330).
async fn send_file(c: &Connection, id: &str, conv: &str, seq: i64, file: &str) -> WriteOutcome {
    exec(c, &format!(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, seq, sealed, delete_token_hash) \
         VALUES ('{id}', '{conv}', 'sealed', 'ct', '2026-01-01', {seq}, 1, '{}')",
        capability_hash(&token_of(id))
    )).await;
    declare(c, id, file).await
}

async fn send_with_file(c: &Connection, id: &str, conv: &str, seq: i64) {
    assert!(matches!(send_file(c, id, conv, seq, FILE).await, WriteOutcome::Ok));
}

/// The client's best-effort declaration, sent after the envelope.
async fn declare(c: &Connection, id: &str, file: &str) -> WriteOutcome {
    apply_register_attachment(
        c,
        Some("anyone"),
        &AttachmentRegisterBody::ForMessage {
            content_hash: file.into(),
            r2_key: format!("media/{file}.enc"),
            message_id: id.into(),
            delete_token: Some(token_of(id)),
        },
    )
    .await
    .unwrap()
}

/// The author's delete-for-everyone, as `retract_for_everyone` posts it, then
/// the collect the client asks for afterwards.
async fn author_deletes(c: &Connection, author: &str, id: &str, conv: &str) {
    author_deletes_with(c, author, id, conv, Some(token_of(id))).await;
    apply_delete_attachment(
        c,
        Some(author),
        &AttachmentDeleteBody { content_hash: FILE.into(), message_id: Some(id.into()) },
    )
    .await
    .unwrap();
}

async fn author_deletes_with(c: &Connection, author: &str, id: &str, conv: &str, token: Option<String>) {
    let out = apply_delete_message(
        c,
        Some(author),
        &DeleteMessageBody {
            message_id: id.into(),
            conversation_id: conv.into(),
            msg_sender_id: Some(author.into()),
            actor_id: None,
            delete_token: token,
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
}

async fn object_row(c: &Connection) -> bool {
    scalar(c, &format!("SELECT COUNT(*) FROM attachment_object WHERE content_hash = '{FILE}'")).await > 0
}

async fn released(c: &Connection) -> bool {
    scalar(
        c,
        &format!(
            "SELECT COUNT(*) FROM attachment_object WHERE content_hash = '{FILE}' AND reclaim_state IS NOT NULL"
        ),
    )
    .await
        > 0
}

async fn declarations(c: &Connection) -> i64 {
    scalar(c, &format!("SELECT COUNT(*) FROM attachment_ref WHERE content_hash = '{FILE}'")).await
        + scalar(c, &format!("SELECT COUNT(*) FROM vault_attachment_ref WHERE content_hash = '{FILE}'")).await
}

async fn reclaim(c: &Connection, r2: &FakeR2) -> ReclaimReport {
    reclaim_released_attachments(c, r2).await.unwrap()
}

/// Still in someone's history: the row the client resolves, the blob in R2,
/// no release in flight, and the presign gate refusing a delete. Runs a
/// reclaim pass first, so "kept" means kept by the DS, not merely not yet.
async fn assert_kept(c: &Connection, r2: &FakeR2, why: &str) {
    reclaim(c, r2).await;
    assert!(object_row(c).await, "{why}: the object row went while still in history");
    assert!(!released(c).await, "{why}: an object still in history was released");
    assert!(r2.holds(&key()), "{why}: the blob went while still in history");
    assert!(
        object_is_referenced(c, FILE).await.unwrap(),
        "{why}: the presign gate must still refuse a delete"
    );
}

async fn assert_removed(c: &Connection, r2: &FakeR2, why: &str) {
    reclaim(c, r2).await;
    assert!(!r2.holds(&key()), "{why}: the blob outlived the last delete");
    assert!(!object_row(c).await, "{why}: the object row outlived the last delete");
    assert_eq!(declarations(c).await, 0, "{why}: references outlived the object");
}

/// Alice's channel `ca` and Bob's channel `cb`, both users in both, each user's
/// message carrying the same file.
async fn two_messages(c: &Connection) {
    user(c, "alice").await;
    user(c, "bob").await;
    group(c, "ga", "ca", &["alice", "bob"]).await;
    group(c, "gb", "cb", &["bob", "alice"]).await;
    send_with_file(c, "a_msg", "ca", 1).await;
    send_with_file(c, "b_msg", "cb", 1).await;
}

async fn watermark(c: &Connection, conv: &str, u: &str, seq: i64, reported: &str) {
    exec(c, &format!(
        "INSERT INTO conversation_watermark \
             (conversation_id, user_id, device_id, last_fetched_at, last_seq, reported_at) \
         VALUES ('{conv}', '{u}', '{u}_dev', datetime('now'), {seq}, datetime('now', '{reported}'))"
    )).await;
}

/// Every member has fetched both messages, so envelope GC collects them.
async fn deliver_both(c: &Connection) {
    for conv in ["ca", "cb"] {
        for u in ["alice", "bob"] {
            watermark(c, conv, u, 5, "-1 minute").await;
        }
    }
    sweep_envelope_gc(c, STALE).await.unwrap();
    assert_eq!(scalar(c, "SELECT COUNT(*) FROM message_envelope").await, 0, "GC collected both");
}

// ---- removals that keep history: the file must stay ------------------------

#[tokio::test]
async fn delivery_gc_of_both_messages_keeps_the_file() {
    let c = conn().await;
    two_messages(&c).await;
    let r2 = FakeR2::with(&key());
    deliver_both(&c).await;
    assert_kept(&c, &r2, "after GC collected both delivered messages").await;
}

/// #720: a device silent past the window stops pinning; the members who did
/// fetch still hold the message.
#[tokio::test]
async fn the_staleness_window_keeps_the_file() {
    let c = conn().await;
    two_messages(&c).await;
    let r2 = FakeR2::with(&key());
    for conv in ["ca", "cb"] {
        watermark(&c, conv, "bob", 5, "-1 minute").await;
        watermark(&c, conv, "alice", 0, "-24 months").await;
    }
    sweep_envelope_gc(&c, STALE).await.unwrap();
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM message_envelope").await, 0);
    assert_kept(&c, &r2, "after the staleness window collected both messages").await;
}

async fn delete_account(c: &Connection, u: &str, withdraw: Vec<WithdrawEnvelope>) {
    let (out, _) = pollis_delivery::account::apply_delete_account(
        c,
        Some(u),
        &DeleteAccountBody { user_id: Some(u.into()), withdraw },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
}

fn withdraw(id: &str) -> Vec<WithdrawEnvelope> {
    vec![WithdrawEnvelope { message_id: id.into(), delete_token: token_of(id) }]
}

/// The deleted account's sent messages stay in their recipients' history.
#[tokio::test]
async fn deleting_both_senders_accounts_keeps_the_file() {
    let c = conn().await;
    two_messages(&c).await;
    user(&c, "carol").await;
    exec(&c, "INSERT INTO group_member (group_id, user_id, role) VALUES ('ga', 'carol', 'member'), ('gb', 'carol', 'member')").await;
    let r2 = FakeR2::with(&key());

    // Alice's envelope is still undelivered: the withdraw removes it.
    delete_account(&c, "alice", withdraw("a_msg")).await;
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM message_envelope WHERE id = 'a_msg'").await, 0);
    delete_account(&c, "bob", withdraw("b_msg")).await;
    assert_kept(&c, &r2, "after both senders deleted their accounts").await;
}

#[tokio::test]
async fn group_teardown_keeps_the_file() {
    let c = conn().await;
    two_messages(&c).await;
    let r2 = FakeR2::with(&key());
    for (g, admin) in [("ga", "alice"), ("gb", "bob")] {
        let (out, _) = pollis_delivery::groups::apply_delete_group(
            &c,
            Some(admin),
            &DeleteGroupBody { group_id: g.into(), requester_id: Some(admin.into()) },
        )
        .await
        .unwrap();
        assert!(matches!(out, WriteOutcome::Ok));
    }
    assert_kept(&c, &r2, "after both groups were deleted").await;
}

#[tokio::test]
async fn channel_deletion_keeps_the_file() {
    let c = conn().await;
    two_messages(&c).await;
    let r2 = FakeR2::with(&key());
    for (ch, admin) in [("ca", "alice"), ("cb", "bob")] {
        let out = pollis_delivery::groups::apply_delete_channel(
            &c,
            Some(admin),
            &DeleteChannelBody { channel_id: ch.into(), requester_id: Some(admin.into()) },
        )
        .await
        .unwrap();
        assert!(matches!(out, WriteOutcome::Ok));
    }
    assert_kept(&c, &r2, "after both channels were deleted").await;
}

#[tokio::test]
async fn dm_teardown_keeps_the_file() {
    let c = conn().await;
    for u in ["alice", "bob", "carol"] {
        user(&c, u).await;
    }
    dm(&c, "dm_ac", &["alice", "carol"]).await;
    dm(&c, "dm_bc", &["bob", "carol"]).await;
    send_with_file(&c, "a_msg", "dm_ac", 1).await;
    send_with_file(&c, "b_msg", "dm_bc", 1).await;
    let r2 = FakeR2::with(&key());
    for (d, members) in [("dm_ac", ["alice", "carol"]), ("dm_bc", ["bob", "carol"])] {
        for u in members {
            let (out, _) = pollis_delivery::profile::apply_leave_dm(
                &c,
                Some(u),
                &LeaveDmBody { dm_channel_id: d.into(), user_id: u.into() },
            )
            .await
            .unwrap();
            assert!(matches!(out, WriteOutcome::Ok));
        }
    }
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM dm_channel").await, 0, "both DMs were torn down");
    assert_kept(&c, &r2, "after both DMs were torn down").await;
}

// ---- explicit deletes: the only release -----------------------------------

#[tokio::test]
async fn deleting_both_messages_removes_the_file() {
    let c = conn().await;
    two_messages(&c).await;
    let r2 = FakeR2::with(&key());

    author_deletes(&c, "alice", "a_msg", "ca").await;
    assert_kept(&c, &r2, "after alice deleted hers; bob's still shows it").await;

    author_deletes(&c, "bob", "b_msg", "cb").await;
    assert!(released(&c).await, "the last delete releases in its own transaction");
    // The client collect after the delete must not drop the row the reclaim needs.
    assert!(object_row(&c).await);
    assert_removed(&c, &r2, "after both messages were deleted").await;
}

/// The usual case: both messages were delivered and GC'd long before anyone
/// deletes them. The author's capability, copied onto the reference at
/// registration, still releases it.
#[tokio::test]
async fn deleting_both_messages_after_delivery_gc_removes_the_file() {
    let c = conn().await;
    two_messages(&c).await;
    let r2 = FakeR2::with(&key());
    deliver_both(&c).await;

    author_deletes(&c, "alice", "a_msg", "ca").await;
    assert_kept(&c, &r2, "after alice deleted hers").await;
    author_deletes(&c, "bob", "b_msg", "cb").await;
    assert_removed(&c, &r2, "after both delivered messages were deleted").await;
}

/// An admin's tombstone removes the message for everyone, so it releases too.
#[tokio::test]
async fn admin_deletes_release_the_file() {
    let c = conn().await;
    two_messages(&c).await;
    let r2 = FakeR2::with(&key());
    for (admin, sender, id, conv) in [("alice", "bob", "a_msg", "ca"), ("bob", "alice", "b_msg", "cb")] {
        let out = apply_delete_message(
            &c,
            Some(admin),
            &DeleteMessageBody {
                message_id: id.into(),
                conversation_id: conv.into(),
                msg_sender_id: Some(sender.into()),
                actor_id: None,
                delete_token: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(out, WriteOutcome::Ok));
        if id == "a_msg" {
            assert_kept(&c, &r2, "after the first admin delete").await;
        }
    }
    assert_removed(&c, &r2, "after both admin deletes").await;
}

/// After GC the DS cannot tell who wrote a message except by its capability;
/// a member naming someone else's message id releases nothing.
#[tokio::test]
async fn a_delete_without_the_capability_releases_nothing() {
    let c = conn().await;
    two_messages(&c).await;
    let r2 = FakeR2::with(&key());
    deliver_both(&c).await;
    author_deletes_with(&c, "alice", "b_msg", "cb", None).await;
    author_deletes_with(&c, "alice", "b_msg", "cb", Some("guess".into())).await;
    author_deletes(&c, "alice", "a_msg", "ca").await;
    assert_kept(&c, &r2, "bob's message was never deleted by its author").await;
}

#[tokio::test]
async fn a_single_reference_is_removed_when_its_message_is_deleted() {
    let c = conn().await;
    user(&c, "alice").await;
    group(&c, "ga", "ca", &["alice"]).await;
    send_with_file(&c, "a_msg", "ca", 1).await;
    let r2 = FakeR2::with(&key());
    author_deletes(&c, "alice", "a_msg", "ca").await;
    assert_eq!(reclaim(&c, &r2).await, ReclaimReport { reclaimed: 1, failed: 0 });
    assert_removed(&c, &r2, "a single-reference file").await;
}

// ---- the vault ---------------------------------------------------------------

fn vault_entry(id: &str, hashes: &[&str]) -> SaveVaultMessageBody {
    SaveVaultMessageBody {
        id: id.into(),
        user_id: "bob".into(),
        nonce: b64(&[7u8; 12]),
        blob: b64(&[1u8; 32]),
        created_at: "2026-01-01T00:00:00Z".into(),
        attachment_hashes: hashes.iter().map(|h| h.to_string()).collect(),
    }
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn save_vault(c: &Connection, id: &str, hashes: &[&str]) {
    let out = pollis_delivery::vault::apply_save_vault_message(c, Some("bob"), &vault_entry(id, hashes))
        .await
        .unwrap();
    assert!(matches!(out, pollis_delivery::vault::VaultOutcome::Ok));
}

/// A vault entry pins the file like a message; deleting the entry is deleting
/// it from the only history it is in.
#[tokio::test]
async fn a_vault_entry_pins_the_file_until_it_is_deleted() {
    let c = conn().await;
    two_messages(&c).await;
    save_vault(&c, "v1", &[FILE]).await;
    let r2 = FakeR2::with(&key());

    author_deletes(&c, "alice", "a_msg", "ca").await;
    author_deletes(&c, "bob", "b_msg", "cb").await;
    assert_kept(&c, &r2, "the vault entry still holds it").await;

    pollis_delivery::vault::apply_delete_vault_message(
        &c,
        Some("bob"),
        &DeleteVaultMessageBody { id: "v1".into(), user_id: "bob".into() },
    )
    .await
    .unwrap();
    assert_removed(&c, &r2, "after the vault entry was deleted").await;
}

/// A vault edit that drops the file releases it; one that keeps it does not.
#[tokio::test]
async fn a_vault_edit_that_drops_the_file_releases_it() {
    let c = conn().await;
    user(&c, "bob").await;
    exec(&c, &format!("INSERT INTO attachment_object (content_hash, r2_key) VALUES ('{FILE}', '{}')", key())).await;
    save_vault(&c, "v1", &[FILE]).await;
    let r2 = FakeR2::with(&key());
    save_vault(&c, "v1", &[FILE]).await;
    assert_kept(&c, &r2, "a re-save that keeps the file").await;
    save_vault(&c, "v1", &[]).await;
    assert_removed(&c, &r2, "an edit that dropped the file").await;
}

/// Account deletion deletes the account's own vault: a file only it held goes.
#[tokio::test]
async fn account_deletion_removes_a_file_only_its_vault_held() {
    let c = conn().await;
    user(&c, "bob").await;
    exec(&c, &format!("INSERT INTO attachment_object (content_hash, r2_key) VALUES ('{FILE}', '{}')", key())).await;
    save_vault(&c, "v1", &[FILE]).await;
    let r2 = FakeR2::with(&key());
    delete_account(&c, "bob", Vec::new()).await;
    assert_removed(&c, &r2, "after the vault's account went").await;
}

// ---- races, failures, scope ---------------------------------------------------

/// The client posts the envelope, then declares. A declaration that lands after
/// another message's delete released the file revives it.
#[tokio::test]
async fn a_late_declaration_revives_a_released_file() {
    let c = conn().await;
    user(&c, "alice").await;
    user(&c, "bob").await;
    group(&c, "ga", "ca", &["alice", "bob"]).await;
    send_with_file(&c, "a_msg", "ca", 1).await;
    // Bob's message carrying the same file is posted; its declaration lags.
    exec(&c, &format!(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, seq, sealed, delete_token_hash) \
         VALUES ('b_msg', 'ca', 'sealed', 'ct', '2026-01-01', 2, 1, '{}')",
        capability_hash(&token_of("b_msg"))
    )).await;
    let r2 = FakeR2::with(&key());

    author_deletes(&c, "alice", "a_msg", "ca").await;
    assert!(released(&c).await);
    assert!(matches!(declare(&c, "b_msg", FILE).await, WriteOutcome::Ok), "never refused");
    assert!(!released(&c).await, "the declaration itself revives the file");
    assert_kept(&c, &r2, "the late declaration revived it").await;

    // And a vault save does the same.
    author_deletes(&c, "bob", "b_msg", "ca").await;
    assert!(released(&c).await);
    save_vault(&c, "v1", &[FILE]).await;
    assert!(!released(&c).await, "the save itself revives the file");
    assert_kept(&c, &r2, "a vault save revived it").await;
}

/// Once the R2 delete has begun the bytes may be gone, so a reference landing
/// then cannot keep a row that names a missing blob: row and references go.
#[tokio::test]
async fn a_reference_landing_mid_delete_never_leaves_a_row_without_its_blob() {
    let c = conn().await;
    user(&c, "alice").await;
    group(&c, "ga", "ca", &["alice"]).await;
    send_with_file(&c, "a_msg", "ca", 1).await;
    author_deletes(&c, "alice", "a_msg", "ca").await;
    // A pass claimed it and crashed after the blob delete.
    exec(&c, &format!("UPDATE attachment_object SET reclaim_state = 'deleting' WHERE content_hash = '{FILE}'")).await;
    let r2 = FakeR2::default();
    exec(&c, "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at, seq) \
              VALUES ('late', 'ca', 'alice', 'ct', '2026-01-01', 9)").await;
    exec(&c, &format!("INSERT INTO attachment_ref (content_hash, message_id) VALUES ('{FILE}', 'late')")).await;
    assert_eq!(reclaim(&c, &r2).await, ReclaimReport { reclaimed: 1, failed: 0 });
    assert!(!object_row(&c).await, "a row survived naming a deleted blob");
    assert_eq!(declarations(&c).await, 0);
}

/// A released file referenced again by a row written outside the revive path
/// is un-released, never deleted.
#[tokio::test]
async fn a_reference_that_reappears_cancels_the_reclaim() {
    let c = conn().await;
    user(&c, "alice").await;
    group(&c, "ga", "ca", &["alice"]).await;
    send_with_file(&c, "a_msg", "ca", 1).await;
    author_deletes(&c, "alice", "a_msg", "ca").await;
    exec(&c, &format!("INSERT INTO attachment_ref (content_hash, message_id) VALUES ('{FILE}', 'late')")).await;
    let r2 = FakeR2::with(&key());
    assert_kept(&c, &r2, "a reappeared reference").await;
    assert_eq!(r2.delete_calls(), 0);
}

/// A failed R2 delete keeps the row (the only record of the key) and a later
/// pass finishes the job.
#[tokio::test]
async fn a_failed_r2_delete_is_retried_on_a_later_pass() {
    let c = conn().await;
    user(&c, "alice").await;
    group(&c, "ga", "ca", &["alice"]).await;
    send_with_file(&c, "a_msg", "ca", 1).await;
    author_deletes(&c, "alice", "a_msg", "ca").await;
    let r2 = FakeR2::with(&key());

    r2.fail.store(true, Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(reclaim(&c, &r2).await, ReclaimReport { reclaimed: 0, failed: 1 });
        assert!(object_row(&c).await && released(&c).await, "the row must survive a failed delete");
        assert!(r2.holds(&key()));
    }
    r2.fail.store(false, Ordering::SeqCst);
    assert_eq!(reclaim(&c, &r2).await, ReclaimReport { reclaimed: 1, failed: 0 });
    assert_removed(&c, &r2, "after the retry").await;
    assert_eq!(r2.delete_calls(), 3);
}

/// A blob already gone (the client deleted it, or a crash after the R2 delete
/// but before the row delete) is a success, so the row still goes.
#[tokio::test]
async fn a_crash_between_blob_and_row_converges() {
    let c = conn().await;
    user(&c, "alice").await;
    group(&c, "ga", "ca", &["alice"]).await;
    send_with_file(&c, "a_msg", "ca", 1).await;
    author_deletes(&c, "alice", "a_msg", "ca").await;
    let r2 = FakeR2::default();
    assert_eq!(reclaim(&c, &r2).await, ReclaimReport { reclaimed: 1, failed: 0 });
    assert!(!object_row(&c).await);
}

/// The DS deletes only the key its own presign would name for the row's file
/// id: a row registered with any other key loses the row, never that object.
#[tokio::test]
async fn reclaim_never_deletes_a_key_outside_the_objects_own_name() {
    let c = conn().await;
    user(&c, "alice").await;
    group(&c, "ga", "ca", &["alice"]).await;
    let foreign = "avatars/bob/0000000000000000000000000000000000000000000000000000000000000000.webp";
    exec(&c, &format!(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, seq, sealed, delete_token_hash) \
         VALUES ('a_msg', 'ca', 'sealed', 'ct', '2026-01-01', 1, 1, '{}')",
        capability_hash(&token_of("a_msg"))
    )).await;
    apply_register_attachment(
        &c,
        Some("alice"),
        &AttachmentRegisterBody::ForMessage {
            content_hash: FILE.into(),
            r2_key: foreign.into(),
            message_id: "a_msg".into(),
            delete_token: Some(token_of("a_msg")),
        },
    )
    .await
    .unwrap();
    let r2 = FakeR2::with(foreign);
    author_deletes(&c, "alice", "a_msg", "ca").await;
    reclaim(&c, &r2).await;
    assert!(r2.holds(foreign), "the DS deleted an object outside the attachment's own key");
    assert_eq!(r2.delete_calls(), 0);
    assert!(!object_row(&c).await);
}

/// An upload registered with no message yet (`ObjectOnly`, between upload and
/// send) is never touched: nothing released it.
#[tokio::test]
async fn an_upload_awaiting_its_message_is_left_alone() {
    let c = conn().await;
    two_messages(&c).await;
    let pending = "2f2e2d2c2b2a29282726252423222120f0e0d0c0b0a090807060504030201000";
    let pending_key = format!("media/{pending}.enc");
    apply_register_attachment(
        &c,
        Some("alice"),
        &AttachmentRegisterBody::ObjectOnly { content_hash: pending.into(), r2_key: pending_key.clone() },
    )
    .await
    .unwrap();
    let r2 = FakeR2::with(&pending_key);
    author_deletes(&c, "alice", "a_msg", "ca").await;
    author_deletes(&c, "bob", "b_msg", "cb").await;
    reclaim(&c, &r2).await;
    assert!(r2.holds(&pending_key));
    assert_eq!(
        scalar(&c, &format!("SELECT COUNT(*) FROM attachment_object WHERE content_hash = '{pending}'")).await,
        1
    );
}

// ---- message ids are client-chosen --------------------------------------------

/// After delivery GC frees id `a_msg`, someone else posts their own envelope
/// under that id and deletes it, by their own capability or as admin of their
/// own group. Only references registered against the envelope actually removed
/// go, so the original's file stays until its author deletes it.
#[tokio::test]
async fn an_id_reused_after_gc_cannot_release_the_originals_file() {
    let c = conn().await;
    two_messages(&c).await;
    user(&c, "mallory").await;
    group(&c, "gm", "cm", &["mallory"]).await;
    let r2 = FakeR2::with(&key());
    deliver_both(&c).await;
    author_deletes(&c, "bob", "b_msg", "cb").await;

    // Self path: mallory's own envelope `a_msg`, her own capability.
    exec(&c, &format!(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, seq, sealed, delete_token_hash) \
         VALUES ('a_msg', 'cm', 'sealed', 'ct', '2026-01-02', 1, 1, '{}')",
        capability_hash("mallory-token")
    )).await;
    let _ = apply_register_attachment(
        &c,
        Some("mallory"),
        &AttachmentRegisterBody::ForMessage {
            content_hash: FILE.into(),
            r2_key: key(),
            message_id: "a_msg".into(),
            delete_token: Some("mallory-token".into()),
        },
    )
    .await
    .unwrap();
    author_deletes_with(&c, "mallory", "a_msg", "cm", Some("mallory-token".into())).await;
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM message_envelope WHERE id = 'a_msg'").await, 0);
    assert_kept(&c, &r2, "a self-delete of a reused id").await;

    // Admin path: the same id again, tombstoned by mallory as admin of her group.
    exec(&c, &format!(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, seq, sealed, delete_token_hash) \
         VALUES ('a_msg', 'cm', 'sealed', 'ct', '2026-01-03', 2, 1, '{}')",
        capability_hash("mallory-token-2")
    )).await;
    let out = apply_delete_message(
        &c,
        Some("mallory"),
        &DeleteMessageBody {
            message_id: "a_msg".into(),
            conversation_id: "cm".into(),
            msg_sender_id: Some("someone".into()),
            actor_id: None,
            delete_token: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
    assert_kept(&c, &r2, "an admin delete of a reused id").await;

    // The real author's delete still works.
    author_deletes(&c, "alice", "a_msg", "ca").await;
    assert_removed(&c, &r2, "after the original's author deleted it").await;
}

/// A legacy reference carries no capability hash; no delete can prove it, so
/// it pins its file.
#[tokio::test]
async fn a_legacy_reference_without_a_capability_is_never_released() {
    let c = conn().await;
    user(&c, "alice").await;
    group(&c, "ga", "ca", &["alice"]).await;
    exec(&c, "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at, seq) \
              VALUES ('old', 'ca', 'alice', 'ct', '2026-01-01', 1)").await;
    let out = apply_register_attachment(
        &c,
        Some("alice"),
        &AttachmentRegisterBody::ForMessage {
            content_hash: FILE.into(),
            r2_key: key(),
            message_id: "old".into(),
            delete_token: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
    let r2 = FakeR2::with(&key());
    author_deletes_with(&c, "alice", "old", "ca", None).await;
    assert_kept(&c, &r2, "a legacy message's delete").await;
}

// ---- registration while the bytes are going --------------------------------

/// Once the DS has started deleting a file's bytes, a new reference could not
/// keep them: registration and the Vault save say so instead of accepting a
/// reference that would be dropped.
#[tokio::test]
async fn a_reference_to_a_file_being_deleted_is_refused_explicitly() {
    let c = conn().await;
    user(&c, "alice").await;
    user(&c, "bob").await;
    group(&c, "ga", "ca", &["alice", "bob"]).await;
    send_with_file(&c, "a_msg", "ca", 1).await;
    author_deletes(&c, "alice", "a_msg", "ca").await;
    exec(&c, &format!("UPDATE attachment_object SET reclaim_state = 'deleting' WHERE content_hash = '{FILE}'")).await;

    exec(&c, &format!(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, seq, sealed, delete_token_hash) \
         VALUES ('b_msg', 'ca', 'sealed', 'ct', '2026-01-01', 2, 1, '{}')",
        capability_hash(&token_of("b_msg"))
    )).await;
    assert!(matches!(declare(&c, "b_msg", FILE).await, WriteOutcome::Invalid(_)));
    let out = pollis_delivery::vault::apply_save_vault_message(&c, Some("bob"), &vault_entry("v1", &[FILE]))
        .await
        .unwrap();
    assert!(matches!(out, pollis_delivery::vault::VaultOutcome::Invalid(_)));
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM vault_message").await, 0, "the save is all or nothing");
    assert_eq!(declarations(&c).await, 0);
}

// ---- bounded passes -------------------------------------------------------------

/// One pass takes on at most a bounded number of objects; the rest wait.
#[tokio::test]
async fn a_reclaim_pass_is_bounded() {
    let c = conn().await;
    let r2 = FakeR2::default();
    for i in 0..150 {
        let id = format!("{i:064x}");
        exec(&c, &format!(
            "INSERT INTO attachment_object (content_hash, r2_key, reclaim_state) \
             VALUES ('{id}', 'media/{id}.enc', 'released')"
        )).await;
    }
    assert_eq!(reclaim(&c, &r2).await.reclaimed, 100);
    assert_eq!(reclaim(&c, &r2).await.reclaimed, 50);
    assert_eq!(scalar(&c, "SELECT COUNT(*) FROM attachment_object").await, 0);
}
