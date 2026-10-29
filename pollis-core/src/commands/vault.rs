//! The Vault (#107) — a personal encrypted space that behaves like a chat
//! with yourself, synced across the account's devices like cloud storage.
//!
//! Sealed under a key derived from the account identity key, so a new device reads the whole vault.
//! Plaintext is `{ body, pinned }` (`body` as in chat), so pin/unpin looks like an edit to the DS.
//! Each save's `attachment_hashes` keep its files alive for the DS GC.
//! Writes go remote first; reads pull then serve the cache (offline-tolerant). Last write wins.

use std::sync::Arc;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use crate::util::b64;
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::state::AppState;

/// Distinct info strings keep the account key from being repurposed across uses.
const VAULT_HKDF_INFO: &[u8] = b"pollis-vault-v1";

const NONCE_LEN: usize = 12;

/// One decrypted vault entry, as the UI consumes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultMessage {
    pub id: String,
    /// The entry's content string — same shape as `message.content`.
    pub content: String,
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// What gets sealed: the entry body plus everything the server must not see.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct VaultEntryPlain {
    v: u8,
    body: String,
    #[serde(default)]
    pinned: bool,
}

/// Derive this account's vault key, salted by the user id.
fn derive_vault_key(account_seed: &[u8], user_id: &str) -> Zeroizing<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(user_id.as_bytes()), account_seed);
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(VAULT_HKDF_INFO, out.as_mut())
        .expect("HKDF-SHA256 expand 32 bytes is always valid");
    out
}

/// HKDF info for the root key Vault ATTACHMENTS are sealed under (#1382).
const VAULT_FILES_HKDF_INFO: &[u8] = b"pollis-vault-files-v1";

/// Root key for Vault attachments, derived from the account key like the entry key.
pub(crate) async fn vault_file_root_key(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<Zeroizing<[u8; 32]>> {
    let seed = account_seed(state, user_id).await?;
    let hk = Hkdf::<Sha256>::new(Some(user_id.as_bytes()), &seed);
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(VAULT_FILES_HKDF_INFO, out.as_mut())
        .expect("HKDF-SHA256 expand 32 bytes is always valid");
    Ok(out)
}

/// This device's copy of the account identity key seed, as HKDF input.
async fn account_seed(state: &Arc<AppState>, user_id: &str) -> Result<Zeroizing<Vec<u8>>> {
    let key = crate::commands::account_identity::load_account_id_key(state, user_id).await?;
    Ok(Zeroizing::new(key.to_seed().to_vec()))
}

/// Seal one entry, padded to the message size buckets.
pub(crate) fn seal_entry(
    account_seed: &[u8],
    user_id: &str,
    entry: &VaultEntryPlain,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let json = serde_json::to_vec(entry)
        .map_err(|e| Error::Other(anyhow::anyhow!("serialize vault entry: {e}")))?;
    let padded = super::messages::framing::pad(&json);
    let key = derive_vault_key(account_seed, user_id);
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key.as_ref()));
    let ct = cipher
        .encrypt(GenericArray::from_slice(&nonce), padded.as_slice())
        .map_err(|e| Error::Crypto(format!("seal vault entry: {e}")))?;
    Ok((nonce.to_vec(), ct))
}

/// Recover one entry from what the DS handed back.
pub(crate) fn open_entry(
    account_seed: &[u8],
    user_id: &str,
    nonce: &[u8],
    blob: &[u8],
) -> Result<VaultEntryPlain> {
    if nonce.len() != NONCE_LEN {
        return Err(Error::Crypto(format!(
            "vault nonce has wrong length: {} (expected {NONCE_LEN})",
            nonce.len()
        )));
    }
    let key = derive_vault_key(account_seed, user_id);
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key.as_ref()));
    let padded = cipher
        .decrypt(GenericArray::from_slice(nonce), blob)
        .map_err(|e| Error::Crypto(format!("open vault entry: {e}")))?;
    let json = super::messages::framing::strip(&padded);
    serde_json::from_slice(&json)
        .map_err(|e| Error::Other(anyhow::anyhow!("parse vault entry: {e}")))
}

fn upsert_local(
    conn: &Connection,
    id: &str,
    content: &str,
    pinned: bool,
    created_at: &str,
    updated_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO vault_entry_cache (id, content, pinned, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(id) DO UPDATE SET \
             content = excluded.content, \
             pinned = excluded.pinned, \
             created_at = excluded.created_at, \
             updated_at = excluded.updated_at",
        rusqlite::params![id, content, pinned as i64, created_at, updated_at],
    )
    .map_err(|e| Error::Other(anyhow::anyhow!("vault cache upsert: {e}")))?;
    Ok(())
}

fn list_local(conn: &Connection) -> Result<Vec<VaultMessage>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, content, pinned, created_at, updated_at FROM vault_entry_cache \
             ORDER BY created_at ASC, id ASC",
        )
        .map_err(|e| Error::Other(anyhow::anyhow!("vault cache prepare: {e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(VaultMessage {
                id: row.get(0)?,
                content: row.get(1)?,
                pinned: row.get::<_, i64>(2)? != 0,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })
        .map_err(|e| Error::Other(anyhow::anyhow!("vault cache query: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| Error::Other(anyhow::anyhow!("vault cache row: {e}")))?);
    }
    Ok(out)
}

fn get_local(conn: &Connection, id: &str) -> Result<Option<VaultMessage>> {
    conn.query_row(
        "SELECT id, content, pinned, created_at, updated_at FROM vault_entry_cache WHERE id = ?1",
        rusqlite::params![id],
        |row| {
            Ok(VaultMessage {
                id: row.get(0)?,
                content: row.get(1)?,
                pinned: row.get::<_, i64>(2)? != 0,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(|e| Error::Other(anyhow::anyhow!("vault cache lookup: {e}")))
}

/// Pull the account's entries and make the local cache exactly match them —
/// upsert everything the server holds, delete everything it no longer does
/// (removed elsewhere). Undecryptable entries are hidden, never deleted server-side.
pub(crate) async fn sync_vault(state: &Arc<AppState>, user_id: &str) -> Result<usize> {
    let resp = super::ds_reads::read_vault(state, user_id).await?;
    let seed = account_seed(state, user_id).await?;

    let mut decrypted: Vec<(String, VaultEntryPlain, String, String)> =
        Vec::with_capacity(resp.messages.len());
    let mut keep_ids: Vec<String> = Vec::with_capacity(resp.messages.len());
    for m in resp.messages {
        let nonce = super::ds_reads::decode_b64("vault nonce", &m.nonce)?;
        let blob = super::ds_reads::decode_b64("vault blob", &m.blob)?;
        match open_entry(&seed, user_id, &nonce, &blob) {
            Ok(entry) => {
                keep_ids.push(m.id.clone());
                decrypted.push((m.id, entry, m.created_at, m.updated_at));
            }
            Err(e) => {
                crate::diag!("[vault] entry {} is unreadable on this build: {e}", m.id);
                keep_ids.push(m.id);
            }
        }
    }

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    let conn = db.conn();
    // Chunked IN-list, mirroring the codebase's BIND_CHUNK habit.
    if keep_ids.is_empty() {
        conn.execute("DELETE FROM vault_entry_cache", [])
            .map_err(|e| Error::Other(anyhow::anyhow!("vault cache prune: {e}")))?;
    } else {
        let placeholders = keep_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM vault_entry_cache WHERE id NOT IN ({placeholders})");
        conn.execute(&sql, rusqlite::params_from_iter(keep_ids.iter()))
            .map_err(|e| Error::Other(anyhow::anyhow!("vault cache prune: {e}")))?;
    }
    let count = decrypted.len();
    for (id, entry, created_at, updated_at) in decrypted {
        upsert_local(conn, &id, &entry.body, entry.pinned, &created_at, &updated_at)?;
    }
    Ok(count)
}

/// The vault's entries, oldest first; serves the cache when offline.
pub async fn get_vault_messages(user_id: String, state: &Arc<AppState>) -> Result<Vec<VaultMessage>> {
    if let Err(e) = sync_vault(state, &user_id).await {
        crate::diag!("[vault] sync failed, serving the local cache: {e}");
    }
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    list_local(db.conn())
}

/// Save remote-first; cache only after the DS accepts.
async fn save_entry(
    state: &Arc<AppState>,
    user_id: &str,
    id: &str,
    entry: &VaultEntryPlain,
    created_at: &str,
) -> Result<()> {
    let seed = account_seed(state, user_id).await?;
    let (nonce, blob) = seal_entry(&seed, user_id, entry)?;
    let attachment_hashes = super::messages::parse_attachment_refs(&entry.body)
        .into_iter()
        .map(|r| r.content_hash)
        .collect();
    let body = pollis_api::vault::SaveVaultMessageBody {
        id: id.to_string(),
        user_id: user_id.to_string(),
        nonce: b64(&nonce),
        blob: b64(&blob),
        created_at: created_at.to_string(),
        attachment_hashes,
    };
    super::mls::ds_post_ok(state, &body).await
}

/// Add a new entry; `content` is a chat composer string.
pub async fn send_vault_message(
    user_id: String,
    content: String,
    state: &Arc<AppState>,
) -> Result<VaultMessage> {
    if content.trim().is_empty() {
        return Err(Error::Other(anyhow::anyhow!("vault entry is empty")));
    }
    let id = ulid::Ulid::new().to_string();
    let created_at = super::messages::envelope_sent_at();
    let entry = VaultEntryPlain {
        v: 1,
        body: content.clone(),
        pinned: false,
    };
    save_entry(state, &user_id, &id, &entry, &created_at).await?;

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    upsert_local(db.conn(), &id, &content, false, &created_at, &created_at)?;
    Ok(VaultMessage {
        id,
        content,
        pinned: false,
        created_at: created_at.clone(),
        updated_at: created_at,
    })
}

/// Rewrite an entry's body, keeping its pinned flag and creation time.
pub async fn edit_vault_message(
    id: String,
    new_content: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<VaultMessage> {
    if new_content.trim().is_empty() {
        return Err(Error::Other(anyhow::anyhow!("vault entry is empty")));
    }
    let existing = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        get_local(db.conn(), &id)?
    }
    .ok_or_else(|| Error::Other(anyhow::anyhow!("vault entry not found")))?;
    // An edit that drops a file releases it, exactly like a delete does.
    let released = released_files(state, &id, Some(&new_content)).await?;

    let entry = VaultEntryPlain {
        v: 1,
        body: new_content.clone(),
        pinned: existing.pinned,
    };
    save_entry(state, &user_id, &id, &entry, &existing.created_at).await?;
    for att in &released {
        release_vault_file(state, &user_id, att).await;
    }

    let updated_at = super::messages::envelope_sent_at();
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    upsert_local(
        db.conn(),
        &id,
        &new_content,
        existing.pinned,
        &existing.created_at,
        &updated_at,
    )?;
    Ok(VaultMessage {
        id,
        content: new_content,
        pinned: existing.pinned,
        created_at: existing.created_at,
        updated_at,
    })
}

/// Flip an entry's pinned flag (inside the ciphertext).
pub async fn set_vault_message_pinned(
    id: String,
    pinned: bool,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<VaultMessage> {
    let existing = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        get_local(db.conn(), &id)?
    }
    .ok_or_else(|| Error::Other(anyhow::anyhow!("vault entry not found")))?;

    let entry = VaultEntryPlain {
        v: 1,
        body: existing.content.clone(),
        pinned,
    };
    save_entry(state, &user_id, &id, &entry, &existing.created_at).await?;

    let updated_at = super::messages::envelope_sent_at();
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    upsert_local(
        db.conn(),
        &id,
        &existing.content,
        pinned,
        &existing.created_at,
        &updated_at,
    )?;
    Ok(VaultMessage {
        id,
        content: existing.content,
        pinned,
        created_at: existing.created_at,
        updated_at,
    })
}

/// Hard-delete one entry, remote first.
pub async fn delete_vault_message(
    id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // Read the files BEFORE delete; nothing else would ever collect them.
    let released = released_files(state, &id, None).await?;

    let body = pollis_api::vault::DeleteVaultMessageBody {
        id: id.clone(),
        user_id: user_id.clone(),
    };
    super::mls::ds_post_ok(state, &body).await?;

    {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        db.conn()
            .execute(
                "DELETE FROM vault_entry_cache WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(|e| Error::Other(anyhow::anyhow!("vault cache delete: {e}")))?;
    }

    for att in &released {
        release_vault_file(state, &user_id, att).await;
    }
    Ok(())
}

/// The files entry `id` holds that no OTHER entry in the local cache holds,
/// minus any still named by `new_body` (an edit that keeps a file). These are
/// the files whose last reference a delete or an edit of `id` releases.
async fn released_files(
    state: &Arc<AppState>,
    id: &str,
    new_body: Option<&str>,
) -> Result<Vec<super::messages::AttachmentRef>> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    let mut held_elsewhere = std::collections::HashSet::new();
    let mut mine = Vec::new();
    for entry in list_local(db.conn())? {
        let refs = super::messages::parse_attachment_refs(&entry.content);
        if entry.id == id {
            mine = refs;
        } else {
            held_elsewhere.extend(refs.into_iter().map(|r| r.content_hash));
        }
    }
    if let Some(body) = new_body {
        held_elsewhere.extend(
            super::messages::parse_attachment_refs(body)
                .into_iter()
                .map(|r| r.content_hash),
        );
    }
    Ok(mine
        .into_iter()
        .filter(|r| !held_elsewhere.contains(&r.content_hash))
        .collect())
}

/// Collect a Vault file whose last reference was just released (#1491). Best-effort.
async fn release_vault_file(
    state: &Arc<AppState>,
    user_id: &str,
    att: &super::messages::AttachmentRef,
) {
    let collect = pollis_api::messages::AttachmentDeleteBody {
        content_hash: att.content_hash.clone(),
        message_id: None,
    };
    if let Err(e) = super::mls::ds_post_ok(state, &collect).await {
        crate::diag!("[vault] failed to collect attachment row {}: {e}", att.content_hash);
    }
    if let Err(e) = super::r2::delete_r2_object(state, &att.r2_key).await {
        crate::diag!("[vault] failed to delete object {}: {e}", att.r2_key);
    }
    super::r2::evict_cached_file_for_user(user_id, &att.content_hash);
}

/// Case-insensitive substring search over the local cache (the server can't search).
pub async fn search_vault_messages(
    query: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<VaultMessage>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let _ = user_id;
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    let all = list_local(db.conn())?;
    Ok(all
        .into_iter()
        .filter(|m| m.content.to_lowercase().contains(&needle))
        .collect())
}

/// Test harness only: a Vault file's secrets, for the soak suite (#1491).
#[cfg(feature = "test-harness")]
pub mod test_hooks {
    use super::*;

    pub struct VaultFileSecrets {
        /// The account identity key seed every Vault key derives from.
        pub account_seed: Vec<u8>,
        /// The Vault file root (`pollis-vault-files-v1`).
        pub root_key: [u8; 32],
        /// This file's AES-256-GCM key and base nonce.
        pub file_key: [u8; 32],
        pub file_nonce: [u8; 12],
    }

    pub async fn vault_file_secrets(
        state: &Arc<AppState>,
        user_id: &str,
        file_id_hex: &str,
    ) -> Result<VaultFileSecrets> {
        let file_id: [u8; 32] = hex::decode(file_id_hex)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| Error::Other(anyhow::anyhow!("file id must be 32 hex bytes")))?;
        let seed = account_seed(state, user_id).await?;
        let root = vault_file_root_key(state, user_id).await?;
        let (key, nonce) = crate::commands::file_keys::derive_file_key(&root, &file_id);
        Ok(VaultFileSecrets {
            account_seed: seed.to_vec(),
            root_key: *root,
            file_key: *key,
            file_nonce: nonce,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: &[u8; 32] = b"0123456789abcdef0123456789abcdef";
    const ME: &str = "u-me";

    fn entry(body: &str, pinned: bool) -> VaultEntryPlain {
        VaultEntryPlain {
            v: 1,
            body: body.into(),
            pinned,
        }
    }

    #[test]
    fn a_sealed_entry_opens_back_to_itself() {
        let e = entry("remember the milk", true);
        let (nonce, blob) = seal_entry(SEED, ME, &e).unwrap();
        let back = open_entry(SEED, ME, &nonce, &blob).unwrap();
        assert_eq!(back.body, "remember the milk");
        assert!(back.pinned);
    }

    /// The stored bytes carry neither the words nor the pinned flag.
    #[test]
    fn the_stored_bytes_leak_neither_words_nor_the_pinned_flag() {
        let (_, blob) = seal_entry(SEED, ME, &entry("remember the milk", true)).unwrap();
        for needle in [b"remember the milk".as_slice(), b"pinned".as_slice()] {
            assert!(
                !blob.windows(needle.len()).any(|w| w == needle),
                "plaintext leaked into the stored vault bytes"
            );
        }
        // Same length pinned vs unpinned: the flag does not leak through size.
        let (_, a) = seal_entry(SEED, ME, &entry("same body", true)).unwrap();
        let (_, b) = seal_entry(SEED, ME, &entry("same body", false)).unwrap();
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn another_account_seed_cannot_open_an_entry() {
        let (nonce, blob) = seal_entry(SEED, ME, &entry("secret", false)).unwrap();
        let other = b"ffffffffffffffffffffffffffffffff";
        assert!(open_entry(other, ME, &nonce, &blob).is_err());
    }

    #[test]
    fn the_key_is_bound_to_the_user_and_to_the_vault() {
        let (nonce, blob) = seal_entry(SEED, ME, &entry("secret", false)).unwrap();
        assert!(
            open_entry(SEED, "u-other", &nonce, &blob).is_err(),
            "the same seed under a different user id must not open the entry"
        );
        // A different info string (the read-cursor one) yields a different key.
        let mine = derive_vault_key(SEED, ME);
        let hk = Hkdf::<Sha256>::new(Some(ME.as_bytes()), SEED.as_slice());
        let mut cursor_key = [0u8; 32];
        hk.expand(b"pollis-read-cursor-sync-v1", &mut cursor_key)
            .unwrap();
        assert_ne!(mine.as_ref(), &cursor_key);
    }

    #[test]
    fn a_tampered_blob_is_rejected() {
        let (nonce, mut blob) = seal_entry(SEED, ME, &entry("secret", false)).unwrap();
        blob[0] ^= 1;
        assert!(open_entry(SEED, ME, &nonce, &blob).is_err());
    }

    #[test]
    fn short_entries_share_a_size_bucket() {
        let (_, a) = seal_entry(SEED, ME, &entry("hi", false)).unwrap();
        let (_, b) = seal_entry(SEED, ME, &entry("a somewhat longer note", false)).unwrap();
        assert_eq!(a.len(), b.len(), "entry length leaked through the blob");
    }

    #[test]
    fn two_seals_of_the_same_entry_differ() {
        let e = entry("same", false);
        let (n1, b1) = seal_entry(SEED, ME, &e).unwrap();
        let (n2, b2) = seal_entry(SEED, ME, &e).unwrap();
        assert_ne!(n1, n2);
        assert_ne!(b1, b2);
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::local::apply_local_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn the_cache_round_trips_entries_in_chat_order() {
        let conn = db();
        upsert_local(&conn, "01B", "second", false, "2026-01-02T00:00:00Z", "2026-01-02T00:00:00Z").unwrap();
        upsert_local(&conn, "01A", "first", true, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z").unwrap();
        let all = list_local(&conn).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].content, "first");
        assert!(all[0].pinned);
        assert_eq!(all[1].content, "second");
    }

    #[test]
    fn an_upsert_rewrites_the_entry_whole() {
        let conn = db();
        upsert_local(&conn, "01A", "before", false, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z").unwrap();
        upsert_local(&conn, "01A", "after", true, "2026-01-01T00:00:00Z", "2026-01-03T00:00:00Z").unwrap();
        let m = get_local(&conn, "01A").unwrap().unwrap();
        assert_eq!(m.content, "after");
        assert!(m.pinned);
        assert_eq!(m.created_at, "2026-01-01T00:00:00Z");
        assert_eq!(m.updated_at, "2026-01-03T00:00:00Z");
    }
}
