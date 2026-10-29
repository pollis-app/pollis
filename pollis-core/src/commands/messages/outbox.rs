//! The durable outbox for outgoing messages (#1335).
//!
//! The outbox is the `message` row itself, written first with
//! `send_state = 'pending'` and an empty ciphertext, so every read path shows
//! the message across restarts with no second copy. Delivery fills the
//! ciphertext, posts and clears the state; a failed attempt sets `'failed'`.
//! Unsent rows are re-delivered by [`flush_outbox`] (after unlock, on
//! reconnect, on mobile foreground) and [`retry_message`]; nothing polls.
//!
//! Idempotency: a retry reuses the message id, which is the envelope id, so the
//! DS answers a re-post of a landed envelope as a replay. In-process, [`deliver`]
//! holds the message's lock in `AppState::outbox_locks` for the whole attempt
//! and re-reads the row under it.

use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::Message;

/// `message.send_state` for a stored message not yet confirmed by the DS,
/// including an attachment send waiting on its conversation's file key
/// (`file_keys::FileKeyUnavailable`): a wait, not a failure.
pub(crate) const PENDING: &str = "pending";
/// `message.send_state` after a failure a later attempt may not repeat
/// (offline, a 5xx, an outdated client).
pub(crate) const FAILED: &str = "failed";
/// `message.send_state` after a refusal no retry fixes (not a member, the
/// conversation is gone, a malformed body). The drain leaves these to the user.
pub(crate) const REJECTED: &str = "rejected";

/// Whether `e` is a refusal no retry can fix (see [`REJECTED`]): a DS 400, 403,
/// 404, 410 or 422, or [`NotAMember`].
pub(super) fn is_permanent(e: &Error) -> bool {
    let Error::Other(any) = e else {
        return false;
    };
    any.downcast_ref::<NotAMember>().is_some()
        || any
            .downcast_ref::<crate::commands::mls::ds_client::DsRefused>()
            .is_some_and(|r| matches!(r.status.as_u16(), 400 | 403 | 404 | 410 | 422))
}

/// The DS resolve answered that the sender is not a member. Typed so
/// [`is_permanent`] need not read a message.
#[derive(Debug)]
pub(super) struct NotAMember;

impl std::fmt::Display for NotAMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("you are not a member of this conversation")
    }
}

impl std::error::Error for NotAMember {}

pub(super) fn not_a_member() -> Error {
    Error::Other(anyhow::Error::new(NotAMember))
}

/// The one way a row is soft-deleted. It also clears `send_state`: a delete can
/// only target an envelope that exists, and an unsent row must keep its content
/// (the CHECK), so leaving the state set would fail the redaction and wedge the
/// ingest cursor (#1335). Binds `?1` = deleted_at, `?2` = id.
pub(crate) const SOFT_DELETE_SQL: &str = "UPDATE message SET content = NULL, deleted_at = ?1, send_state = NULL
     WHERE id = ?2 AND deleted_at IS NULL";

/// An outgoing message as the outbox stores it.
#[derive(Debug, Clone)]
pub(super) struct Queued {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub content: String,
    pub reply_to_id: Option<String>,
    pub thread_id: Option<String>,
    /// Refreshed by every attempt until the first seal (a message queued
    /// offline lands when it went out), then fixed, so a retry after a
    /// crash-after-post matches what recipients hold.
    pub sent_at: String,
}

/// One attachment a send hands to the outbox before upload (#1335): exactly one
/// of `path` (a user-chosen file, already scope-checked by the shell) or
/// `staged_id` (a paste staged in memory). Its bytes are stored locally before
/// the message row and before any network work.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutgoingAttachment {
    pub path: Option<String>,
    pub staged_id: Option<String>,
    pub filename: String,
    pub content_type: String,
    /// Renderer-computed (a video's poster frame); the upload's own value wins.
    pub blurhash: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// A stored attachment without its bytes. The bytes are read one attachment at
/// a time, when uploaded.
#[derive(Debug, Clone)]
pub(super) struct AttMeta {
    pub filename: String,
    pub content_type: String,
    pub size: usize,
    pub blurhash: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// The most one message may carry through the outbox, all attachments
/// together.
pub(crate) const MAX_OUTBOX_BYTES_PER_MESSAGE: u64 = 4 * pollis_api::broker::R2_MEDIA_MAX_BYTES;

fn over_cap(size: u64) -> Option<Error> {
    (size > pollis_api::broker::R2_MEDIA_MAX_BYTES).then(|| {
        Error::Other(anyhow::anyhow!(
            "attachment is {size} bytes; the limit is {} bytes",
            pollis_api::broker::R2_MEDIA_MAX_BYTES
        ))
    })
}

/// Size every attachment from metadata and refuse anything the upload would
/// refuse before a byte of it is read.
async fn plan_attachments(attachments: &[OutgoingAttachment]) -> Result<Vec<AttMeta>> {
    let mut metas = Vec::with_capacity(attachments.len());
    let mut total: u64 = 0;
    for att in attachments {
        let size = match (&att.path, &att.staged_id) {
            (Some(path), None) => tokio::fs::metadata(path)
                .await
                .map_err(|e| Error::Other(anyhow::anyhow!("read attachment {}: {e}", att.filename)))?
                .len(),
            (None, Some(id)) => crate::commands::staging::peek_staged(id)
                .map(|bytes| bytes.len() as u64)
                .ok_or_else(|| no_staged(id))?,
            _ => {
                return Err(Error::Other(anyhow::anyhow!(
                    "an attachment needs exactly one of path or stagedId"
                )))
            }
        };
        if let Some(e) = over_cap(size) {
            return Err(e);
        }
        total = total.saturating_add(size);
        if total > MAX_OUTBOX_BYTES_PER_MESSAGE {
            return Err(Error::Other(anyhow::anyhow!(
                "the attachments add up to more than {MAX_OUTBOX_BYTES_PER_MESSAGE} bytes"
            )));
        }
        metas.push(AttMeta {
            filename: att.filename.clone(),
            content_type: att.content_type.clone(),
            size: size as usize,
            blurhash: att.blurhash.clone(),
            width: att.width,
            height: att.height,
        });
    }
    Ok(metas)
}

fn no_staged(id: &str) -> Error {
    Error::Other(anyhow::anyhow!(
        "no staged attachment {id}: it was already sent, discarded, or the session ended"
    ))
}

const INSERT_ATTACHMENT_SQL: &str = "INSERT OR REPLACE INTO outbox_attachment
       (message_id, position, filename, content_type, data, blurhash, width, height)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

pub(super) fn store_attachment_on(
    conn: &rusqlite::Connection,
    id: &str,
    position: usize,
    meta: &AttMeta,
    data: &[u8],
) -> Result<()> {
    conn.execute(
        INSERT_ATTACHMENT_SQL,
        rusqlite::params![
            id,
            position as i64,
            meta.filename,
            meta.content_type,
            data,
            meta.blurhash,
            meta.width,
            meta.height,
        ],
    )?;
    Ok(())
}

/// Store one attachment's bytes under the database lock for that one row only,
/// so a large message never holds the lock across all of its files.
async fn store_attachment(
    state: &Arc<AppState>,
    id: &str,
    position: usize,
    att: &OutgoingAttachment,
    meta: &AttMeta,
) -> Result<()> {
    match (&att.path, &att.staged_id) {
        (Some(path), None) => {
            let data = tokio::fs::read(path).await.map_err(|e| {
                Error::Other(anyhow::anyhow!("read attachment {}: {e}", att.filename))
            })?;
            // The file may have grown since it was sized.
            if let Some(e) = over_cap(data.len() as u64) {
                return Err(e);
            }
            let meta = AttMeta { size: data.len(), ..meta.clone() };
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().ok_or_else(not_signed_in)?;
            store_attachment_on(db.conn(), id, position, &meta, &data)
        }
        (None, Some(staged)) => {
            let bytes = crate::commands::staging::peek_staged(staged).ok_or_else(|| no_staged(staged))?;
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().ok_or_else(not_signed_in)?;
            store_attachment_on(db.conn(), id, position, meta, &bytes)
        }
        _ => Err(Error::Other(anyhow::anyhow!(
            "an attachment needs exactly one of path or stagedId"
        ))),
    }
}
/// One `_att` entry exactly as the renderers encode it
/// (`frontend/src/utils/attachmentEnvelope.ts`, `mobile/lib/attachments.ts`):
/// same keys, same order, absent values omitted.
#[derive(Serialize)]
struct AttWire<'a> {
    key: &'a str,
    url: &'a str,
    name: &'a str,
    ct: &'a str,
    size: usize,
    /// The object's random file id (not a content hash, #1382).
    hash: &'a str,
    /// Which key of the conversation's file key ring sealed it.
    kv: i64,
    /// SHA-256 of the plaintext, checked after decrypt.
    sha: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bh: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    w: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    h: Option<u32>,
}

/// An entry not uploaded yet. No `key`, which is how both renderers tell an
/// attachment still uploading.
#[derive(Serialize)]
struct PendingAttWire<'a> {
    name: &'a str,
    ct: &'a str,
    size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    bh: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    w: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    h: Option<u32>,
}

#[derive(Serialize)]
struct Envelope<'a, A> {
    #[serde(rename = "_att")]
    att: Vec<A>,
    /// Omitted, not `""`, when there is no caption.
    #[serde(rename = "_txt", skip_serializing_if = "Option::is_none")]
    txt: Option<&'a str>,
}

fn caption(text: &str) -> Option<&str> {
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The content an unsent attachment message holds until its upload is done.
fn provisional_content(text: &str, metas: &[AttMeta]) -> String {
    let env = Envelope {
        att: metas
            .iter()
            .map(|a| PendingAttWire {
                name: &a.filename,
                ct: &a.content_type,
                size: a.size,
                bh: a.blurhash.as_deref(),
                w: a.width,
                h: a.height,
            })
            .collect(),
        txt: caption(text),
    };
    serde_json::to_string(&env).expect("an envelope always serializes")
}

/// The finished envelope, once every attachment is uploaded.
fn final_content(
    text: &str,
    metas: &[AttMeta],
    uploaded: &[crate::commands::r2::MediaUploadResult],
) -> String {
    let env = Envelope {
        att: metas
            .iter()
            .zip(uploaded)
            .map(|(a, r)| AttWire {
                key: &r.key,
                url: &r.url,
                name: &r.filename,
                ct: &r.content_type,
                size: r.size_bytes,
                hash: &r.content_hash,
                kv: r.key_version,
                sha: &r.sha256,
                bh: r.blurhash.as_deref().or(a.blurhash.as_deref()),
                w: r.width.or(a.width),
                h: r.height.or(a.height),
            })
            .collect(),
        txt: caption(text),
    };
    serde_json::to_string(&env).expect("an envelope always serializes")
}

/// The caption a provisional envelope carries.
fn caption_of(content: &str) -> String {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| v.get("_txt").and_then(|t| t.as_str()).map(str::to_string))
        .unwrap_or_default()
}

/// The stored attachments of `id`, in order, without their bytes.
pub(super) fn attachment_metas_on(conn: &rusqlite::Connection, id: &str) -> Result<Vec<AttMeta>> {
    let mut stmt = conn.prepare(
        "SELECT filename, content_type, length(data), blurhash, width, height
         FROM outbox_attachment WHERE message_id = ?1 ORDER BY position",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![id], |row| {
            Ok(AttMeta {
                filename: row.get(0)?,
                content_type: row.get(1)?,
                size: row.get::<_, i64>(2)? as usize,
                blurhash: row.get(3)?,
                width: row.get(4)?,
                height: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The bytes of the `n`th stored attachment of `id`, one blob at a time.
fn attachment_data_on(conn: &rusqlite::Connection, id: &str, n: usize) -> Result<Vec<u8>> {
    Ok(conn.query_row(
        "SELECT data FROM outbox_attachment WHERE message_id = ?1
         ORDER BY position LIMIT 1 OFFSET ?2",
        rusqlite::params![id, n as i64],
        |row| row.get(0),
    )?)
}

/// Remove an unsent message together with any bytes it still holds.
fn remove_unsent_on(conn: &rusqlite::Connection, id: &str) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM outbox_attachment WHERE message_id = ?1", rusqlite::params![id])?;
    let n = tx.execute(
        "DELETE FROM message WHERE id = ?1 AND send_state IS NOT NULL",
        rusqlite::params![id],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

/// Bytes whose message row was never written (killed before the commit
/// point). Swept when the database opens, when no send can be mid-way.
pub(crate) fn sweep_orphan_attachments_on(conn: &rusqlite::Connection) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM outbox_attachment WHERE message_id NOT IN (SELECT id FROM message)",
        [],
    )?)
}

/// What [`enqueue`] found.
pub(super) enum Enqueued {
    /// Stored and unsent, now or by an earlier call. Deliver it.
    Unsent,
    AlreadySent(Message),
}

/// A fresh message id for the renderer's next send (#1335). Minted here so it
/// carries this crate's randomness: it is the envelope id the DS dedupes on.
pub fn new_message_id() -> String {
    Ulid::new().to_string()
}

/// A caller-supplied message id must be a ULID.
pub(super) fn checked_message_id(id: String) -> Result<String> {
    if Ulid::from_string(&id).is_err() {
        return Err(Error::Other(anyhow::anyhow!("invalid message id")));
    }
    Ok(id)
}

fn not_signed_in() -> Error {
    Error::Other(anyhow::anyhow!("Not signed in"))
}

/// The stored row behind an id, with its send state.
fn load(conn: &rusqlite::Connection, id: &str) -> Result<Option<(Message, Option<String>)>> {
    Ok(conn
        .query_row(
            "SELECT id, conversation_id, sender_id, content, reply_to_id, thread_id, sent_at, send_state
             FROM message WHERE id = ?1",
            rusqlite::params![id],
            |row| {
                Ok((
                    Message {
                        id: row.get(0)?,
                        conversation_id: row.get(1)?,
                        sender_id: row.get(2)?,
                        content: row.get(3)?,
                        reply_to_id: row.get(4)?,
                        thread_id: row.get(5)?,
                        sent_at: row.get(6)?,
                    },
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?)
}

/// Write the outbox row, before any network. A row with this id is adopted
/// rather than duplicated, but an id already used by another conversation or
/// author is refused.
pub(super) fn enqueue_on(
    conn: &rusqlite::Connection,
    queued: &Queued,
    attachments: &[AttMeta],
) -> Result<Enqueued> {
    if let Some((existing, send_state)) = load(conn, &queued.id)? {
        if existing.conversation_id != queued.conversation_id
            || existing.sender_id != queued.sender_id
        {
            return Err(Error::Other(anyhow::anyhow!("message id already in use")));
        }
        return Ok(match send_state {
            Some(_) => Enqueued::Unsent,
            None => Enqueued::AlreadySent(existing),
        });
    }
    // The row is the commit point: attachment bytes are stored before it.
    let content = if attachments.is_empty() {
        queued.content.clone()
    } else {
        provisional_content(&queued.content, attachments)
    };
    conn.execute(
        "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, reply_to_id, thread_id, sent_at, send_state)
         VALUES (?1, ?2, ?3, X'', ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            queued.id,
            queued.conversation_id,
            queued.sender_id,
            content,
            queued.reply_to_id,
            queued.thread_id,
            queued.sent_at,
            PENDING,
        ],
    )?;
    Ok(Enqueued::Unsent)
}

pub(super) async fn enqueue(
    state: &Arc<AppState>,
    queued: &Queued,
    attachments: Vec<OutgoingAttachment>,
) -> Result<Enqueued> {
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        if load(db.conn(), &queued.id)?.is_some() {
            return enqueue_on(db.conn(), queued, &[]);
        }
    }
    // Size first, store each attachment under its own lock, then the row,
    // which commits the message. A kill before the row leaves only bytes, swept
    // at the next open.
    let metas = plan_attachments(&attachments).await?;
    for (position, (att, meta)) in attachments.iter().zip(&metas).enumerate() {
        if let Err(e) = store_attachment(state, &queued.id, position, att, meta).await {
            let guard = state.local_db.lock().await;
            if let Some(db) = guard.as_ref() {
                let _ = db.conn().execute(
                    "DELETE FROM outbox_attachment WHERE message_id = ?1",
                    rusqlite::params![queued.id],
                );
            }
            return Err(e);
        }
    }
    let outcome = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        enqueue_on(db.conn(), queued, &metas)?
    };
    for id in attachments.iter().filter_map(|a| a.staged_id.as_deref()) {
        crate::commands::staging::discard_staged(id);
    }
    Ok(outcome)
}

/// Mark a message delivered; returns the `sent_at` it settled with.
pub(super) async fn settle(state: &Arc<AppState>, id: &str) -> Result<String> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_signed_in)?;
    db.conn().execute(
        "UPDATE message SET send_state = NULL WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(db.conn().query_row(
        "SELECT sent_at FROM message WHERE id = ?1",
        rusqlite::params![id],
        |row| row.get(0),
    )?)
}

/// Record a failed attempt. A delivered message is never demoted.
#[cfg(test)]
fn mark_failed_on(conn: &rusqlite::Connection, id: &str, permanent: bool) -> Result<()> {
    mark_state_on(conn, id, if permanent { REJECTED } else { FAILED })
}

fn mark_state_on(conn: &rusqlite::Connection, id: &str, send_state: &str) -> Result<()> {
    conn.execute(
        "UPDATE message SET send_state = ?1 WHERE id = ?2 AND send_state IS NOT NULL",
        rusqlite::params![send_state, id],
    )?;
    Ok(())
}

/// Every unsent id the drain should try, oldest first; `rejected` ones are not
/// among them.
pub(super) fn unsent_ids_on(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM message WHERE send_state IN ('pending', 'failed') ORDER BY sent_at, id",
    )?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids)
}

/// True when `id` names one of this device's unsent messages.
pub(super) async fn is_unsent(state: &Arc<AppState>, id: &str) -> Result<bool> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_signed_in)?;
    Ok(matches!(load(db.conn(), id)?, Some((_, Some(_)))))
}

/// Holds `id`'s outbox lock, and drops the map entry afterwards when nobody
/// else is waiting, so the map does not grow per message sent.
struct OutboxLock {
    state: Arc<AppState>,
    id: String,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl OutboxLock {
    async fn acquire(state: &Arc<AppState>, id: &str) -> Self {
        let entry = {
            let mut map = state.outbox_locks.lock().await;
            map.entry(id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = entry.lock_owned().await;
        Self {
            state: state.clone(),
            id: id.to_string(),
            guard: Some(guard),
        }
    }

    async fn release(mut self) {
        self.guard.take();
        let mut map = self.state.outbox_locks.lock().await;
        if let Some(entry) = map.get(&self.id) {
            // Only the map's own reference is left.
            if Arc::strong_count(entry) == 1 {
                map.remove(&self.id);
            }
        }
    }
}

/// Run one delivery attempt for `id`. Re-reads the row under the message's
/// lock, so a message delivered or discarded meanwhile is not sent again.
pub(super) async fn deliver(
    state: &Arc<AppState>,
    id: &str,
    sender_username: Option<String>,
) -> Result<Message> {
    let lock = OutboxLock::acquire(state, id).await;
    let result = deliver_locked(state, id, sender_username).await;
    lock.release().await;
    result
}

async fn deliver_locked(
    state: &Arc<AppState>,
    id: &str,
    sender_username: Option<String>,
) -> Result<Message> {
    let (queued, sender_username) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        let Some((row, send_state)) = load(db.conn(), id)? else {
            return Err(Error::Other(anyhow::anyhow!("Message not found")));
        };
        if send_state.is_none() {
            return Ok(row);
        }
        let Some(content) = row.content.clone() else {
            return Err(Error::Other(anyhow::anyhow!("Message not found")));
        };
        // A drained message has no renderer to name its author; use the
        // profile cache, for the mention ping's label.
        let sender_username = match sender_username {
            Some(name) => Some(name),
            None => db
                .conn()
                .query_row(
                    "SELECT username FROM user_cache WHERE id = ?1",
                    rusqlite::params![row.sender_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten(),
        };
        (
            Queued {
                id: row.id,
                conversation_id: row.conversation_id,
                sender_id: row.sender_id,
                content,
                reply_to_id: row.reply_to_id,
                thread_id: row.thread_id,
                sent_at: row.sent_at,
            },
            sender_username,
        )
    };

    #[cfg(feature = "test-harness")]
    super::send::test_hooks::count_attempt();

    let queued = match finish_uploads(state, queued).await {
        Ok(queued) => queued,
        Err(e) if crate::commands::file_keys::is_file_key_unavailable(&e) => {
            // Only the file key is missing; a key holder's re-wrap brings it.
            // Stays pending, not failed, for the next drain.
            mark_state(state, id, PENDING).await;
            return Err(e);
        }
        Err(e) => {
            mark_failed(state, id, is_permanent(&e)).await;
            return Err(e);
        }
    };

    match super::send::deliver_queued(&queued, sender_username, state).await {
        Ok(message) => Ok(message),
        Err(e) => {
            if !super::send::test_hooks::is_simulated_crash(&e) {
                mark_failed(state, id, is_permanent(&e)).await;
            }
            Err(e)
        }
    }
}

async fn mark_failed(state: &Arc<AppState>, id: &str, permanent: bool) {
    mark_state(state, id, if permanent { REJECTED } else { FAILED }).await;
}

async fn mark_state(state: &Arc<AppState>, id: &str, send_state: &str) {
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        if let Err(mark) = mark_state_on(db.conn(), id, send_state) {
            crate::diag!("[outbox] could not mark {id} {send_state}: {mark}");
        }
    }
}

/// Upload the stored attachment bytes one at a time, then swap the row's
/// provisional content for the finished `_att` envelope and drop the bytes in
/// one transaction. A kill leaves either the bytes (upload again; the DS
/// collects the orphaned object) or the finished content.
async fn finish_uploads(state: &Arc<AppState>, mut queued: Queued) -> Result<Queued> {
    let metas = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        attachment_metas_on(db.conn(), &queued.id)?
    };
    if metas.is_empty() {
        return Ok(queued);
    }
    let mut uploaded = Vec::with_capacity(metas.len());
    for (n, meta) in metas.iter().enumerate() {
        let data = {
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().ok_or_else(not_signed_in)?;
            attachment_data_on(db.conn(), &queued.id, n)?
        };
        uploaded.push(
            crate::commands::r2::upload_outbox_bytes(
                data,
                meta.filename.clone(),
                meta.content_type.clone(),
                &queued.conversation_id,
                state,
            )
            .await?,
        );
    }
    let content = final_content(&caption_of(&queued.content), &metas, &uploaded);
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        let tx = db.conn().unchecked_transaction()?;
        tx.execute(
            "UPDATE message SET content = ?1 WHERE id = ?2",
            rusqlite::params![content, queued.id],
        )?;
        tx.execute(
            "DELETE FROM outbox_attachment WHERE message_id = ?1",
            rusqlite::params![queued.id],
        )?;
        tx.commit()?;
    }
    queued.content = content;
    Ok(queued)
}

/// Deliver one unsent message again — the renderer's Retry (#1335).
pub async fn retry_message(message_id: String, state: &Arc<AppState>) -> Result<Message> {
    deliver(state, &message_id, None).await
}

/// Throw away one of this device's unsent messages, the renderer's Discard.
///
/// Waits out any attempt in flight. A row that was ever sealed may have
/// reached the DS, so it is first withdrawn from everyone like a self-delete.
/// A withdrawal that may pass later (offline) returns the error and removes
/// nothing; a permanent one (not a member) removes the row anyway.
///
/// Returns `false` when there was nothing to discard (unknown or delivered).
pub async fn discard_unsent_message(message_id: String, state: &Arc<AppState>) -> Result<bool> {
    let lock = OutboxLock::acquire(state, &message_id).await;
    let result = discard_locked(state, &message_id).await;
    lock.release().await;
    result
}

/// Whose message, where, and whether it was ever sealed (only a seal writes a
/// non-empty ciphertext).
fn unsent_row_on(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<(String, String, bool)>> {
    Ok(conn
        .query_row(
            "SELECT conversation_id, sender_id, length(ciphertext) > 0
             FROM message WHERE id = ?1 AND send_state IS NOT NULL",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
}

async fn discard_locked(state: &Arc<AppState>, id: &str) -> Result<bool> {
    let row = {
        let guard = state.local_db.lock().await;
        match guard.as_ref() {
            Some(db) => unsent_row_on(db.conn(), id)?,
            None => return Ok(false),
        }
    };
    let Some((conversation_id, sender_id, sealed)) = row else {
        return Ok(false);
    };
    if sealed {
        if let Err(e) = withdraw(state, &conversation_id, id, &sender_id).await {
            if !is_permanent(&e) {
                return Err(e);
            }
            crate::diag!("[outbox] discard of {id}: cannot withdraw it ({e}); removing it here");
        }
    }
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_signed_in)?;
    remove_unsent_on(db.conn(), id)
}

/// Withdraw a possibly-landed message, first checking membership: a
/// non-member's redaction would fail somewhere inside the MLS path instead.
async fn withdraw(
    state: &Arc<AppState>,
    conversation_id: &str,
    id: &str,
    sender_id: &str,
) -> Result<()> {
    let resolved = crate::commands::ds_reads::catch_up(state, conversation_id, false, false).await?;
    if !resolved.authorized {
        return Err(not_a_member());
    }
    super::edit_delete::retract_for_everyone(state, conversation_id, id, sender_id).await
}

/// What one drain did.
#[derive(Debug, Default, Clone, Serialize)]
pub struct OutboxReport {
    /// Ids now delivered.
    pub delivered: Vec<String>,
    /// Ids whose attempt failed; they are marked `'failed'` and stay queued.
    pub failed: Vec<String>,
}

/// Deliver every unsent message, oldest first (#1335).
///
/// Single-flight: a call during a drain sets `outbox_rerun` instead of
/// starting an overlapping one, and the running drain goes round once more (or
/// the waiting caller drains itself if it had finished). Each message goes
/// through [`deliver`]. No retry loop: a failed message waits for the next
/// drain or a Retry.
pub async fn flush_outbox(state: &Arc<AppState>) -> Result<OutboxReport> {
    use std::sync::atomic::Ordering;
    let mut last = match state.outbox_drain.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            state.outbox_rerun.store(true, Ordering::SeqCst);
            let guard = state.outbox_drain.lock().await;
            if !state.outbox_rerun.load(Ordering::SeqCst) {
                // The running drain took our request and went round again.
                return Ok(guard.clone().unwrap_or_default());
            }
            guard
        }
    };
    let mut report;
    loop {
        state.outbox_rerun.store(false, Ordering::SeqCst);
        report = drain(state).await?;
        if !state.outbox_rerun.load(Ordering::SeqCst) {
            break;
        }
    }
    *last = Some(report.clone());
    Ok(report)
}

async fn drain(state: &Arc<AppState>) -> Result<OutboxReport> {
    let ids = {
        let guard = state.local_db.lock().await;
        match guard.as_ref() {
            Some(db) => unsent_ids_on(db.conn())?,
            None => return Ok(OutboxReport::default()),
        }
    };
    let mut report = OutboxReport::default();
    for id in ids {
        match deliver(state, &id, None).await {
            Ok(_) => report.delivered.push(id),
            Err(e) => {
                crate::diag!("[outbox] delivery of {id} failed: {e}");
                report.failed.push(id);
            }
        }
    }
    Ok(report)
}

/// Drain the outbox in the background, run on `unlock`.
pub(crate) fn spawn_outbox_flush(state: &Arc<AppState>) {
    let state = state.clone();
    tokio::spawn(async move {
        match flush_outbox(&state).await {
            Ok(report) if report.delivered.is_empty() && report.failed.is_empty() => {}
            Ok(report) => crate::diag!(
                "[outbox] after unlock: {} delivered, {} still unsent",
                report.delivered.len(),
                report.failed.len()
            ),
            Err(e) => crate::diag!("[outbox] drain after unlock failed: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::local::LocalDb;

    fn queued(id: &str, content: &str) -> Queued {
        Queued {
            id: id.to_string(),
            conversation_id: "c1".to_string(),
            sender_id: "u1".to_string(),
            content: content.to_string(),
            reply_to_id: None,
            thread_id: Some("root".to_string()),
            sent_at: super::super::envelope_sent_at(),
        }
    }

    fn send_state(conn: &rusqlite::Connection, id: &str) -> Option<String> {
        conn.query_row(
            "SELECT send_state FROM message WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// The message is stored, as unsent, before anything else happens.
    #[test]
    fn enqueue_stores_the_message_as_pending_before_any_network() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let id = new_message_id();
        assert!(matches!(enqueue_on(conn, &queued(&id, "hello"), &[]).unwrap(), Enqueued::Unsent));
        assert_eq!(send_state(conn, &id).as_deref(), Some(PENDING));
        let (row, _) = load(conn, &id).unwrap().unwrap();
        assert_eq!(row.content.as_deref(), Some("hello"));
        // Thread replies ride the same row (#825).
        assert_eq!(row.thread_id.as_deref(), Some("root"));
        assert_eq!(unsent_ids_on(conn).unwrap(), vec![id]);
    }

    #[test]
    fn enqueue_is_idempotent_per_id() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let id = new_message_id();
        enqueue_on(conn, &queued(&id, "hello"), &[]).unwrap();
        assert!(matches!(enqueue_on(conn, &queued(&id, "hello"), &[]).unwrap(), Enqueued::Unsent));
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM message WHERE id = ?1", [&id], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        conn.execute("UPDATE message SET send_state = NULL WHERE id = ?1", [&id]).unwrap();
        match enqueue_on(conn, &queued(&id, "hello"), &[]).unwrap() {
            Enqueued::AlreadySent(m) => assert_eq!(m.id, id),
            Enqueued::Unsent => panic!("a delivered message must not be queued again"),
        }
        assert!(unsent_ids_on(conn).unwrap().is_empty());
    }

    /// An id another conversation already uses is refused, not merged.
    #[test]
    fn enqueue_refuses_an_id_used_elsewhere() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let id = new_message_id();
        enqueue_on(conn, &queued(&id, "hello"), &[]).unwrap();
        let mut other = queued(&id, "hello");
        other.conversation_id = "c2".to_string();
        assert!(enqueue_on(conn, &other, &[]).is_err());
    }

    #[test]
    fn mark_failed_never_demotes_a_delivered_message() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let (a, b) = (new_message_id(), new_message_id());
        enqueue_on(conn, &queued(&a, "a"), &[]).unwrap();
        enqueue_on(conn, &queued(&b, "b"), &[]).unwrap();
        conn.execute("UPDATE message SET send_state = NULL WHERE id = ?1", [&b]).unwrap();
        mark_failed_on(conn, &a, false).unwrap();
        mark_failed_on(conn, &b, false).unwrap();
        assert_eq!(send_state(conn, &a).as_deref(), Some(FAILED));
        assert_eq!(send_state(conn, &b), None);
        assert_eq!(unsent_ids_on(conn).unwrap(), vec![a]);
    }

    /// A drain delivers in the order the messages were written.
    #[test]
    fn unsent_ids_are_oldest_first() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let ids: Vec<String> = (0..3).map(|_| new_message_id()).collect();
        for (i, id) in ids.iter().enumerate() {
            conn.execute(
                "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, send_state)
                 VALUES (?1, 'c1', 'u1', X'', 'x', ?2, 'pending')",
                rusqlite::params![id, format!("2026-01-0{}T00:00:00.000Z", 3 - i)],
            )
            .unwrap();
        }
        let mut expected = ids.clone();
        expected.reverse();
        assert_eq!(unsent_ids_on(conn).unwrap(), expected);
    }

    fn meta(name: &str, size: usize) -> AttMeta {
        AttMeta {
            filename: name.to_string(),
            content_type: "image/png".to_string(),
            size,
            blurhash: Some("LKO2".to_string()),
            width: Some(4),
            height: Some(3),
        }
    }

    fn enqueue_with_files(conn: &rusqlite::Connection, q: &Queued, files: &[(&str, &[u8])]) {
        let metas: Vec<AttMeta> = files.iter().map(|(n, d)| meta(n, d.len())).collect();
        for (i, ((_, data), m)) in files.iter().zip(&metas).enumerate() {
            store_attachment_on(conn, &q.id, i, m, data).unwrap();
        }
        enqueue_on(conn, q, &metas).unwrap();
    }

    /// The bytes are stored before the row, and read back one at a time.
    #[test]
    fn an_attachment_send_stores_its_bytes_before_its_row() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let id = new_message_id();
        enqueue_with_files(conn, &queued(&id, "look"), &[("a.png", b"first"), ("b.png", b"second")]);

        let metas = attachment_metas_on(conn, &id).unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].size, 5);
        assert_eq!(metas[1].filename, "b.png");
        assert_eq!(attachment_data_on(conn, &id, 0).unwrap(), b"first");
        assert_eq!(attachment_data_on(conn, &id, 1).unwrap(), b"second");
        let (row, state) = load(conn, &id).unwrap().unwrap();
        assert_eq!(state.as_deref(), Some(PENDING));
        let content = row.content.unwrap();
        assert_eq!(
            content,
            r#"{"_att":[{"name":"a.png","ct":"image/png","size":5,"bh":"LKO2","w":4,"h":3},{"name":"b.png","ct":"image/png","size":6,"bh":"LKO2","w":4,"h":3}],"_txt":"look"}"#
        );
        assert_eq!(caption_of(&content), "look");
    }

    #[test]
    fn discarding_an_attachment_send_drops_its_bytes() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let id = new_message_id();
        enqueue_with_files(conn, &queued(&id, ""), &[("a.png", b"x")]);
        assert!(remove_unsent_on(conn, &id).unwrap());
        assert!(attachment_metas_on(conn, &id).unwrap().is_empty());
        assert!(load(conn, &id).unwrap().is_none());
    }

    /// Bytes from a send killed before its row are swept; queued ones are not.
    #[test]
    fn orphan_bytes_are_swept_and_queued_ones_kept() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let (queued_id, orphan) = (new_message_id(), new_message_id());
        enqueue_with_files(conn, &queued(&queued_id, ""), &[("a.png", b"x")]);
        store_attachment_on(conn, &orphan, 0, &meta("b.png", 1), b"y").unwrap();
        assert_eq!(sweep_orphan_attachments_on(conn).unwrap(), 1);
        assert_eq!(attachment_metas_on(conn, &queued_id).unwrap().len(), 1);
        assert!(attachment_metas_on(conn, &orphan).unwrap().is_empty());
    }

    /// The finished envelope matches the renderers' byte for byte.
    #[test]
    fn the_finished_envelope_matches_the_renderers() {
        let atts = [meta("clip.mp4", 1)];
        let uploaded = [crate::commands::r2::MediaUploadResult {
            key: "media/abc.enc".to_string(),
            url: "https://r2/media/abc.enc".to_string(),
            filename: "clip.mp4".to_string(),
            content_type: "video/mp4".to_string(),
            size_bytes: 1,
            content_hash: "abc".to_string(),
            key_version: 2,
            sha256: "d1g".to_string(),
            blurhash: None,
            width: Some(640),
            height: None,
        }];
        assert_eq!(
            final_content("", &atts, &uploaded),
            r#"{"_att":[{"key":"media/abc.enc","url":"https://r2/media/abc.enc","name":"clip.mp4","ct":"video/mp4","size":1,"hash":"abc","kv":2,"sha":"d1g","bh":"LKO2","w":640,"h":3}]}"#
        );
    }

    /// A remote delete of a row the outbox still holds applies and settles it,
    /// so the drain never re-posts a deleted message.
    #[test]
    fn a_remote_delete_of_an_unsent_row_applies_and_settles_it() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let id = new_message_id();
        enqueue_on(conn, &queued(&id, "landed, then the kill"), &[]).unwrap();
        let n = conn
            .execute(SOFT_DELETE_SQL, rusqlite::params!["2026-10-15T00:00:00Z", id])
            .expect("the soft delete must not trip the send_state CHECK");
        assert_eq!(n, 1);
        assert_eq!(send_state(conn, &id), None);
        assert!(unsent_ids_on(conn).unwrap().is_empty());
    }

    /// Discard withdraws from the DS only a row that was ever sealed.
    #[test]
    fn a_discard_knows_whether_the_message_may_have_left() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let (fresh, sealed) = (new_message_id(), new_message_id());
        enqueue_on(conn, &queued(&fresh, "a"), &[]).unwrap();
        enqueue_on(conn, &queued(&sealed, "b"), &[]).unwrap();
        conn.execute("UPDATE message SET ciphertext = X'01' WHERE id = ?1", [&sealed]).unwrap();
        assert!(!unsent_row_on(conn, &fresh).unwrap().unwrap().2);
        assert!(unsent_row_on(conn, &sealed).unwrap().unwrap().2);
        conn.execute("UPDATE message SET send_state = NULL WHERE id = ?1", [&sealed]).unwrap();
        assert!(unsent_row_on(conn, &sealed).unwrap().is_none());
    }

    /// A `rejected` row stays unsent but the drain leaves it alone.
    #[test]
    fn a_permanent_refusal_is_not_drained_again() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let (gone, flaky) = (new_message_id(), new_message_id());
        enqueue_on(conn, &queued(&gone, "a"), &[]).unwrap();
        enqueue_on(conn, &queued(&flaky, "b"), &[]).unwrap();
        mark_failed_on(conn, &gone, true).unwrap();
        mark_failed_on(conn, &flaky, false).unwrap();
        assert_eq!(send_state(conn, &gone).as_deref(), Some(REJECTED));
        assert_eq!(unsent_ids_on(conn).unwrap(), vec![flaky]);
        assert!(matches!(load(conn, &gone).unwrap(), Some((_, Some(_)))));
    }

    #[test]
    fn only_a_definite_ds_refusal_is_permanent() {
        use crate::commands::mls::ds_client::DsRefused;
        let refused = |code: u16| {
            Error::Other(anyhow::Error::new(DsRefused {
                label: "ds_post".to_string(),
                path: "/v1/messages/send".to_string(),
                status: reqwest::StatusCode::from_u16(code).unwrap(),
                body: String::new(),
            }))
        };
        for code in [400, 403, 404, 410, 422] {
            assert!(is_permanent(&refused(code)), "{code}");
        }
        for code in [401, 409, 429, 500, 502, 503] {
            assert!(!is_permanent(&refused(code)), "{code}");
        }
        assert!(is_permanent(&not_a_member()));
        assert!(!is_permanent(&Error::ClientOutdated));
        assert!(!is_permanent(&Error::Other(anyhow::anyhow!("connection reset"))));
        assert_eq!(refused(403).to_string(), "ds_post /v1/messages/send 403 Forbidden: ");
    }

    #[test]
    fn caller_supplied_ids_must_be_ulids() {
        assert!(checked_message_id(new_message_id()).is_ok());
        assert!(checked_message_id("pending-123".to_string()).is_err());
        assert!(checked_message_id(String::new()).is_err());
    }
}
