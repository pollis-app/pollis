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

/// `message.send_state` for a stored message not yet confirmed by the DS.
pub(crate) const PENDING: &str = "pending";
/// `message.send_state` once a delivery attempt has failed.
pub(crate) const FAILED: &str = "failed";

/// An outgoing message as the outbox stores it.
#[derive(Debug, Clone)]
pub(super) struct Queued {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub content: String,
    pub reply_to_id: Option<String>,
    pub thread_id: Option<String>,
}

/// One attachment a send hands to the outbox before it is uploaded (#1335).
///
/// Exactly one source: `path`, a file the user chose (a picker selection or a
/// drop — the shell has already checked it against its path scope), or
/// `staged_id`, bytes the renderer staged in memory (a paste). Either way the
/// bytes are read and stored in the local SQLCipher database with the message
/// row, before any network work, so a process killed during the upload loses
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

/// An [`OutgoingAttachment`] with its bytes in hand, as `outbox_attachment`
/// stores it.
#[derive(Debug, Clone)]
pub(super) struct HeldAttachment {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
    pub blurhash: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Read every attachment's bytes, refusing anything the upload would refuse
/// anyway (over the R2 cap) before a row is written for it.
async fn hold_attachments(
    attachments: &[OutgoingAttachment],
) -> Result<Vec<HeldAttachment>> {
    let mut held = Vec::with_capacity(attachments.len());
    for att in attachments {
        let data = match (&att.path, &att.staged_id) {
            (Some(path), None) => tokio::fs::read(path).await.map_err(|e| {
                Error::Other(anyhow::anyhow!("read attachment {}: {e}", att.filename))
            })?,
            (None, Some(id)) => crate::commands::staging::peek_staged(id)
                .map(|bytes| bytes.to_vec())
                .ok_or_else(|| {
                    Error::Other(anyhow::anyhow!(
                        "no staged attachment {id}: it was already sent, discarded, or the \
                         session ended"
                    ))
                })?,
            _ => {
                return Err(Error::Other(anyhow::anyhow!(
                    "an attachment needs exactly one of path or stagedId"
                )))
            }
        };
        if data.len() as u64 > pollis_api::broker::R2_MEDIA_MAX_BYTES {
            return Err(Error::Other(anyhow::anyhow!(
                "attachment is {} bytes; the limit is {} bytes",
                data.len(),
                pollis_api::broker::R2_MEDIA_MAX_BYTES
            )));
        }
        held.push(HeldAttachment {
            filename: att.filename.clone(),
            content_type: att.content_type.clone(),
            data,
            blurhash: att.blurhash.clone(),
            width: att.width,
            height: att.height,
        });
    }
    Ok(held)
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
    hash: &'a str,
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
fn provisional_content(text: &str, held: &[HeldAttachment]) -> String {
    let env = Envelope {
        att: held
            .iter()
            .map(|a| PendingAttWire {
                name: &a.filename,
                ct: &a.content_type,
                size: a.data.len(),
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
    held: &[HeldAttachment],
    uploaded: &[crate::commands::r2::MediaUploadResult],
) -> String {
    let env = Envelope {
        att: held
            .iter()
            .zip(uploaded)
            .map(|(a, r)| AttWire {
                key: &r.key,
                url: &r.url,
                name: &r.filename,
                ct: &r.content_type,
                size: r.size_bytes,
                hash: &r.content_hash,
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

fn held_attachments_on(conn: &rusqlite::Connection, id: &str) -> Result<Vec<HeldAttachment>> {
    let mut stmt = conn.prepare(
        "SELECT filename, content_type, data, blurhash, width, height
         FROM outbox_attachment WHERE message_id = ?1 ORDER BY position",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![id], |row| {
            Ok(HeldAttachment {
                filename: row.get(0)?,
                content_type: row.get(1)?,
                data: row.get(2)?,
                blurhash: row.get(3)?,
                width: row.get(4)?,
                height: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Sign-out (#1335): an unsent message still holding attachment bytes cannot
/// be delivered without them, and the bytes are the user's files — they go
/// with the session, like staged pastes. Its row goes too (CASCADE takes the
/// bytes), so nothing is left behind half-sendable. Unsent TEXT stays queued
/// and goes out after the next unlock. Returns how many messages were dropped.
pub(crate) fn drop_unsent_attachment_messages_on(conn: &rusqlite::Connection) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM message WHERE send_state IS NOT NULL
           AND id IN (SELECT message_id FROM outbox_attachment)",
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

/// Write the outbox row — the first thing a send does, before any network.
///
/// A row that already carries this id is adopted rather than duplicated, so a
/// repeated call (a renderer retrying its own invoke) is harmless. It must be
/// the same message, though: an id already used by another conversation or
/// author is refused rather than silently merged.
pub(super) fn enqueue_on(
    conn: &rusqlite::Connection,
    queued: &Queued,
    attachments: &[HeldAttachment],
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
    // The row and its attachment bytes land together or not at all: a row
    // whose bytes were lost would be an attachment message that can never be
    // sent.
    let content = if attachments.is_empty() {
        queued.content.clone()
    } else {
        provisional_content(&queued.content, attachments)
    };
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, reply_to_id, thread_id, sent_at, send_state)
         VALUES (?1, ?2, ?3, X'', ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            queued.id,
            queued.conversation_id,
            queued.sender_id,
            content,
            queued.reply_to_id,
            queued.thread_id,
            super::envelope_sent_at(),
            PENDING,
        ],
    )?;
    for (position, att) in attachments.iter().enumerate() {
        tx.execute(
            "INSERT INTO outbox_attachment
               (message_id, position, filename, content_type, data, blurhash, width, height)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                queued.id,
                position as i64,
                att.filename,
                att.content_type,
                att.data,
                att.blurhash,
                att.width,
                att.height,
            ],
        )?;
    }
    tx.commit()?;
    Ok(Enqueued::Unsent)
}

pub(super) async fn enqueue(
    state: &Arc<AppState>,
    queued: &Queued,
    attachments: Vec<OutgoingAttachment>,
) -> Result<Enqueued> {
    // Read the bytes before taking the database lock: a 100 MB file read
    // should not hold up every other query.
    let held = hold_attachments(&attachments).await?;
    let outcome = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        enqueue_on(db.conn(), queued, &held)?
    };
    // The bytes are stored (or this id was already queued with its own), so
    // the in-memory copy has done its job.
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

/// Record a failed attempt. Only an unsent row can fail — a delivered message
/// is never demoted.
fn mark_failed_on(conn: &rusqlite::Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE message SET send_state = ?1 WHERE id = ?2 AND send_state IS NOT NULL",
        rusqlite::params![FAILED, id],
    )?;
    Ok(())
}

/// The ids of every unsent message, oldest first — the order they were
/// written, so a drain delivers a conversation's backlog in the order it was
/// typed.
pub(super) fn unsent_ids_on(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM message WHERE send_state IS NOT NULL ORDER BY sent_at, id",
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
            },
            sender_username,
        )
    };

    let queued = match finish_uploads(state, queued).await {
        Ok(queued) => queued,
        Err(e) => {
            mark_failed(state, id).await;
            return Err(e);
        }
    };

    match super::send::deliver_queued(&queued, sender_username, state).await {
        Ok(message) => Ok(message),
        Err(e) => {
            if !super::send::test_hooks::is_simulated_crash(&e) {
                mark_failed(state, id).await;
            }
            Err(e)
        }
    }
}

async fn mark_failed(state: &Arc<AppState>, id: &str) {
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        if let Err(mark) = mark_failed_on(db.conn(), id) {
            crate::diag!("[outbox] could not mark {id} failed: {mark}");
        }
    }
}

/// Upload whatever attachment bytes the outbox holds for this message, then
/// swap the row's provisional content for the finished `_att` envelope and drop
/// the bytes — one transaction, so a kill at any point leaves either the bytes
/// (upload again; the convergent upload dedupes) or the finished content.
async fn finish_uploads(state: &Arc<AppState>, mut queued: Queued) -> Result<Queued> {
    let held = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        held_attachments_on(db.conn(), &queued.id)?
    };
    if held.is_empty() {
        return Ok(queued);
    }
    let mut uploaded = Vec::with_capacity(held.len());
    for att in &held {
        uploaded.push(
            crate::commands::r2::upload_outbox_bytes(
                att.data.clone(),
                att.filename.clone(),
                att.content_type.clone(),
                state,
            )
            .await?,
        );
    }
    let content = final_content(&caption_of(&queued.content), &held, &uploaded);
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

/// Throw away one of this device's unsent messages — the renderer's Discard.
///
/// Waits out any attempt in flight, so it never races a post. Returns `false`
/// when there was nothing to discard: the id is unknown, or the message was
/// delivered (a delivered message is deleted with `delete_message`, which also
/// removes it for everyone else).
pub async fn discard_unsent_message(message_id: String, state: &Arc<AppState>) -> Result<bool> {
    let lock = OutboxLock::acquire(state, &message_id).await;
    let removed = {
        let guard = state.local_db.lock().await;
        match guard.as_ref() {
            Some(db) => db.conn().execute(
                "DELETE FROM message WHERE id = ?1 AND send_state IS NOT NULL",
                rusqlite::params![message_id],
            ),
            None => Ok(0),
        }
    };
    lock.release().await;
    Ok(removed? > 0)
}

/// What one drain did.
#[derive(Debug, Default, Serialize)]
pub struct OutboxReport {
    /// Ids now delivered.
    pub delivered: Vec<String>,
    /// Ids whose attempt failed; they are marked `'failed'` and stay queued.
    pub failed: Vec<String>,
}

/// Deliver every unsent message, oldest first (#1335).
///
/// Run after unlock and on reconnect / resume. Each message goes through
/// [`deliver`], so a drain that overlaps a send in flight — or another drain —
/// waits for it and then finds that message settled. Nothing here retries in a
/// loop: a message that fails stays `'failed'` until the next drain or a Retry.
pub async fn flush_outbox(state: &Arc<AppState>) -> Result<OutboxReport> {
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

/// Drain the outbox in the background — what `unlock` runs, so a message a
/// killed process left behind goes out as soon as the device can sign again.
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
        mark_failed_on(conn, &a).unwrap();
        mark_failed_on(conn, &b).unwrap();
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

    fn held(name: &str, bytes: &[u8]) -> HeldAttachment {
        HeldAttachment {
            filename: name.to_string(),
            content_type: "image/png".to_string(),
            data: bytes.to_vec(),
            blurhash: Some("LKO2".to_string()),
            width: Some(4),
            height: Some(3),
        }
    }

    /// The bytes are stored with the row, in one go, before any network — so
    /// a kill during the upload still has everything the upload needs.
    #[test]
    fn an_attachment_send_stores_its_bytes_with_the_row() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let id = new_message_id();
        let atts = [held("a.png", b"first"), held("b.png", b"second")];
        enqueue_on(conn, &queued(&id, "look"), &atts).unwrap();

        let back = held_attachments_on(conn, &id).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].data, b"first");
        assert_eq!(back[1].filename, "b.png");
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
        enqueue_on(conn, &queued(&id, ""), &[held("a.png", b"x")]).unwrap();
        conn.execute("DELETE FROM message WHERE id = ?1", [&id]).unwrap();
        assert!(held_attachments_on(conn, &id).unwrap().is_empty());
    }

    /// Sign-out drops an attachment send it cannot finish, and keeps text.
    #[test]
    fn sign_out_drops_only_unsent_attachment_messages() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let (text, file) = (new_message_id(), new_message_id());
        enqueue_on(conn, &queued(&text, "words"), &[]).unwrap();
        enqueue_on(conn, &queued(&file, ""), &[held("a.png", b"x")]).unwrap();
        assert_eq!(drop_unsent_attachment_messages_on(conn).unwrap(), 1);
        assert_eq!(unsent_ids_on(conn).unwrap(), vec![text]);
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM outbox_attachment", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    /// The finished envelope is the renderers' byte for byte: same key order,
    /// no `_txt` without a caption, the upload's metadata over the renderer's.
    #[test]
    fn the_finished_envelope_matches_the_renderers() {
        let atts = [held("clip.mp4", b"v")];
        let uploaded = [crate::commands::r2::MediaUploadResult {
            key: "media/abc.enc".to_string(),
            url: "https://r2/media/abc.enc".to_string(),
            filename: "clip.mp4".to_string(),
            content_type: "video/mp4".to_string(),
            size_bytes: 1,
            content_hash: "abc".to_string(),
            blurhash: None,
            width: Some(640),
            height: None,
        }];
        assert_eq!(
            final_content("", &atts, &uploaded),
            r#"{"_att":[{"key":"media/abc.enc","url":"https://r2/media/abc.enc","name":"clip.mp4","ct":"video/mp4","size":1,"hash":"abc","bh":"LKO2","w":640,"h":3}]}"#
        );
    }

    #[test]
    fn caller_supplied_ids_must_be_ulids() {
        assert!(checked_message_id(new_message_id()).is_ok());
        assert!(checked_message_id("pending-123".to_string()).is_err());
        assert!(checked_message_id(String::new()).is_err());
    }
}
