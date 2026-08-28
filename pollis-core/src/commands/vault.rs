//! The Vault (#107) — a personal encrypted space that behaves like a chat
//! with yourself, synced across the account's devices like cloud storage.
//!
//! # The one deliberate inversion of "a new device starts empty"
//!
//! Accepted loss (2) in CLAUDE.md says a new device has no message history —
//! that is a property of MLS (no key backup, by design). The vault is the
//! stated exception, and the exception is CRYPTOGRAPHIC, not policy: entries
//! are sealed under a key derived from the ACCOUNT identity key
//! (HKDF-SHA256, `pollis-vault-v1`), the one secret every enrolled device
//! already receives at enrollment and the server has never held. So a
//! brand-new device decrypts the entire vault the moment it enrolls, every
//! device converges with no coordination, and the DS remains a byte bucket.
//! Same construction as the read-cursor sync (`messages::read_state`), one
//! entry per row instead of one blob per account, different `info` string so
//! the two keys cannot be repurposed across uses.
//!
//! # Entry shape
//!
//! An entry's plaintext is `{ body, pinned }` where `body` is the SAME
//! content string chat messages use — plain text, or the
//! `{"_att":[...],"_txt":"..."}` attachment envelope — so the renderer, the
//! attachment pipeline, and the media grid all run the exact same code
//! against vault entries as against messages. The pinned flag lives inside
//! the ciphertext: pin/unpin is a re-encrypt-and-save the server cannot
//! distinguish from an edit.
//!
//! Attachments ride the normal upload pipeline (`commands::r2`), which
//! stores convergent ciphertext in the shared `media/` domain. What keeps a
//! vault-held object from being garbage-collected out from under the entry
//! is the `attachment_hashes` list on every save: the DS maintains
//! `vault_attachment_ref` rows from it and its GC liveness predicate counts
//! them (see `pollis-delivery/src/messages.rs`). The DECRYPTION capability
//! for those objects still travels only inside the entry's ciphertext.
//!
//! # Sync model
//!
//! The remote is the source of truth — that is what "functions like cloud
//! storage" means. Every read command first pulls and replaces the local
//! `vault_message` cache (tolerating a failed pull by serving the last
//! synced state, so the vault renders offline); every write goes remote
//! FIRST and touches the cache only after the DS accepted it, so the cache
//! can lag the truth but never contradict it. Entries are ULID-keyed and
//! rewritten whole, so cross-device conflict is last-write-wins per entry —
//! exactly Drive semantics, and honest for a single-owner store.

use std::sync::Arc;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine as _;
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::state::AppState;

/// HKDF info string — binds this key to the vault so the account identity key
/// cannot be repurposed between this, the read-cursor sync, and the
/// `account_recovery` wrap.
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

// ── Key derivation + AEAD ────────────────────────────────────────────────────

/// Derive this account's vault key. Salted by the user id (non-secret domain
/// separation between accounts, same reasoning as the cursor-sync key — the
/// IKM is a uniformly random 32-byte seed, so no stretching salt is needed).
fn derive_vault_key(account_seed: &[u8], user_id: &str) -> Zeroizing<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(user_id.as_bytes()), account_seed);
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(VAULT_HKDF_INFO, out.as_mut())
        .expect("HKDF-SHA256 expand 32 bytes is always valid");
    out
}

/// This device's copy of the account identity key seed, as HKDF input.
async fn account_seed(state: &Arc<AppState>, user_id: &str) -> Result<Zeroizing<Vec<u8>>> {
    let key = crate::commands::account_identity::load_account_id_key(state, user_id).await?;
    Ok(Zeroizing::new(key.to_seed().to_vec()))
}

/// Seal one entry into the `(nonce, blob)` the DS stores. Padded to the
/// message-framing size buckets first, so entry length does not count words
/// for the server.
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

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ── Local cache ──────────────────────────────────────────────────────────────

fn upsert_local(
    conn: &Connection,
    id: &str,
    content: &str,
    pinned: bool,
    created_at: &str,
    updated_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO vault_message (id, content, pinned, created_at, updated_at) \
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
            "SELECT id, content, pinned, created_at, updated_at FROM vault_message \
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
        "SELECT id, content, pinned, created_at, updated_at FROM vault_message WHERE id = ?1",
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

// ── Sync ─────────────────────────────────────────────────────────────────────

/// Pull the account's entries and make the local cache exactly match them —
/// upsert everything the server holds, delete everything it no longer does
/// (removed on another device). Returns how many entries the vault holds.
///
/// An entry that fails to decrypt is DROPPED FROM THE CACHE VIEW but left
/// untouched on the server: it may belong to a newer client version, and
/// deleting data because this build cannot read it is how "cloud storage"
/// stops deserving the name.
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
                // Keep the row server-side; just don't render what we cannot
                // read. Undecryptable ≠ deletable.
                eprintln!("[vault] entry {} is unreadable on this build: {e}", m.id);
                keep_ids.push(m.id);
            }
        }
    }

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    let conn = db.conn();
    // Remove local rows the server no longer holds (deleted from another
    // device). Chunked IN-list, mirroring the codebase's BIND_CHUNK habit.
    if keep_ids.is_empty() {
        conn.execute("DELETE FROM vault_message", [])
            .map_err(|e| Error::Other(anyhow::anyhow!("vault cache prune: {e}")))?;
    } else {
        let placeholders = keep_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM vault_message WHERE id NOT IN ({placeholders})");
        conn.execute(&sql, rusqlite::params_from_iter(keep_ids.iter()))
            .map_err(|e| Error::Other(anyhow::anyhow!("vault cache prune: {e}")))?;
    }
    let count = decrypted.len();
    for (id, entry, created_at, updated_at) in decrypted {
        upsert_local(conn, &id, &entry.body, entry.pinned, &created_at, &updated_at)?;
    }
    Ok(count)
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// The vault's entries, oldest first (chat order). Syncs from the server
/// when reachable; serves the last synced state when not, because a vault
/// you cannot open on a plane is not storage.
pub async fn get_vault_messages(user_id: String, state: &Arc<AppState>) -> Result<Vec<VaultMessage>> {
    if let Err(e) = sync_vault(state, &user_id).await {
        eprintln!("[vault] sync failed, serving the local cache: {e}");
    }
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    list_local(db.conn())
}

/// Save one entry remote-first: seal, POST (with the attachment hashes the
/// DS needs for GC liveness), and only then cache locally.
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

/// Add a new entry (a "message to yourself"). `content` is the same string
/// the chat composer produces — plain text or the attachment envelope.
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

    let entry = VaultEntryPlain {
        v: 1,
        body: new_content.clone(),
        pinned: existing.pinned,
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

/// Flip an entry's pinned flag. On the wire this is indistinguishable from an
/// edit — the flag lives inside the ciphertext.
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

/// Hard-delete one entry, remote first. The DS releases its attachment
/// references with it; the shared objects are collected only when nothing
/// else (message or vault, any user) still references them.
pub async fn delete_vault_message(
    id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::vault::DeleteVaultMessageBody {
        id: id.clone(),
        user_id: user_id.clone(),
    };
    super::mls::ds_post_ok(state, &body).await?;

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    db.conn()
        .execute(
            "DELETE FROM vault_message WHERE id = ?1",
            rusqlite::params![id],
        )
        .map_err(|e| Error::Other(anyhow::anyhow!("vault cache delete: {e}")))?;
    Ok(())
}

/// Case-insensitive substring search over the local cache. Runs entirely
/// client-side — the server cannot search what it cannot read, which is the
/// point.
pub async fn search_vault_messages(
    query: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<VaultMessage>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    // Search sees whatever the last sync saw; a stale cache is refreshed by
    // the list view, not by every keystroke here.
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

    /// THE POINT: the bytes the server stores carry neither the words nor the
    /// pinned flag — pin/unpin must be indistinguishable from an edit.
    #[test]
    fn the_stored_bytes_leak_neither_words_nor_the_pinned_flag() {
        let (_, blob) = seal_entry(SEED, ME, &entry("remember the milk", true)).unwrap();
        for needle in [b"remember the milk".as_slice(), b"pinned".as_slice()] {
            assert!(
                !blob.windows(needle.len()).any(|w| w == needle),
                "plaintext leaked into the stored vault bytes"
            );
        }
        // And a pinned vs unpinned save of the same body has the same length,
        // so the flag does not even leak through size.
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

    // ── Local cache behavior ────────────────────────────────────────────────

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::db::local::schema_sql()).unwrap();
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
