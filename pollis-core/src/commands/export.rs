//! On-device data export (#856): one conversation, or the whole account, as a
//! plaintext JSON archive written to a file the user picked.
//!
//! # What this is, and is not
//!
//! "Your data is yours" needs an exit that is not `delete_account`. This is
//! that exit, and it is also the honest answer to a data-access request. It is
//! **not** key backup and does not touch the no-history-sync rule: it is a
//! user-initiated read of plaintext this device has *already* decrypted, written
//! to local disk, and nothing else. Concretely:
//!
//! - **Strictly local.** Every read is a rusqlite query against the encrypted
//!   local DB. No DS call, no Turso, no R2. `tests::export_never_talks_to_the_network`
//!   scans this file for the network helpers so that stays true.
//! - **Per device.** A device only holds what it decrypted, so the archive is
//!   exactly that device's view — accepted losses (1) and (2) in CLAUDE.md apply
//!   to the export the same way they apply to the message list.
//! - **No key material, ever.** Ciphertext, MLS state, identity keys, pin keys
//!   and the device keystore are never read. `tests::archive_never_carries_key_material`
//!   seeds every secret-bearing table with a marker and asserts none of it reaches
//!   the file.
//! - **No re-import.** There is deliberately no command that reads one of these
//!   files back. An import path is exactly the backup channel the constraint on
//!   #856 forbids.
//!
//! # Attachments
//!
//! The archive records each attachment's metadata (name, type, size, content
//! hash, storage key) and the relative `file` its bytes go to, in a sibling
//! `<archive>-files/` directory. Bytes are written for every attachment whose
//! decrypted copy is **already in this device's media cache** — still zero
//! network, and still only what this device has already seen. The rest are
//! reported back in `ExportSummary::attachments_missing`, so the UI can offer
//! (and only ever *offer*) the opt-in fetch in `export_fetch.rs`. The content
//! hash is the convergent-encryption key for the object, so an archive is also
//! sufficient to fetch and decrypt every attachment later without any other
//! secret.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

/// Bumped when the JSON shape changes incompatibly. Readers key off this.
pub const ARCHIVE_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Archive {
    /// Always `"pollis-archive"`, so a reader can refuse an unrelated file.
    pub format: String,
    pub version: u32,
    pub exported_at: String,
    pub scope: ArchiveScope,
    pub account: ArchiveAccount,
    pub conversations: Vec<ArchiveConversation>,
    /// The Vault (#107). Account scope only — a conversation export omits it.
    pub vault: Vec<ArchiveVaultEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveScope {
    Account,
    Conversation { conversation_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveAccount {
    pub user_id: String,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveConversation {
    pub id: String,
    /// `"channel"` / `"dm"`, or `null` when the local name cache has no row.
    pub kind: Option<String>,
    pub name: Option<String>,
    pub group_id: Option<String>,
    pub group_name: Option<String>,
    /// The other party of a 1:1 DM, when this device recorded one.
    pub peer_user_id: Option<String>,
    pub messages: Vec<ArchiveMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveMessage {
    pub id: String,
    pub sender_id: String,
    pub sender_username: Option<String>,
    /// `null` for a deleted message — the tombstone is kept so the reader can
    /// see that something was here.
    pub text: Option<String>,
    pub attachments: Vec<ArchiveAttachment>,
    pub reply_to_id: Option<String>,
    pub thread_id: Option<String>,
    pub sent_at: String,
    pub received_at: String,
    pub edited_at: Option<String>,
    pub deleted_at: Option<String>,
    pub saved: bool,
    pub receipts: Vec<ArchiveReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveAttachment {
    pub name: Option<String>,
    pub content_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub content_hash: String,
    pub storage_key: String,
    /// Where the bytes live relative to the archive's directory, when they
    /// were exported. Deterministic from the hash and name, so the same
    /// attachment referenced twice is one file, and the opt-in fetch writes to
    /// the same place. The JSON never claims the file exists — the filesystem
    /// is the source of truth for that.
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveReceipt {
    pub reader_id: String,
    pub kind: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveVaultEntry {
    pub id: String,
    pub text: Option<String>,
    pub attachments: Vec<ArchiveAttachment>,
    pub pinned: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// What the UI reports after a successful export.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportSummary {
    pub path: String,
    pub conversations: usize,
    pub messages: usize,
    /// Attachment *references* across messages and vault entries.
    pub attachments: usize,
    pub vault_entries: usize,
    pub bytes: u64,
    /// The sibling directory attachment bytes were (or would be) written to.
    pub files_dir: String,
    /// Distinct attachments whose bytes came out of the local media cache.
    pub attachments_written: usize,
    /// Distinct attachments this device does not hold decrypted. The opt-in
    /// fetch takes exactly this list.
    pub attachments_missing: Vec<MissingAttachment>,
}

/// One attachment the export could not satisfy from the local cache.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissingAttachment {
    pub content_hash: String,
    pub storage_key: String,
    pub content_type: Option<String>,
    /// Relative to `files_dir`; always a single path component.
    pub file: String,
}

// ── Content envelope ──────────────────────────────────────────────────────

/// Split a stored `content` string into its text and attachment refs.
///
/// Plain text is returned as-is. The `{"_att":[...],"_txt":"..."}` envelope
/// (see `frontend/src/utils/attachmentEnvelope.ts`) is unpacked; anything that
/// starts with `{` but does not parse as that envelope is treated as text, so
/// a message that literally begins with a brace is not lost.
fn split_content(raw: &str) -> (Option<String>, Vec<ArchiveAttachment>) {
    if !raw.starts_with('{') {
        return (Some(raw.to_string()), Vec::new());
    }
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(raw) else {
        return (Some(raw.to_string()), Vec::new());
    };
    if !obj.contains_key("_att") && !obj.contains_key("_txt") {
        return (Some(raw.to_string()), Vec::new());
    }
    let text = obj
        .get("_txt")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let attachments = obj
        .get("_att")
        .and_then(|v| v.as_array())
        .map(|atts| {
            atts.iter()
                .filter_map(|a| {
                    let content_hash = a.get("hash")?.as_str()?.to_string();
                    let storage_key = a.get("key")?.as_str()?.to_string();
                    if content_hash.is_empty() || storage_key.is_empty() {
                        return None;
                    }
                    let name = a.get("name").and_then(|v| v.as_str()).map(str::to_string);
                    let content_type = a.get("ct").and_then(|v| v.as_str()).map(str::to_string);
                    Some(ArchiveAttachment {
                        file: attachment_file_name(&content_hash, name.as_deref(), content_type.as_deref()),
                        name,
                        content_type,
                        size_bytes: a.get("size").and_then(|v| v.as_u64()),
                        content_hash,
                        storage_key,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (text, attachments)
}

/// `<first 16 hex of hash>-<sanitised name>` — unique per attachment, readable
/// by a human, and safe to hand to any filesystem: one path component, ASCII
/// letters/digits/`.`/`-`/`_` only, no leading dot, capped in length. A
/// nameless attachment gets an extension from its content type instead.
pub fn attachment_file_name(content_hash: &str, name: Option<&str>, content_type: Option<&str>) -> String {
    let prefix: String = content_hash.chars().take(16).collect();
    let safe: String = name
        .unwrap_or("")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let safe = safe.trim_start_matches('.');
    if safe.is_empty() {
        let ext = content_type.map(crate::commands::r2::ext_for_content_type).unwrap_or("bin");
        return format!("{prefix}.{ext}");
    }
    // Keep the extension when truncating a very long name.
    let (stem, ext) = match safe.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && e.len() <= 8 => (s, Some(e)),
        _ => (safe, None),
    };
    let stem: String = stem.chars().take(80).collect();
    match ext {
        Some(e) => format!("{prefix}-{stem}.{e}"),
        None => format!("{prefix}-{stem}"),
    }
}

// ── Building ──────────────────────────────────────────────────────────────

fn conversation_filter(scope: &ArchiveScope) -> Option<&str> {
    match scope {
        ArchiveScope::Account => None,
        ArchiveScope::Conversation { conversation_id } => Some(conversation_id.as_str()),
    }
}

fn load_receipts(
    conn: &Connection,
    only: Option<&str>,
) -> Result<HashMap<String, Vec<ArchiveReceipt>>> {
    let mut stmt = conn.prepare(
        "SELECT r.message_id, r.reader_id, r.kind, r.at
           FROM message_receipt r
           JOIN message m ON m.id = r.message_id
          WHERE ?1 IS NULL OR m.conversation_id = ?1
          ORDER BY r.at ASC, r.kind ASC",
    )?;
    let mut out: HashMap<String, Vec<ArchiveReceipt>> = HashMap::new();
    let rows = stmt.query_map(rusqlite::params![only], |r| {
        Ok((
            r.get::<_, String>(0)?,
            ArchiveReceipt { reader_id: r.get(1)?, kind: r.get(2)?, at: r.get(3)? },
        ))
    })?;
    for row in rows {
        let (message_id, receipt) = row?;
        out.entry(message_id).or_default().push(receipt);
    }
    Ok(out)
}

fn load_bookmarks(conn: &Connection, only: Option<&str>) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT message_id FROM bookmark WHERE ?1 IS NULL OR conversation_id = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![only], |r| r.get::<_, String>(0))?;
    rows.collect::<std::result::Result<_, _>>().map_err(Into::into)
}

fn load_usernames(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT id, username FROM user_cache")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.collect::<std::result::Result<_, _>>().map_err(Into::into)
}

fn load_conversations(conn: &Connection, only: Option<&str>) -> Result<Vec<ArchiveConversation>> {
    // Every conversation this device holds a message for, whether or not the
    // name cache knows it: an unnamed conversation still exports its messages.
    let mut stmt = conn.prepare(
        "SELECT m.conversation_id, c.kind, c.name, c.group_id, c.group_name, d.peer_user_id
           FROM (SELECT DISTINCT conversation_id FROM message
                  WHERE ?1 IS NULL OR conversation_id = ?1) m
           LEFT JOIN conversation_cache c ON c.id = m.conversation_id
           LEFT JOIN dm_conversation d ON d.id = m.conversation_id
          ORDER BY c.group_name, c.name, m.conversation_id",
    )?;
    let rows = stmt.query_map(rusqlite::params![only], |r| {
        Ok(ArchiveConversation {
            id: r.get(0)?,
            kind: r.get(1)?,
            name: r.get(2)?,
            group_id: r.get(3)?,
            group_name: r.get(4)?,
            peer_user_id: r.get(5)?,
            messages: Vec::new(),
        })
    })?;
    rows.collect::<std::result::Result<_, _>>().map_err(Into::into)
}

fn load_messages(
    conn: &Connection,
    conversation_id: &str,
    usernames: &HashMap<String, String>,
    bookmarks: &HashSet<String>,
    receipts: &mut HashMap<String, Vec<ArchiveReceipt>>,
) -> Result<Vec<ArchiveMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, sender_id, content, reply_to_id, thread_id, sent_at, received_at,
                edited_at, deleted_at
           FROM message
          WHERE conversation_id = ?1
          ORDER BY sent_at ASC, id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![conversation_id], |r| {
        let id: String = r.get(0)?;
        let sender_id: String = r.get(1)?;
        let content: Option<String> = r.get(2)?;
        let deleted_at: Option<String> = r.get(8)?;
        let (text, attachments) = match (&content, &deleted_at) {
            (Some(raw), None) => split_content(raw),
            _ => (None, Vec::new()),
        };
        Ok(ArchiveMessage {
            sender_username: usernames.get(&sender_id).cloned(),
            saved: bookmarks.contains(&id),
            receipts: receipts.remove(&id).unwrap_or_default(),
            id,
            sender_id,
            text,
            attachments,
            reply_to_id: r.get(3)?,
            thread_id: r.get(4)?,
            sent_at: r.get(5)?,
            received_at: r.get(6)?,
            edited_at: r.get(7)?,
            deleted_at,
        })
    })?;
    rows.collect::<std::result::Result<_, _>>().map_err(Into::into)
}

fn load_vault(conn: &Connection) -> Result<Vec<ArchiveVaultEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, content, pinned, created_at, updated_at
           FROM vault_entry_cache ORDER BY created_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        let content: String = r.get(1)?;
        let (text, attachments) = split_content(&content);
        Ok(ArchiveVaultEntry {
            id: r.get(0)?,
            text,
            attachments,
            pinned: r.get::<_, i64>(2)? != 0,
            created_at: r.get(3)?,
            updated_at: r.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<_, _>>().map_err(Into::into)
}

/// Assemble the archive from the local database. Pure rusqlite, no I/O beyond
/// the connection — the unit tests drive this directly.
pub fn build_archive(conn: &Connection, user_id: &str, scope: ArchiveScope) -> Result<Archive> {
    let only = conversation_filter(&scope);
    let usernames = load_usernames(conn)?;
    let bookmarks = load_bookmarks(conn, only)?;
    let mut receipts = load_receipts(conn, only)?;
    let mut conversations = load_conversations(conn, only)?;
    for conversation in &mut conversations {
        conversation.messages =
            load_messages(conn, &conversation.id, &usernames, &bookmarks, &mut receipts)?;
    }
    let vault = match scope {
        ArchiveScope::Account => load_vault(conn)?,
        ArchiveScope::Conversation { .. } => Vec::new(),
    };
    Ok(Archive {
        format: "pollis-archive".to_string(),
        version: ARCHIVE_FORMAT_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        scope,
        account: ArchiveAccount {
            user_id: user_id.to_string(),
            username: usernames.get(user_id).cloned(),
        },
        conversations,
        vault,
    })
}

// ── Writing ───────────────────────────────────────────────────────────────

/// `<archive>.json` → sibling `<archive>-files/`.
pub fn files_dir_for(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("pollis-archive");
    path.with_file_name(format!("{stem}-files"))
}

/// Where the bytes of an already-decrypted attachment can be read from, and how
/// to read them. Injected so the writer is testable without the process-wide
/// cache root; the command passes the real cache lookup. The closure gets a
/// content hash and returns the cache file's bytes as stored (still under the
/// cache's own encryption) — decryption happens here, under `db_key`.
pub struct CachedBytes<'a> {
    pub db_key: &'a [u8],
    pub locate: &'a dyn Fn(&str) -> Option<PathBuf>,
}

/// Write the archive to `path`, and every attachment the local cache holds to
/// `files_dir_for(path)`. The path must be absolute (a save-dialog result); a
/// relative one would resolve against the process's working directory, which
/// is never where the user pointed.
///
/// The JSON is written to a sibling `.part` file and renamed into place, so a
/// failure mid-write never leaves a truncated archive that parses as an empty
/// account. Attachment files are written first, one by one; a file that fails
/// to decrypt or to verify is treated as missing rather than aborting the
/// export — the archive itself is the deliverable, the bytes are a bonus.
pub fn write_archive(path: &Path, archive: &Archive, cached: Option<CachedBytes<'_>>) -> Result<ExportSummary> {
    if !path.is_absolute() {
        return Err(Error::Other(anyhow::anyhow!("export path must be absolute")));
    }
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Other(anyhow::anyhow!("export path has no file name")))?;
    let files_dir = files_dir_for(path);

    // Distinct attachments, first reference wins for the name.
    let mut distinct: Vec<&ArchiveAttachment> = Vec::new();
    let mut seen = HashSet::new();
    let refs = archive
        .conversations
        .iter()
        .flat_map(|c| c.messages.iter().flat_map(|m| m.attachments.iter()))
        .chain(archive.vault.iter().flat_map(|v| v.attachments.iter()));
    for att in refs {
        if seen.insert(att.content_hash.as_str()) {
            distinct.push(att);
        }
    }

    let mut attachments_written = 0usize;
    let mut attachments_missing = Vec::new();
    if !distinct.is_empty() {
        crate::private_fs::create_dir_all(&files_dir)
            .map_err(|e| Error::Other(anyhow::anyhow!("could not create {}: {e}", files_dir.display())))?;
    }
    for att in &distinct {
        let written = cached
            .as_ref()
            .map(|c| write_cached_attachment(c, &files_dir, att))
            .unwrap_or(false);
        if written {
            attachments_written += 1;
        } else {
            attachments_missing.push(MissingAttachment {
                content_hash: att.content_hash.clone(),
                storage_key: att.storage_key.clone(),
                content_type: att.content_type.clone(),
                file: att.file.clone(),
            });
        }
    }

    let part = path.with_file_name(format!("{file_name}.part"));
    let result = (|| -> std::io::Result<()> {
        let file = crate::private_fs::create_file(&part)?;
        let mut writer = std::io::BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, archive)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        std::fs::rename(&part, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(Error::Other(anyhow::anyhow!("could not write archive: {e}")));
    }
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(ExportSummary {
        path: path.to_string_lossy().into_owned(),
        conversations: archive.conversations.len(),
        messages: archive.conversations.iter().map(|c| c.messages.len()).sum(),
        attachments: archive
            .conversations
            .iter()
            .flat_map(|c| c.messages.iter())
            .map(|m| m.attachments.len())
            .sum::<usize>()
            + archive.vault.iter().map(|v| v.attachments.len()).sum::<usize>(),
        vault_entries: archive.vault.len(),
        bytes,
        files_dir: files_dir.to_string_lossy().into_owned(),
        attachments_written,
        attachments_missing,
    })
}

/// Decrypt one cache entry and write it under its archive name. `false` for
/// "not in the cache" and for every failure — the caller only needs to know
/// whether the bytes are now next to the archive.
fn write_cached_attachment(cached: &CachedBytes<'_>, files_dir: &Path, att: &ArchiveAttachment) -> bool {
    use sha2::{Digest, Sha256};
    let Some(source) = (cached.locate)(&att.content_hash) else {
        return false;
    };
    let Ok(stored) = std::fs::read(&source) else {
        return false;
    };
    let Ok(plaintext) =
        crate::commands::r2::cache_decrypt(&stored, cached.db_key, att.content_hash.as_bytes())
    else {
        return false;
    };
    // The cache entry was hash-verified when it was written, but the export is
    // the one artefact a person will keep, so check again before vouching.
    if hex::encode(Sha256::digest(&plaintext)) != att.content_hash {
        return false;
    }
    crate::private_fs::write(&files_dir.join(&att.file), plaintext).is_ok()
}

// ── Bundling (mobile) ─────────────────────────────────────────────────────

/// Zip `<archive>.json` and its `<archive>-files/` directory into a single
/// `<archive>.zip` beside them, for platforms whose only exit is a share sheet
/// that takes ONE file (mobile). Desktop keeps the loose layout — a folder next
/// to the JSON is the more useful shape when the user chose the destination.
///
/// Pure local I/O. The zip holds `archive.json` at its root and the attachment
/// files under `files/`, so a reader finds the same relative `file` paths the
/// JSON records once it strips the `-files/` directory name.
pub fn bundle_archive(archive_path: &Path) -> Result<PathBuf> {
    use std::io::Read;
    if !archive_path.is_absolute() {
        return Err(Error::Other(anyhow::anyhow!("archive path must be absolute")));
    }
    let files_dir = files_dir_for(archive_path);
    let zip_path = archive_path.with_extension("zip");
    let result = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
        let out = crate::private_fs::create_file(&zip_path)?;
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(out));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("archive.json", opts)?;
        let mut json = std::fs::File::open(archive_path)?;
        std::io::copy(&mut json, &mut zip)?;
        if let Ok(entries) = std::fs::read_dir(&files_dir) {
            let mut names: Vec<_> = entries
                .flatten()
                .filter(|e| e.path().is_file())
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .collect();
            names.sort();
            for name in names {
                zip.start_file(format!("files/{name}"), opts)?;
                let mut f = std::fs::File::open(files_dir.join(&name))?;
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)?;
                zip.write_all(&buf)?;
            }
        }
        zip.finish()?.flush()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&zip_path);
        return Err(Error::Other(anyhow::anyhow!("could not bundle archive: {e}")));
    }
    Ok(zip_path)
}

// ── Command ───────────────────────────────────────────────────────────────

/// Export this device's decrypted history to `path` as JSON. `conversation_id`
/// narrows it to one conversation; `None` is the full account archive.
pub async fn export_archive(
    path: String,
    conversation_id: Option<String>,
    state: &Arc<AppState>,
) -> Result<ExportSummary> {
    let (user_id, db_key) = {
        let guard = state.unlock.lock().await;
        guard
            .as_ref()
            .map(|u| (u.user_id.clone(), u.db_key.clone()))
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?
    };
    let scope = match conversation_id {
        Some(conversation_id) => ArchiveScope::Conversation { conversation_id },
        None => ArchiveScope::Account,
    };
    // The whole build runs under the DB lock with no `.await` inside, so the
    // non-`Send` connection never crosses a suspension point.
    let archive = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        build_archive(db.conn(), &user_id, scope)?
    };
    // The cache is looked up for the unlocked user BY NAME, never through the
    // ambient cache user — see `find_cached_file_for_user` and #1000.
    let locate = |hash: &str| {
        crate::commands::r2::find_cached_file_for_user(&user_id, hash).map(|(p, _)| p)
    };
    write_archive(
        Path::new(&path),
        &archive,
        Some(CachedBytes { db_key: &db_key, locate: &locate }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::local::LocalDb;

    fn db() -> LocalDb {
        LocalDb::open_in_memory().expect("in-memory db")
    }

    fn seed_message(conn: &Connection, id: &str, conversation_id: &str, sender: &str, content: &str) {
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES (?1, ?2, ?3, X'01', ?4, ?5)",
            rusqlite::params![id, conversation_id, sender, content, "2024-01-01T00:00:00Z"],
        )
        .expect("seed message");
    }

    fn seed_fixture(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO user_cache (id, username) VALUES ('me', 'dan'), ('alice', 'alice');
             INSERT INTO conversation_cache (id, kind, name, group_id, group_name)
                  VALUES ('chan-1', 'channel', 'general', 'grp-1', 'Pollis');
             INSERT INTO conversation_cache (id, kind, name) VALUES ('dm-1', 'dm', 'alice');
             INSERT INTO dm_conversation (id, peer_user_id) VALUES ('dm-1', 'alice');
             INSERT INTO vault_entry_cache (id, content, pinned, created_at, updated_at)
                  VALUES ('v1', 'a note', 1, '2024-02-01T00:00:00Z', '2024-02-01T00:00:00Z');",
        )
        .expect("fixture");
        seed_message(conn, "m1", "chan-1", "alice", "hello");
        seed_message(
            conn,
            "m2",
            "chan-1",
            "me",
            r#"{"_att":[{"key":"r2/abc","name":"cat.png","ct":"image/png","size":123,"hash":"deadbeef"}],"_txt":"look"}"#,
        );
        seed_message(conn, "m3", "dm-1", "alice", "secret dm");
        conn.execute(
            "INSERT INTO message_receipt (message_id, reader_id, kind, at)
             VALUES ('m3', 'me', 'read', '2024-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO bookmark (message_id, conversation_id) VALUES ('m1', 'chan-1')", [])
            .unwrap();
    }

    #[test]
    fn account_archive_carries_every_conversation_the_device_holds() {
        let db = db();
        seed_fixture(db.conn());
        let archive = build_archive(db.conn(), "me", ArchiveScope::Account).unwrap();

        assert_eq!(archive.format, "pollis-archive");
        assert_eq!(archive.version, ARCHIVE_FORMAT_VERSION);
        assert_eq!(archive.account.username.as_deref(), Some("dan"));
        assert_eq!(archive.conversations.len(), 2);

        let chan = archive.conversations.iter().find(|c| c.id == "chan-1").unwrap();
        assert_eq!(chan.group_name.as_deref(), Some("Pollis"));
        assert_eq!(chan.name.as_deref(), Some("general"));
        assert_eq!(chan.messages.len(), 2);
        let m1 = &chan.messages[0];
        assert_eq!(m1.text.as_deref(), Some("hello"));
        assert_eq!(m1.sender_username.as_deref(), Some("alice"));
        assert!(m1.saved);
        let m2 = &chan.messages[1];
        assert_eq!(m2.text.as_deref(), Some("look"));
        assert_eq!(m2.attachments.len(), 1);
        assert_eq!(m2.attachments[0].name.as_deref(), Some("cat.png"));
        assert_eq!(m2.attachments[0].size_bytes, Some(123));
        assert_eq!(m2.attachments[0].storage_key, "r2/abc");

        let dm = archive.conversations.iter().find(|c| c.id == "dm-1").unwrap();
        assert_eq!(dm.peer_user_id.as_deref(), Some("alice"));
        // The read-implies-delivered trigger materialises the second row.
        let kinds: Vec<&str> = dm.messages[0].receipts.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds, ["delivered", "read"]);

        assert_eq!(archive.vault.len(), 1);
        assert_eq!(archive.vault[0].text.as_deref(), Some("a note"));
        assert!(archive.vault[0].pinned);
    }

    #[test]
    fn conversation_scope_exports_that_conversation_and_nothing_else() {
        let db = db();
        seed_fixture(db.conn());
        let archive = build_archive(
            db.conn(),
            "me",
            ArchiveScope::Conversation { conversation_id: "dm-1".into() },
        )
        .unwrap();
        assert_eq!(archive.conversations.len(), 1);
        assert_eq!(archive.conversations[0].id, "dm-1");
        assert!(archive.vault.is_empty(), "vault is account-level, never part of one conversation");
        let json = serde_json::to_string(&archive).unwrap();
        assert!(!json.contains("hello"), "channel text leaked into a DM export");
    }

    #[test]
    fn an_unnamed_conversation_still_exports_its_messages() {
        let db = db();
        seed_message(db.conn(), "m9", "conv-unknown", "alice", "orphan");
        let archive = build_archive(db.conn(), "me", ArchiveScope::Account).unwrap();
        assert_eq!(archive.conversations.len(), 1);
        assert_eq!(archive.conversations[0].name, None);
        assert_eq!(archive.conversations[0].messages[0].text.as_deref(), Some("orphan"));
    }

    #[test]
    fn a_deleted_message_is_a_tombstone_not_its_last_text() {
        let db = db();
        seed_message(db.conn(), "m1", "c", "alice", "was here");
        db.conn()
            .execute("UPDATE message SET deleted_at = '2024-01-03T00:00:00Z' WHERE id = 'm1'", [])
            .unwrap();
        let archive = build_archive(db.conn(), "me", ArchiveScope::Account).unwrap();
        let m = &archive.conversations[0].messages[0];
        assert_eq!(m.text, None);
        assert!(m.deleted_at.is_some());
    }

    #[test]
    fn split_content_keeps_a_literal_brace_message() {
        assert_eq!(split_content("{not json").0.as_deref(), Some("{not json"));
        assert_eq!(split_content(r#"{"a":1}"#).0.as_deref(), Some(r#"{"a":1}"#));
        let (text, atts) = split_content(r#"{"_att":[],"_txt":"just text"}"#);
        assert_eq!(text.as_deref(), Some("just text"));
        assert!(atts.is_empty());
        let (text, atts) = split_content(r#"{"_att":[{"key":"k","hash":"h"}]}"#);
        assert_eq!(text, None);
        assert_eq!(atts.len(), 1);
    }

    #[test]
    fn archive_never_carries_key_material() {
        let db = db();
        let conn = db.conn();
        conn.execute_batch(
            "INSERT INTO identity_key (id, public_key) VALUES (1, X'49444B4D41524B4552');
             INSERT INTO mls_kv (scope, key, value) VALUES ('group', X'01', X'4D4C534D41524B4552');
             INSERT INTO pin_key_cache (conversation_id, kpin) VALUES ('c', X'4B50494E4D41524B4552');
             INSERT INTO kv (key, value) VALUES ('session_token', 'KVMARKER');
             INSERT INTO contact_verification (peer_user_id, account_id_pub, identity_version)
                  VALUES ('alice', X'434F4E544143544D41524B4552', 1);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES ('m1', 'c', 'alice', X'43495048455254455854', 'plain', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let archive = build_archive(conn, "me", ArchiveScope::Account).unwrap();
        let json = serde_json::to_string(&archive).unwrap();
        for marker in ["IDKMARKER", "MLSMARKER", "KPINMARKER", "KVMARKER", "CONTACTMARKER", "CIPHERTEXT"] {
            assert!(!json.contains(marker), "{marker} reached the archive");
        }
        for base64ish in ["SURLTUFSS0VS", "TUxTTUFSS0VS", "Q0lQSEVSVEVYVA"] {
            assert!(!json.contains(base64ish), "an encoded secret reached the archive");
        }
        assert!(!json.contains("ciphertext"), "the ciphertext column must not be exported at all");
        assert!(json.contains("plain"));
    }

    #[test]
    fn write_is_atomic_and_round_trips() {
        let db = db();
        seed_fixture(db.conn());
        let archive = build_archive(db.conn(), "me", ArchiveScope::Account).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pollis-export.json");
        let summary = write_archive(&path, &archive, None).unwrap();

        assert_eq!(summary.conversations, 2);
        assert_eq!(summary.messages, 3);
        assert_eq!(summary.attachments, 1);
        assert_eq!(summary.vault_entries, 1);
        assert!(summary.bytes > 0);
        assert!(!dir.path().join("pollis-export.json.part").exists(), ".part left behind");
        assert_eq!(summary.files_dir, dir.path().join("pollis-export-files").to_string_lossy());
        assert_eq!(summary.attachments_written, 0);
        assert_eq!(summary.attachments_missing.len(), 1);
        assert_eq!(summary.attachments_missing[0].file, "deadbeef-cat.png");

        let read_back: Archive =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(read_back, archive);
    }

    #[test]
    fn a_bundle_holds_the_json_and_every_exported_file() {
        let db = db();
        seed_fixture(db.conn());
        let archive = build_archive(db.conn(), "me", ArchiveScope::Account).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.json");
        write_archive(&path, &archive, None).unwrap();
        std::fs::write(dir.path().join("archive-files").join("deadbeef-cat.png"), b"cat").unwrap();

        let zip_path = bundle_archive(&path).unwrap();
        assert_eq!(zip_path, dir.path().join("archive.zip"));
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
        let names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_string()).collect();
        assert_eq!(names, ["archive.json", "files/deadbeef-cat.png"]);
        let mut json = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("archive.json").unwrap(), &mut json).unwrap();
        let read_back: Archive = serde_json::from_str(&json).unwrap();
        assert_eq!(read_back, archive);
        assert!(bundle_archive(Path::new("relative.json")).is_err());
    }

    #[test]
    fn a_relative_path_is_refused() {
        let db = db();
        let archive = build_archive(db.conn(), "me", ArchiveScope::Account).unwrap();
        let err = write_archive(Path::new("relative.json"), &archive, None).unwrap_err();
        assert!(err.to_string().contains("absolute"));
        assert!(!Path::new("relative.json").exists());
    }

    #[test]
    fn a_failed_write_leaves_no_partial_file() {
        let db = db();
        let archive = build_archive(db.conn(), "me", ArchiveScope::Account).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-dir").join("export.json");
        assert!(write_archive(&path, &archive, None).is_err());
        assert!(!path.exists());
        assert!(!dir.path().join("missing-dir").exists());
    }

    #[test]
    fn attachment_file_names_are_one_safe_path_component() {
        let h = "0123456789abcdef0123456789abcdef";
        assert_eq!(attachment_file_name(h, Some("cat.png"), None), "0123456789abcdef-cat.png");
        assert_eq!(attachment_file_name(h, Some("../../etc/passwd"), None), "0123456789abcdef-_.._etc_passwd");
        assert_eq!(attachment_file_name(h, Some(".hidden"), None), "0123456789abcdef-hidden");
        assert_eq!(attachment_file_name(h, Some("my photo (1).JPG"), None), "0123456789abcdef-my_photo__1_.JPG");
        assert_eq!(attachment_file_name(h, None, Some("image/png")), "0123456789abcdef.png");
        assert_eq!(attachment_file_name(h, Some(""), None), "0123456789abcdef.bin");
        let long = format!("{}.txt", "a".repeat(300));
        let out = attachment_file_name(h, Some(&long), None);
        assert!(out.len() < 120 && out.ends_with(".txt"));
        for name in [out.as_str(), "0123456789abcdef-_.._etc_passwd"] {
            assert!(!name.contains('/') && !name.contains('\\'));
        }
    }

    /// A cache entry is written next to the archive, decrypted, hash-verified,
    /// once — however many messages reference it.
    #[test]
    fn cached_attachments_are_written_beside_the_archive() {
        use sha2::{Digest, Sha256};
        let db = db();
        let plaintext = b"the real cat bytes".to_vec();
        let hash = hex::encode(Sha256::digest(&plaintext));
        let db_key = [7u8; 32];
        let content = format!(
            r#"{{"_att":[{{"key":"r2/cat","name":"cat.png","ct":"image/png","size":18,"hash":"{hash}"}}],"_txt":"cat"}}"#
        );
        seed_message(db.conn(), "m1", "c", "alice", &content);
        seed_message(db.conn(), "m2", "c", "alice", &content);
        let archive = build_archive(db.conn(), "me", ArchiveScope::Account).unwrap();

        let cache = tempfile::tempdir().unwrap();
        let entry = cache.path().join(format!("{hash}.png.enc"));
        let sealed = crate::commands::r2::cache_encrypt(plaintext.clone(), &db_key, hash.as_bytes()).unwrap();
        std::fs::write(&entry, sealed).unwrap();
        let locate = |h: &str| if h == hash { Some(entry.clone()) } else { None };

        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("archive.json");
        let summary =
            write_archive(&path, &archive, Some(CachedBytes { db_key: &db_key, locate: &locate })).unwrap();

        assert_eq!(summary.attachments, 2, "two references");
        assert_eq!(summary.attachments_written, 1, "one distinct file");
        assert!(summary.attachments_missing.is_empty());
        let file = archive.conversations[0].messages[0].attachments[0].file.clone();
        assert_eq!(std::fs::read(out.path().join("archive-files").join(&file)).unwrap(), plaintext);
    }

    /// A cache entry that does not decrypt under this user's key, or whose
    /// bytes do not match their hash, is reported missing — never written.
    #[test]
    fn an_unverifiable_cache_entry_is_missing_not_exported() {
        use sha2::{Digest, Sha256};
        let db = db();
        let hash = hex::encode(Sha256::digest(b"what the sender meant"));
        let content = format!(r#"{{"_att":[{{"key":"r2/x","name":"x.bin","hash":"{hash}"}}]}}"#);
        seed_message(db.conn(), "m1", "c", "alice", &content);
        let archive = build_archive(db.conn(), "me", ArchiveScope::Account).unwrap();

        let cache = tempfile::tempdir().unwrap();
        let entry = cache.path().join(format!("{hash}.bin.enc"));
        let locate = |_: &str| Some(entry.clone());
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("archive.json");

        // Wrong key.
        let sealed = crate::commands::r2::cache_encrypt(b"what the sender meant".to_vec(), &[1u8; 32], hash.as_bytes()).unwrap();
        std::fs::write(&entry, sealed).unwrap();
        let summary =
            write_archive(&path, &archive, Some(CachedBytes { db_key: &[2u8; 32], locate: &locate })).unwrap();
        assert_eq!(summary.attachments_written, 0);
        assert_eq!(summary.attachments_missing.len(), 1);

        // Right key, substituted bytes.
        let sealed = crate::commands::r2::cache_encrypt(b"something else".to_vec(), &[2u8; 32], hash.as_bytes()).unwrap();
        std::fs::write(&entry, sealed).unwrap();
        let summary =
            write_archive(&path, &archive, Some(CachedBytes { db_key: &[2u8; 32], locate: &locate })).unwrap();
        assert_eq!(summary.attachments_written, 0);
        assert!(std::fs::read_dir(out.path().join("archive-files")).unwrap().next().is_none());
    }

    /// The constraint on #856: strictly local. Nothing in this module may reach
    /// for the DS client, the directory reads, R2, or an HTTP client.
    #[test]
    fn export_never_talks_to_the_network() {
        let src = include_str!("export.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        for banned in ["ds_client", "ds_post", "ds_reads", "reqwest", "presign", "r2_get_url", "download_media", "get_media_url", "libsql"] {
            assert!(!body.contains(banned), "export.rs must stay device-local; found `{banned}`");
        }
    }

    /// #1093: an export is the one artefact that holds full plaintext history
    /// and every attachment, written wherever the user pointed the picker —
    /// often a shared-machine directory. Every write here must go through
    /// `private_fs` (0600/0700), never the process umask. Scanning the source
    /// rather than the output is what keeps the NEXT write site honest.
    #[test]
    fn export_never_writes_outside_private_fs() {
        for (name, src) in [
            ("export.rs", include_str!("export.rs")),
            ("export_fetch.rs", include_str!("export_fetch.rs")),
        ] {
            let body = src.split("#[cfg(test)]").next().unwrap();
            for banned in [
                "std::fs::write",
                "std::fs::File::create",
                "std::fs::create_dir_all",
                "tokio::fs::write",
                "OpenOptions",
            ] {
                assert!(
                    !body.contains(banned),
                    "{name} must write through private_fs; found `{banned}`"
                );
            }
        }
    }

    /// And the behaviour the scan is standing in for: the archive is owner-only
    /// on a platform that says so by mode. `#[cfg(unix)]` because the mode
    /// accessor itself is unix-only — a runtime guard would still leave the
    /// `PermissionsExt` import to fail the Windows build.
    #[cfg(unix)]
    #[test]
    fn a_written_archive_is_owner_only() {
        if !crate::private_fs::owner_only_is_enforced_by_mode() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let db = db();
        seed_fixture(db.conn());
        let archive = build_archive(db.conn(), "me", ArchiveScope::Account).unwrap();
        let out = tempfile::tempdir().unwrap();
        let path = out.path().join("archive.json");
        write_archive(&path, &archive, None).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            crate::private_fs::FILE_MODE,
            "the export archive must be 0600, not umask"
        );
    }

    /// The other half of the constraint: no re-import path. A reader for this
    /// format would be the backup channel #856 forbids.
    #[test]
    fn there_is_no_import_command() {
        let src = include_str!("export.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        assert!(!body.contains("pub async fn import"), "no import command may exist");
        assert!(!body.contains("fn restore"), "no restore path may exist");
    }
}
