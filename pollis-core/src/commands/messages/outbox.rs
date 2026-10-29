//! The durable outbox for outgoing messages (#1335).
//!
//! Before #1335 a send wrote its local `message` row only after the slow part —
//! resolve, MLS catch-up, seal — and the DS post came after that. The first send
//! in a fresh group can take up to a minute, and the renderer shows the message
//! as sent from the first frame. A process killed in that window (the user
//! swiping the app away, the OS reclaiming it) lost the message without a trace:
//! no row, nothing marked unsent, nothing to retry.
//!
//! The outbox is the `message` row itself, written FIRST with
//! `send_state = 'pending'` and an empty ciphertext. Delivery fills the
//! ciphertext, posts, and clears the state; a failed attempt sets `'failed'`.
//! Using the row rather than a side table means every read path — the
//! conversation page, previews, threads, search — already shows the message, on
//! every restart, with no second copy to keep in step.
//!
//! An unsent row is delivered again by [`flush_outbox`] — spawned after every
//! unlock (which is also every cold start) and called by both renderers on a
//! realtime reconnect and (mobile) on returning to the foreground — and by
//! [`retry_message`] when the user taps Retry. All event-driven; nothing polls.
//! The same drain also delivers edits still in the edit outbox (#1555,
//! `edit_outbox`).
//!
//! **Idempotency.** A retry reuses the message id, which is the envelope id.
//! If an earlier attempt's envelope did land (the process died before it could
//! record that), the DS answers the re-post as a replay — 200, nothing stored
//! twice, no second push (`pollis_delivery::messages::send_envelope`) — and a
//! recipient that somehow holds the id already keeps its one row. Within this
//! process, one attempt per message runs at a time: [`deliver`] holds the
//! message's lock in `AppState::outbox_locks` for the whole attempt and re-reads
//! the row once it has it, so a drain arriving behind an in-flight send finds
//! the message already delivered and leaves it alone.

use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::Message;

/// `message.send_state` for a stored message not yet confirmed by the DS —
/// including an attachment send whose only obstacle is that its
/// conversation's file key has not reached this device yet
/// (`file_keys::FileKeyUnavailable`): that is a wait, not a failure.
pub(crate) const PENDING: &str = "pending";
/// `message.send_state` once a delivery attempt has failed in a way a later
/// one may not (offline, a 5xx, an outdated client).
pub(crate) const FAILED: &str = "failed";
/// `message.send_state` once the DS refused it in a way no retry fixes — not a
/// member any more, the conversation is gone, a malformed body. The drain
/// leaves these alone; the user's Retry or Discard decides.
pub(crate) const REJECTED: &str = "rejected";

/// Whether `e` is a refusal no retry can fix (see [`REJECTED`]): a DS answer of
/// 400, 403, 404, 410 or 422. Everything else — transport errors, 5xx, 429, an
/// outdated client, a local MLS problem — may pass next time.
pub(super) fn is_permanent(e: &Error) -> bool {
    let Error::Other(any) = e else {
        return false;
    };
    any.downcast_ref::<NotAMember>().is_some()
        || any
            .downcast_ref::<crate::commands::mls::ds_client::DsRefused>()
            .is_some_and(|r| matches!(r.status.as_u16(), 400 | 403 | 404 | 410 | 422))
}

/// The DS's conversation resolve answered that the sender is not (or no longer)
/// a member — the commonest refusal no retry fixes. Typed so
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

/// The one way a row is soft-deleted (a redaction, an admin tombstone, the
/// admin's own copy). It also settles `send_state`: a delete can only target
/// an envelope that exists, so a message of ours that is deleted for everyone
/// was delivered — and an unsent row must keep its content (the CHECK), so a
/// redaction that left the state set would fail, and the ingest would hold its
/// cursor on it forever (#1335). Binds `?1` = deleted_at, `?2` = id.
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
    /// The send's display stamp. While the row has never been sealed every
    /// attempt refreshes it, so a message queued offline lands at the time it
    /// actually went out rather than hours earlier; the first attempt that
    /// SEALS persists its stamp, and every later attempt posts that one
    /// unchanged — so a retry after a crash-after-post (which the DS answers as
    /// a replay of the first post) leaves the sender's copy where every
    /// recipient's is.
    pub sent_at: String,
}

/// One attachment a send hands to the outbox before it is uploaded (#1335).
///
/// Exactly one source: `path`, a file the user chose (a picker selection or a
/// drop — the shell has already checked it against its path scope), or
/// `staged_id`, bytes the renderer staged in memory (a paste). Either way the
/// bytes are stored in the local SQLCipher database before the message row is,
/// and before any network work, so a process killed during the upload loses
/// nothing; the staging entry is released once they are.
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

/// What the outbox knows about one stored attachment without its bytes: all a
/// provisional envelope, the finished one and an upload's bookkeeping need. The
/// bytes themselves are read one attachment at a time, when uploaded.
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
/// together. Each is already capped at the R2 limit; this bounds what a single
/// queued message can put into the local database.
pub(crate) const MAX_OUTBOX_BYTES_PER_MESSAGE: u64 = 4 * pollis_api::broker::R2_MEDIA_MAX_BYTES;

fn over_cap(size: u64) -> Option<Error> {
    (size > pollis_api::broker::R2_MEDIA_MAX_BYTES).then(|| {
        Error::Other(anyhow::anyhow!(
            "attachment is {size} bytes; the limit is {} bytes",
            pollis_api::broker::R2_MEDIA_MAX_BYTES
        ))
    })
}

/// Size every attachment from what is already known about it — the file's
/// metadata, the staged buffer's length — and refuse anything the upload would
/// refuse (over the R2 cap, or over the per-message total) BEFORE a byte of it
/// is read or allocated.
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

/// Store one attachment's bytes: read (a file) or borrowed (a staged paste —
/// no copy), inserted under the database lock for that one row only, so a
/// large message never holds the lock across all of its files.
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
/// same keys, same order, absent values omitted — so a message whose upload
/// the outbox finished is indistinguishable on the wire from one the renderer
/// uploaded itself.
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
    /// SHA-256 of the plaintext, checked after decrypt. Only ever inside the
    /// MLS-encrypted message.
    sha: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bh: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    w: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    h: Option<u32>,
}

/// An entry the outbox has not uploaded yet: what to draw while it waits. No
/// `key`, which is how both renderers tell an attachment still uploading.
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
    /// Omitted, not `""`, when there is no caption — as the renderers do.
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

/// The bytes of the `n`th stored attachment of `id` (by order) — one blob at a
/// time, so an upload never holds a whole message's files in memory.
fn attachment_data_on(conn: &rusqlite::Connection, id: &str, n: usize) -> Result<Vec<u8>> {
    Ok(conn.query_row(
        "SELECT data FROM outbox_attachment WHERE message_id = ?1
         ORDER BY position LIMIT 1 OFFSET ?2",
        rusqlite::params![id, n as i64],
        |row| row.get(0),
    )?)
}

/// Remove an unsent message together with any bytes it still holds.
pub(super) fn remove_unsent_on(conn: &rusqlite::Connection, id: &str) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM outbox_attachment WHERE message_id = ?1", rusqlite::params![id])?;
    let n = tx.execute(
        "DELETE FROM message WHERE id = ?1 AND send_state IS NOT NULL",
        rusqlite::params![id],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

/// Bytes whose message row was never written: a process killed after storing
/// an attachment but before the row (the commit point) — a send the user never
/// saw accepted. Swept when the database opens, when no send can be mid-way.
pub(crate) fn sweep_orphan_attachments_on(conn: &rusqlite::Connection) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM outbox_attachment WHERE message_id NOT IN (SELECT id FROM message)",
        [],
    )?)
}

/// What [`enqueue`] found.
pub(super) enum Enqueued {
    /// The row is stored and unsent — written now, or by an earlier call that
    /// never finished. Deliver it.
    Unsent,
    /// A message with this id was already delivered. Nothing to do.
    AlreadySent(Message),
}

/// A fresh message id for the renderer to name its next send with, so its
/// optimistic row and the stored outbox row are the same message (#1335).
/// Minted here rather than in JavaScript so it carries this crate's
/// randomness: the id is the envelope id the DS dedupes on.
pub fn new_message_id() -> String {
    Ulid::new().to_string()
}

/// A caller-supplied message id must be a ULID — the shape every id this crate
/// mints has, and the one the DS and every reader assume.
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
pub(super) fn load(conn: &rusqlite::Connection, id: &str) -> Result<Option<(Message, Option<String>)>> {
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

/// Write the outbox row — the first thing a send does, before any network.
///
/// A row that already carries this id is adopted rather than duplicated, so a
/// repeated call (a renderer retrying its own invoke) is harmless. It must be
/// the same message, though: an id already used by another conversation or
/// author is refused rather than silently merged.
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
    // The row is the commit point: an attachment message's bytes are all
    // stored before it (see `enqueue`), so a row never exists without them.
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
    // An id already stored is adopted as it stands — any attachments it has
    // were stored with it.
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        if load(db.conn(), &queued.id)?.is_some() {
            return enqueue_on(db.conn(), queued, &[]);
        }
    }
    // Sized before anything is read; then stored one at a time, each under the
    // database lock for its own insert only; then the row, which commits the
    // message. A kill before the row leaves only bytes, swept at the next open.
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
    // The bytes are stored, so the in-memory copy has done its job.
    for id in attachments.iter().filter_map(|a| a.staged_id.as_deref()) {
        crate::commands::staging::discard_staged(id);
    }
    Ok(outcome)
}

/// Mark a message delivered; returns the `sent_at` it settled with.
pub(super) async fn settle(state: &Arc<AppState>, id: &str) -> Result<String> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_signed_in)?;
    settle_on(db.conn(), id)
}

/// [`settle`] on a connection.
pub(super) fn settle_on(conn: &rusqlite::Connection, id: &str) -> Result<String> {
    conn.execute(
        "UPDATE message SET send_state = NULL WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(conn.query_row(
        "SELECT sent_at FROM message WHERE id = ?1",
        rusqlite::params![id],
        |row| row.get(0),
    )?)
}

/// Record a failed attempt — `rejected` when no retry can fix it, `failed`
/// otherwise. Only an unsent row can fail: a delivered message is never
/// demoted.
#[cfg(test)]
fn mark_failed_on(conn: &rusqlite::Connection, id: &str, permanent: bool) -> Result<()> {
    mark_state_on(conn, id, if permanent { REJECTED } else { FAILED })
}

/// Set an unsent row's state. A delivered row (`send_state IS NULL`) is never
/// touched.
pub(super) fn mark_state_on(conn: &rusqlite::Connection, id: &str, send_state: &str) -> Result<()> {
    conn.execute(
        "UPDATE message SET send_state = ?1 WHERE id = ?2 AND send_state IS NOT NULL",
        rusqlite::params![send_state, id],
    )?;
    Ok(())
}

/// The ids of every unsent message the drain should try, oldest first — the
/// order they were written, so a drain delivers a conversation's backlog in
/// the order it was typed. A `rejected` one is not among them.
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

/// Hold `id`'s outbox lock for the life of the guard, and drop the map entry
/// afterwards when nobody else is waiting on it, so the map does not grow by
/// one entry per message sent this session.
pub(super) struct OutboxLock {
    state: Arc<AppState>,
    id: String,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl OutboxLock {
    pub(super) async fn acquire(state: &Arc<AppState>, id: &str) -> Self {
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

    pub(super) async fn release(mut self) {
        self.guard.take();
        let mut map = self.state.outbox_locks.lock().await;
        if let Some(entry) = map.get(&self.id) {
            // The map's own reference is the only one left: no attempt holds or
            // awaits this lock.
            if Arc::strong_count(entry) == 1 {
                map.remove(&self.id);
            }
        }
    }
}

/// Run one delivery attempt for the queued message `id`.
///
/// Takes the message's lock, then re-reads the row: a message that another
/// attempt delivered while this one waited is returned as it stands, and one
/// the user discarded meanwhile is gone. A failed attempt marks the row
/// `'failed'` and returns the error.
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
        // A drained message has no renderer to name its author; the cache this
        // device keeps of its own profile does, for the mention ping's label.
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
            // Only the conversation's file key is missing: a key holder's
            // re-wrap brings it, nothing about the message is wrong. It stays
            // queued — not failed, no Retry — and the next drain sends it.
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

/// Upload whatever attachment bytes the outbox holds for this message — one
/// at a time, each blob read from the database just before its upload and
/// moved into it — then swap the row's provisional content for the finished
/// `_att` envelope and drop the bytes in one transaction, so a kill at any
/// point leaves either the bytes (upload again; the orphaned first object is
/// collected by the DS sweep once nothing references it)
/// or the finished content.
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

/// Deliver one unsent message again — the renderer's Retry (#1335). Naming a
/// delivered message whose edit is still queued delivers that edit (#1555).
pub async fn retry_message(message_id: String, state: &Arc<AppState>) -> Result<Message> {
    if !is_unsent(state, &message_id).await?
        && super::edit_outbox::has_pending_edit(state, &message_id).await?
    {
        super::edit_outbox::deliver_edit(state, &message_id).await?;
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        return load(db.conn(), &message_id)?
            .map(|(m, _)| m)
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Message not found")));
    }
    deliver(state, &message_id, None).await
}

/// Throw away one of this device's unsent messages — the renderer's Discard.
///
/// Waits out any attempt in flight, so it never races a post. A message that
/// was never sealed never left this device, and only the local row goes. One
/// that WAS sealed may have reached the DS — a process that died after its post
/// landed but before it recorded that, followed by an attempt that failed for
/// another reason, leaves exactly that — so it is first withdrawn from everyone
/// the way a self-delete is (redaction + envelope removal, a no-op when nothing
/// landed), and only then removed here. If that withdrawal fails for a reason
/// that may pass (offline), nothing is removed and the error is returned:
/// discarding must never leave a delivered message the sender could still have
/// deleted. If it fails for good — the sender is no longer a member, the
/// conversation is gone — there is nothing left they could ever withdraw, so
/// the row goes anyway rather than staying undiscardable forever.
///
/// Returns `false` when there was nothing to discard: the id is unknown, or the
/// message was delivered (a delivered message is deleted with `delete_message`).
pub async fn discard_unsent_message(message_id: String, state: &Arc<AppState>) -> Result<bool> {
    let lock = OutboxLock::acquire(state, &message_id).await;
    let result = discard_locked(state, &message_id).await;
    lock.release().await;
    result
}

/// The facts a discard decides on: whose message, where, and whether it was
/// ever sealed (a non-empty ciphertext is written only by a seal, right before
/// the post).
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

/// Withdraw a possibly-landed message from everyone, first asking whether the
/// sender can still act in that conversation at all — a non-member's redaction
/// would fail somewhere inside the MLS path instead of saying so.
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
    /// Messages whose stored edit is now confirmed (#1555), by message id.
    pub edits_delivered: Vec<String>,
    /// Messages whose stored edit failed again; it stays queued.
    pub edits_failed: Vec<String>,
}

/// Deliver every unsent message, oldest first (#1335).
///
/// Run after unlock and on reconnect / resume — triggers that often fire
/// together (an unlock, the launch catch-up, a reconnect and an `online`
/// event within seconds). So it is SINGLE-FLIGHT: a call that arrives while a
/// drain runs does not start a second, overlapping one (which would re-attempt
/// — a catch-up and a fresh seal each — every message the first is failing).
/// But it is not dropped either: it sets `outbox_rerun`, the running drain goes
/// round once more before it lets go, and if it had already finished its last
/// pass, the waiting caller drains itself. A reconnect that arrives while the
/// unlock drain is failing offline attempts therefore still gets its pass.
/// Each message still goes
/// through [`deliver`], so a drain that overlaps a send in flight waits for it
/// and then finds that message settled. Nothing here retries in a loop: a
/// message that fails stays `failed` until the next drain or a Retry, and a
/// `rejected` one is left to the user.
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
    // Edits after sends: an edit only ever targets a delivered message, so
    // nothing here waits on the loop above (#1555).
    let (edits_delivered, edits_failed) = super::edit_outbox::drain_edits(state).await?;
    report.edits_delivered = edits_delivered;
    report.edits_failed = edits_failed;
    Ok(report)
}

/// Drain the outbox in the background — what `unlock` runs, so a message a
/// killed process left behind goes out as soon as the device can sign again.
pub(crate) fn spawn_outbox_flush(state: &Arc<AppState>) {
    let state = state.clone();
    tokio::spawn(async move {
        match flush_outbox(&state).await {
            Ok(report)
                if report.delivered.is_empty()
                    && report.failed.is_empty()
                    && report.edits_delivered.is_empty()
                    && report.edits_failed.is_empty() => {}
            Ok(report) => crate::diag!(
                "[outbox] after unlock: {} delivered, {} still unsent; {} edits delivered, {} still unsent",
                report.delivered.len(),
                report.failed.len(),
                report.edits_delivered.len(),
                report.edits_failed.len()
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

    /// The whole fix in one assertion: the message is stored, as unsent, before
    /// anything else happens — so a process that dies next still has it.
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

    /// Enqueueing the same id twice never makes two messages, and a delivered
    /// message is handed back rather than queued again.
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

    /// Failing only ever applies to an unsent row; a delivered one stays
    /// delivered.
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
        // A failed message is still queued — the drain retries it.
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

    /// Store `files` the way `enqueue` does — the bytes first, then the row.
    fn enqueue_with_files(conn: &rusqlite::Connection, q: &Queued, files: &[(&str, &[u8])]) {
        let metas: Vec<AttMeta> = files.iter().map(|(n, d)| meta(n, d.len())).collect();
        for (i, ((_, data), m)) in files.iter().zip(&metas).enumerate() {
            store_attachment_on(conn, &q.id, i, m, data).unwrap();
        }
        enqueue_on(conn, q, &metas).unwrap();
    }

    /// The bytes are stored before the row, before any network — so a kill
    /// during the upload still has everything the upload needs — and are read
    /// back one attachment at a time.
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
        // While it waits the row draws the files, with no key to fetch.
        let (row, state) = load(conn, &id).unwrap().unwrap();
        assert_eq!(state.as_deref(), Some(PENDING));
        let content = row.content.unwrap();
        assert_eq!(
            content,
            r#"{"_att":[{"name":"a.png","ct":"image/png","size":5,"bh":"LKO2","w":4,"h":3},{"name":"b.png","ct":"image/png","size":6,"bh":"LKO2","w":4,"h":3}],"_txt":"look"}"#
        );
        assert_eq!(caption_of(&content), "look");
    }

    /// Discarding the message takes its bytes with it.
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

    /// Bytes stored for a send that was killed before its row (the commit
    /// point) are swept; a queued message's bytes are not.
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

    /// The finished envelope is the renderers' byte for byte: same key order,
    /// no `_txt` without a caption, the upload's metadata over the renderer's.
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

    /// A redaction or tombstone that reaches a row the outbox still holds
    /// applies (no CHECK failure that would hold the ingest's cursor) and
    /// settles it, so the drain never re-posts a deleted message.
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

    /// Discard withdraws from the DS only what may have reached it: a row the
    /// seal never touched is local-only; a sealed one may have landed.
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
        // A delivered message is not the discard's to touch.
        conn.execute("UPDATE message SET send_state = NULL WHERE id = ?1", [&sealed]).unwrap();
        assert!(unsent_row_on(conn, &sealed).unwrap().is_none());
    }

    /// A refusal no retry fixes is `rejected`, and the drain leaves it alone;
    /// it stays unsent (Retry / Discard), never dropped.
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

    /// Which errors are permanent: DS refusals of the request itself, never a
    /// transient failure.
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
        // The text is what it always was.
        assert_eq!(refused(403).to_string(), "ds_post /v1/messages/send 403 Forbidden: ");
    }

    #[test]
    fn caller_supplied_ids_must_be_ulids() {
        assert!(checked_message_id(new_message_id()).is_ok());
        assert!(checked_message_id("pending-123".to_string()).is_err());
        assert!(checked_message_id(String::new()).is_err());
    }
}
