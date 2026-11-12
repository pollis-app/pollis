//! The durable outbox for outgoing messages (#1335).
//!
//! The outbox is the `message` row itself, written first as `pending`, so every read path shows it.
//!
//! [`flush_outbox`] (event-driven, never polled) and [`retry_message`] redeliver unsent rows and queued edits.
//!
//! **Idempotency.** A retry reuses the envelope id (the DS answers a replay); [`deliver`] holds the per-message lock.
//!
//! **Order (#1636).** One conversation delivers one message at a time, in stored `(sent_at, id)`
//! order: [`deliver`] holds the conversation's line lock and sends every earlier `pending` row
//! first. A `failed` or `rejected` row never holds up the ones behind it.

use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::Message;

/// Not yet confirmed by the DS, including an attachment send waiting on its file key (a wait, not a failure).
pub(crate) const PENDING: &str = "pending";
/// `message.send_state` once a delivery attempt has failed in a way a later
/// one may not (offline, a 5xx, an outdated client).
pub(crate) const FAILED: &str = "failed";
/// The DS refused it in a way no retry fixes; the drain leaves it to the user.
pub(crate) const REJECTED: &str = "rejected";

/// Whether `e` is a refusal no retry fixes: DS 400, 403, 404, 410 or 422.
pub(super) fn is_permanent(e: &Error) -> bool {
    let Error::Other(any) = e else {
        return false;
    };
    any.downcast_ref::<NotAMember>().is_some()
        || any
            .downcast_ref::<crate::commands::mls::ds_client::DsRefused>()
            .is_some_and(|r| matches!(r.status.as_u16(), 400 | 403 | 404 | 410 | 422))
}

/// The DS resolve says the sender is not a member; typed so [`is_permanent`] needn't parse.
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

/// The one soft-delete; also clears `send_state`, or the CHECK would wedge ingest (#1335). `?1` = deleted_at, `?2` = id.
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
    /// Refreshed each attempt until the first seal, then fixed so a replayed post matches recipients' copies.
    pub sent_at: String,
}

/// One attachment a send hands to the outbox before it is uploaded (#1335).
/// Exactly one source: a `path` or a `staged_id`; bytes are stored locally before any network work.
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
    /// Marked sensitive (#1568); travels only inside the encrypted envelope (`sens`).
    #[serde(default)]
    pub sensitive: bool,
    /// A video's poster frame from the platform decoder, base64 (#1837). The
    /// outbox bounds it into the envelope's `pv` and takes its blurhash and size.
    #[serde(default)]
    pub poster: Option<String>,
}

/// What the outbox knows about one stored attachment, without its bytes.
#[derive(Debug, Clone)]
pub(super) struct AttMeta {
    pub filename: String,
    pub content_type: String,
    pub size: usize,
    pub blurhash: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sensitive: bool,
    /// The bounded poster JPEG, base64 (#1837); `pv` on the wire.
    pub poster: Option<String>,
}

/// The most one queued message may carry through the outbox, all attachments together.
pub(crate) const MAX_OUTBOX_BYTES_PER_MESSAGE: u64 = 4 * pollis_api::broker::R2_MEDIA_MAX_BYTES;

fn over_cap(size: u64) -> Option<Error> {
    (size > pollis_api::broker::R2_MEDIA_MAX_BYTES).then(|| {
        Error::Other(anyhow::anyhow!(
            "attachment is {size} bytes; the limit is {} bytes",
            pollis_api::broker::R2_MEDIA_MAX_BYTES
        ))
    })
}

/// Size every attachment and refuse anything over the caps before a byte is read.
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
        let mut meta = AttMeta {
            filename: att.filename.clone(),
            content_type: att.content_type.clone(),
            size: size as usize,
            blurhash: att.blurhash.clone(),
            width: att.width,
            height: att.height,
            sensitive: att.sensitive,
            poster: None,
        };
        // The frame's blurhash and size win over the renderer's: they are the video's own.
        if let Some(p) =
            crate::commands::r2::video_poster(&att.content_type, att.poster.clone()).await
        {
            meta.blurhash = Some(p.blurhash);
            meta.width = Some(p.width);
            meta.height = Some(p.height);
            meta.poster = p.jpeg.map(crate::commands::poster::PosterJpeg::into_string);
        }
        metas.push(meta);
    }
    crate::commands::poster::within_budget(metas.iter_mut().map(|m| &mut m.poster));
    Ok(metas)
}

fn no_staged(id: &str) -> Error {
    Error::Other(anyhow::anyhow!(
        "no staged attachment {id}: it was already sent, discarded, or the session ended"
    ))
}

const INSERT_ATTACHMENT_SQL: &str = "INSERT OR REPLACE INTO outbox_attachment
       (message_id, position, filename, content_type, data, blurhash, width, height, sensitive, poster)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

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
            meta.sensitive,
            meta.poster,
        ],
    )?;
    Ok(())
}

/// Store one attachment's bytes, locking the DB for that one row only.
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
/// One `_att` entry exactly as the renderers encode it (same keys, same order).
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
    /// Sensitive media (#1568); omitted unless set, so unmarked entries are unchanged.
    #[serde(skip_serializing_if = "is_false")]
    sens: bool,
    /// A video's poster JPEG, base64 (#1837); omitted when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pv: Option<&'a str>,
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
    /// Sensitive media (#1568); omitted unless set, so unmarked entries are unchanged.
    #[serde(skip_serializing_if = "is_false")]
    sens: bool,
    /// A video's poster JPEG, base64 (#1837); omitted when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pv: Option<&'a str>,
}

#[derive(Serialize)]
struct Envelope<'a, A> {
    #[serde(rename = "_att")]
    att: Vec<A>,
    /// Omitted, not `""`, when there is no caption — as the renderers do.
    #[serde(rename = "_txt", skip_serializing_if = "Option::is_none")]
    txt: Option<&'a str>,
}

fn is_false(b: &bool) -> bool {
    !*b
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
                sens: a.sensitive,
                pv: a.poster.as_deref(),
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
                sens: a.sensitive,
                pv: a.poster.as_deref(),
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
        "SELECT filename, content_type, length(data), blurhash, width, height, sensitive, poster
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
                sensitive: row.get(6)?,
                poster: row.get(7)?,
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

/// Bytes whose message row was never written (killed mid-enqueue); swept at DB open.
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

/// A fresh message id for the renderer, so its optimistic row is the stored row (#1335).
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
/// An existing row with this id is adopted, but only if it is the same message.
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
    // An id already stored is adopted as it stands.
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        if load(db.conn(), &queued.id)?.is_some() {
            return enqueue_on(db.conn(), queued, &[]);
        }
    }
    // Sized, then stored one at a time, then the row (the commit point); a kill leaves only bytes.
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
    // Test-harness only: hold an attachment send between its bytes and its row (#1636).
    #[cfg(feature = "test-harness")]
    if !attachments.is_empty() {
        crate::commands::mls::rendezvous::park(crate::commands::mls::rendezvous::Point::OutboxBeforeRow)
            .await;
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

/// Record a failed attempt: `rejected` if permanent, else `failed`.
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

/// The app is being suspended with these sends still out: mark the ones still `pending`
/// `failed`, so the row reads "Not sent" until a drain (resume, relaunch, reconnect) delivers
/// it. A delivered or already-failed row is left alone. Returns how many were marked.
pub(crate) fn mark_suspended_sends_on(conn: &rusqlite::Connection, ids: &[String]) -> Result<usize> {
    let mut marked = 0;
    for id in ids {
        marked += conn.execute(
            "UPDATE message SET send_state = ?1 WHERE id = ?2 AND send_state = ?3",
            rusqlite::params![FAILED, id, PENDING],
        )?;
    }
    Ok(marked)
}

/// Unsent message ids the drain should try, oldest first; `rejected` ones excluded.
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

/// Holds `id`'s outbox lock; drops the map entry when nobody else waits.
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

/// Sends of each conversation still writing their outbox row (#1636).
///
/// A send registers here before its first await and leaves once its row is written (or it
/// died trying), so a delivery never runs past an earlier send whose row is not there yet:
/// an attachment send stores its bytes before its row, and a text sent just after must not
/// overtake it.
#[derive(Default)]
pub struct Enqueuing {
    inner: std::sync::Mutex<std::collections::HashMap<String, Vec<EnqueueEntry>>>,
}

struct EnqueueEntry {
    sent_at: String,
    id: String,
    gate: Arc<tokio::sync::Mutex<()>>,
}

/// One send's place in [`Enqueuing`]; dropping it (the row is written, or the future was
/// dropped) lets deliveries behind it go.
pub(super) struct EnqueueTicket {
    state: Arc<AppState>,
    conversation_id: String,
    id: String,
    _open: tokio::sync::OwnedMutexGuard<()>,
}

impl EnqueueTicket {
    /// Synchronous on purpose: taken before the send's first await.
    pub(super) fn begin(state: &Arc<AppState>, queued: &Queued) -> Self {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let open = gate
            .clone()
            .try_lock_owned()
            .expect("a gate nobody has seen is free");
        state
            .outbox_enqueuing
            .inner
            .lock()
            .expect("outbox_enqueuing poisoned")
            .entry(queued.conversation_id.clone())
            .or_default()
            .push(EnqueueEntry {
                sent_at: queued.sent_at.clone(),
                id: queued.id.clone(),
                gate,
            });
        Self {
            state: state.clone(),
            conversation_id: queued.conversation_id.clone(),
            id: queued.id.clone(),
            _open: open,
        }
    }
}

impl Drop for EnqueueTicket {
    fn drop(&mut self) {
        let mut map = self
            .state
            .outbox_enqueuing
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(entries) = map.get_mut(&self.conversation_id) {
            entries.retain(|e| e.id != self.id);
            if entries.is_empty() {
                map.remove(&self.conversation_id);
            }
        }
    }
}

impl Enqueuing {
    /// Sends still writing their row, across conversations. For tests.
    #[cfg(feature = "test-harness")]
    pub fn in_progress(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .map(Vec::len)
            .sum()
    }
}

/// Gates of this conversation's sends still writing a row that comes before `(sent_at, id)`.
fn earlier_enqueues(
    state: &Arc<AppState>,
    conversation_id: &str,
    sent_at: &str,
    id: &str,
) -> Vec<Arc<tokio::sync::Mutex<()>>> {
    let map = state
        .outbox_enqueuing
        .inner
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    map.get(conversation_id)
        .map(|entries| {
            entries
                .iter()
                .filter(|e| e.id != id && (e.sent_at.as_str(), e.id.as_str()) < (sent_at, id))
                .map(|e| e.gate.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// The outbox-lock key that serializes one conversation's deliveries (#1636). Message ids are
/// ULIDs, so this can never name a message's own lock.
fn line_key(conversation_id: &str) -> String {
    format!("line:{conversation_id}")
}

/// Where a stored row sits in its conversation's line: `(conversation_id, sent_at)`.
fn line_position_on(conn: &rusqlite::Connection, id: &str) -> Result<Option<(String, String)>> {
    Ok(conn
        .query_row(
            "SELECT conversation_id, sent_at FROM message WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// Earlier `pending` rows of a conversation; reads only the unsent rows, through
/// `idx_message_unsent_line` (`tests::the_line_queries_read_only_unsent_rows`).
const AHEAD_SQL: &str = "SELECT id FROM message
     WHERE conversation_id = ?1 AND send_state = 'pending' AND id != ?3
       AND (sent_at < ?2 OR (sent_at = ?2 AND id < ?3))
     ORDER BY sent_at, id";

/// Later never-sealed unsent rows of a conversation, through the same index.
const BEHIND_SQL: &str = "SELECT id, sent_at FROM message
     WHERE conversation_id = ?1 AND send_state IS NOT NULL AND length(ciphertext) = 0
       AND id != ?3 AND (sent_at > ?2 OR (sent_at = ?2 AND id > ?3))
     ORDER BY sent_at, id";

/// This conversation's `pending` messages stored before `id`, oldest first (#1636).
///
/// Only `pending`: a `failed` or `rejected` message does not hold up the ones behind it. It
/// is retried by Retry and (`failed` only) the drain, and then lands when it goes.
pub(super) fn pending_ahead_on(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    id: &str,
) -> Result<Vec<String>> {
    let Some((_, sent_at)) = line_position_on(conn, id)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(AHEAD_SQL)?;
    let ids = stmt
        .query_map(rusqlite::params![conversation_id, sent_at, id], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids)
}

/// The stamp a never-sealed row seals with (#1636), and the rows behind it kept behind it.
///
/// The time it actually goes out, but never earlier than its own stored stamp, so a
/// conversation's sends keep their stored order. Every later unsent, never-sealed row of the
/// conversation whose stamp would then sort before it is moved just after it, in order, so
/// the line (and what the sender sees) never reshuffles. A sealed row's stamp is the DS's
/// and never moves.
pub(super) fn restamp_on(
    conn: &rusqlite::Connection,
    id: &str,
    conversation_id: &str,
    stored: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    let parse = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.to_utc())
    };
    let stamp = match parse(stored) {
        Some(at) if at > now => at,
        _ => now,
    };
    let behind: Vec<(String, String)> = {
        let mut stmt = conn.prepare(BEHIND_SQL)?;
        let rows = stmt
            .query_map(rusqlite::params![conversation_id, stored, id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    let mut prev = stamp;
    for (later, at) in behind {
        let keep = parse(&at).filter(|t| *t > prev);
        let next = keep.unwrap_or(prev + chrono::Duration::microseconds(1));
        if keep.is_none() {
            conn.execute(
                "UPDATE message SET sent_at = ?1 WHERE id = ?2",
                rusqlite::params![super::stamp_envelope_sent_at(next), later],
            )?;
        }
        prev = next;
    }
    Ok(super::stamp_envelope_sent_at(stamp))
}

/// Deliver the queued message `id`, in its conversation's order (#1636).
///
/// One conversation delivers one message at a time, oldest first: under the conversation's
/// line lock, any earlier send still writing its row is waited for, then every earlier
/// `pending` message is delivered before this one. A failed earlier one is recorded and
/// skipped, never a reason to stop. Every path (send, Retry, the drain) comes through here,
/// so the order holds whichever trigger runs and across restarts.
pub(super) async fn deliver(
    state: &Arc<AppState>,
    id: &str,
    sender_username: Option<String>,
) -> Result<Message> {
    let _sending = crate::notify::lock::suspend::send_started(Some(id));
    let position = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        line_position_on(db.conn(), id)?
    };
    let Some((conversation_id, sent_at)) = position else {
        return deliver_one(state, id, sender_username, Attempt::Own).await;
    };
    let line = OutboxLock::acquire(state, &line_key(&conversation_id)).await;
    let result = deliver_in_line(state, &conversation_id, &sent_at, id, sender_username).await;
    line.release().await;
    result
}

async fn deliver_in_line(
    state: &Arc<AppState>,
    conversation_id: &str,
    sent_at: &str,
    id: &str,
    sender_username: Option<String>,
) -> Result<Message> {
    for gate in earlier_enqueues(state, conversation_id, sent_at, id) {
        drop(gate.lock().await);
    }
    let ahead = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        pending_ahead_on(db.conn(), conversation_id, id)?
    };
    // A row waiting for its file key cannot go yet, so it holds nobody up; the drain or its
    // own retry sends it when the key arrives.
    let ahead: Vec<String> = ahead
        .into_iter()
        .filter(|earlier| !awaiting_key(state, earlier))
        .collect();
    for earlier in ahead {
        if let Err(e) = deliver_one(state, &earlier, None, Attempt::Ahead).await {
            crate::diag!("[outbox] delivery of {earlier}, ahead of {id}, failed: {e}");
        }
    }
    deliver_one(state, id, sender_username, Attempt::Own).await
}

/// Whose attempt this is (#1636), which decides what a transient failure records.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// The row's own send, Retry, or the drain: a transient failure marks it `failed`.
    Own,
    /// A pass for a LATER send of the conversation: a transient failure leaves the row
    /// `pending` (only a refusal no retry fixes is recorded), so another send never changes
    /// what this row shows.
    Ahead,
}

/// Whether `id` last stopped on its conversation's file key (#1636); in memory only, so after a
/// restart such a row gets one more ahead attempt, which the key check ends before any bytes.
fn awaiting_key(state: &Arc<AppState>, id: &str) -> bool {
    state
        .outbox_awaiting_key
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(id)
}

fn set_awaiting_key(state: &Arc<AppState>, id: &str, waiting: bool) {
    let mut set = state
        .outbox_awaiting_key
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if waiting {
        set.insert(id.to_string());
    } else {
        set.remove(id);
    }
}

/// One delivery attempt for `id`, under its own lock.
/// Re-reads the row under the lock; what a failure records depends on [`Attempt`].
async fn deliver_one(
    state: &Arc<AppState>,
    id: &str,
    sender_username: Option<String>,
    attempt: Attempt,
) -> Result<Message> {
    let lock = OutboxLock::acquire(state, id).await;
    let result = deliver_locked(state, id, sender_username, attempt).await;
    lock.release().await;
    result
}

async fn deliver_locked(
    state: &Arc<AppState>,
    id: &str,
    sender_username: Option<String>,
    attempt: Attempt,
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
        // A drained message's author label comes from this device's cached profile.
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
            // Only the file key is missing: stays queued, not failed; the next drain sends it.
            mark_state(state, id, PENDING).await;
            set_awaiting_key(state, id, true);
            return Err(e);
        }
        Err(e) => {
            set_awaiting_key(state, id, false);
            record_failure(state, id, &e, attempt).await;
            return Err(e);
        }
    };
    set_awaiting_key(state, id, false);

    match super::send::deliver_queued(&queued, sender_username, state).await {
        Ok(message) => Ok(message),
        Err(e) => {
            if !super::send::test_hooks::is_simulated_crash(&e) {
                record_failure(state, id, &e, attempt).await;
            }
            Err(e)
        }
    }
}

/// Record a failed attempt; an [`Attempt::Ahead`] pass records only a permanent refusal.
async fn record_failure(state: &Arc<AppState>, id: &str, e: &Error, attempt: Attempt) {
    let permanent = is_permanent(e);
    if attempt == Attempt::Ahead && !permanent {
        return;
    }
    mark_failed(state, id, permanent).await;
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

/// Upload stored attachments one at a time, then swap in the finished `_att` envelope in one transaction.
async fn finish_uploads(state: &Arc<AppState>, mut queued: Queued) -> Result<Queued> {
    let metas = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        attachment_metas_on(db.conn(), &queued.id)?
    };
    if metas.is_empty() {
        return Ok(queued);
    }
    // The key before any bytes (#1636): a row still waiting for it must not read its
    // attachments out of the database on every attempt.
    crate::commands::r2::check_outbox_upload_key(&queued.conversation_id, state).await?;
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
    // Finished before the app is suspended, if iOS gives it the time (`notify::lock::suspend`).
    let _sending = crate::notify::lock::suspend::send_started(Some(&message_id));
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
/// Waits out any attempt in flight; a sealed message is first withdrawn from the DS unless that is impossible for good.
///
/// Returns `false` when there was nothing to discard: the id is unknown, or the
/// message was delivered (a delivered message is deleted with `delete_message`).
pub async fn discard_unsent_message(message_id: String, state: &Arc<AppState>) -> Result<bool> {
    let _sending = crate::notify::lock::suspend::send_started(None);
    let lock = OutboxLock::acquire(state, &message_id).await;
    let result = discard_locked(state, &message_id).await;
    lock.release().await;
    result
}

/// Whose message, where, and whether it was ever sealed.
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

/// Withdraw a possibly-landed message, checking membership first.
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

impl OutboxReport {
    /// Fold a later pass into this one: delivered from either pass, failed only if the later pass did not deliver it.
    fn absorb(&mut self, next: OutboxReport) {
        fold(&mut self.delivered, &mut self.failed, next.delivered, next.failed);
        fold(&mut self.edits_delivered, &mut self.edits_failed, next.edits_delivered, next.edits_failed);
    }
}

fn fold(delivered: &mut Vec<String>, failed: &mut Vec<String>, next_delivered: Vec<String>, next_failed: Vec<String>) {
    failed.retain(|id| !next_delivered.contains(id));
    for id in next_delivered {
        if !delivered.contains(&id) {
            delivered.push(id);
        }
    }
    for id in next_failed {
        if !failed.contains(&id) && !delivered.contains(&id) {
            failed.push(id);
        }
    }
}

/// Deliver every unsent message, oldest first (#1335).
///
/// Single-flight: a call during a drain sets `outbox_rerun` instead of overlapping. Nothing retries in a loop.
pub async fn flush_outbox(state: &Arc<AppState>) -> Result<OutboxReport> {
    use std::sync::atomic::Ordering;
    // The whole drain, so the gaps between its messages do not count as idle; each message
    // also counts by id while `deliver` has it.
    let _sending = crate::notify::lock::suspend::send_started(None);
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
    // Every pass, not just the last: a caller that waited is answered with this
    // report, and the rerun it asked for usually finds nothing left to send.
    let mut report = OutboxReport::default();
    loop {
        state.outbox_rerun.store(false, Ordering::SeqCst);
        report.absorb(drain(state).await?);
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

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }

    fn queued_at(id: &str, conversation: &str, sent_at: &str) -> Queued {
        Queued {
            conversation_id: conversation.to_string(),
            thread_id: None,
            sent_at: sent_at.to_string(),
            ..queued(id, id)
        }
    }

    fn sent_at_of(conn: &rusqlite::Connection, id: &str) -> String {
        conn.query_row("SELECT sent_at FROM message WHERE id = ?1", [id], |r| r.get(0))
            .unwrap()
    }

    fn line(conn: &rusqlite::Connection, conversation: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT id FROM message WHERE conversation_id = ?1 ORDER BY sent_at, id")
            .unwrap();
        stmt.query_map([conversation], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<String>, _>>()
            .unwrap()
    }

    /// A message waits only behind earlier PENDING messages of its own conversation (#1636);
    /// failed, rejected, delivered and other conversations' messages hold nobody up.
    #[test]
    fn only_earlier_pending_messages_of_the_conversation_are_ahead() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let rows = [
            ("01A", "c1", "2026-10-31T10:00:00.000001+00:00", Some(PENDING)),
            ("01B", "c1", "2026-10-31T10:00:00.000002+00:00", Some(FAILED)),
            ("01C", "c1", "2026-10-31T10:00:00.000003+00:00", Some(REJECTED)),
            ("01D", "c1", "2026-10-31T10:00:00.000004+00:00", None),
            ("01E", "c2", "2026-10-31T10:00:00.000005+00:00", Some(PENDING)),
            ("01F", "c1", "2026-10-31T10:00:00.000006+00:00", Some(PENDING)),
            ("01G", "c1", "2026-10-31T10:00:00.000007+00:00", Some(PENDING)),
            ("01H", "c1", "2026-10-31T10:00:00.000008+00:00", Some(PENDING)),
        ];
        for (id, conv, sent_at, state) in rows {
            enqueue_on(conn, &queued_at(id, conv, sent_at), &[]).unwrap();
            match state {
                Some(s) => mark_state_on(conn, id, s).unwrap(),
                None => {
                    settle_on(conn, id).unwrap();
                }
            }
        }
        assert_eq!(pending_ahead_on(conn, "c1", "01A").unwrap(), Vec::<String>::new());
        assert_eq!(pending_ahead_on(conn, "c1", "01G").unwrap(), vec!["01A", "01F"]);
        assert_eq!(pending_ahead_on(conn, "c1", "01H").unwrap(), vec!["01A", "01F", "01G"]);
        assert_eq!(pending_ahead_on(conn, "c2", "01E").unwrap(), Vec::<String>::new());
        // Equal stamps fall back to the id, the same tie-break as the drain.
        enqueue_on(conn, &queued_at("01I", "c3", "2026-10-31T10:00:00+00:00"), &[]).unwrap();
        enqueue_on(conn, &queued_at("01J", "c3", "2026-10-31T10:00:00+00:00"), &[]).unwrap();
        assert_eq!(pending_ahead_on(conn, "c3", "01J").unwrap(), vec!["01I"]);
        assert!(pending_ahead_on(conn, "c3", "01I").unwrap().is_empty());
    }

    /// The line's queries read the partial index of unsent rows, never the whole
    /// conversation (#1636).
    #[test]
    fn the_line_queries_read_only_unsent_rows() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        for sql in [AHEAD_SQL, BEHIND_SQL] {
            let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let plan: Vec<String> = stmt
                .query_map(rusqlite::params!["c1", "2026-10-31T10:00:00+00:00", "01A"], |r| {
                    r.get::<_, String>(3)
                })
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert!(
                plan.iter().any(|step| step.contains("idx_message_unsent_line")),
                "{sql}\n{plan:?}"
            );
            assert!(
                !plan.iter().any(|step| step.contains("TEMP B-TREE")),
                "the index gives the order: {plan:?}"
            );
        }
    }

    /// A seal stamps the time it goes out, never earlier than the row's own stamp, and the
    /// unsent rows behind it stay behind it, in their order (#1636).
    #[test]
    fn a_restamp_keeps_the_conversation_in_stored_order() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let t = |n: u32| format!("2026-10-31T10:00:0{n}+00:00");
        for (i, id) in ["01A", "01B", "01C", "01D"].iter().enumerate() {
            enqueue_on(conn, &queued_at(id, "c1", &t(i as u32 + 1)), &[]).unwrap();
        }
        // 01D was sealed once: its stamp is the DS's and must not move.
        conn.execute("UPDATE message SET ciphertext = X'01' WHERE id = '01D'", []).unwrap();
        let before_d = sent_at_of(conn, "01D");
        enqueue_on(conn, &queued_at("01Z", "c2", &t(1)), &[]).unwrap();

        // 01A goes out late, at :07: B and C were stored after it, so they move behind it.
        let stamp = restamp_on(conn, "01A", "c1", &t(1), at("2026-10-31T10:00:07+00:00")).unwrap();
        conn.execute("UPDATE message SET sent_at = ?1 WHERE id = '01A'", [&stamp]).unwrap();
        assert_eq!(at(&stamp), at("2026-10-31T10:00:07+00:00"));
        assert!(at(&sent_at_of(conn, "01B")) > at(&stamp));
        assert!(at(&sent_at_of(conn, "01C")) > at(&sent_at_of(conn, "01B")));
        assert_eq!(sent_at_of(conn, "01D"), before_d, "a sealed stamp never moves");
        assert_eq!(sent_at_of(conn, "01Z"), t(1), "another conversation is untouched");
        let order = line(conn, "c1");
        let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
        assert!(pos("01A") < pos("01B") && pos("01B") < pos("01C"), "{order:?}");

        // A clock behind the stored stamp never moves a message earlier.
        let stored_b = sent_at_of(conn, "01B");
        let stamp_b = restamp_on(conn, "01B", "c1", &stored_b, at("2026-10-31T09:00:00+00:00")).unwrap();
        assert_eq!(at(&stamp_b), at(&stored_b));
    }

    /// The message is stored as unsent before anything else happens.
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
    fn a_send_frozen_by_the_suspension_reads_unsent_and_stays_queued() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let (out, sent, refused) = (new_message_id(), new_message_id(), new_message_id());
        for id in [&out, &sent, &refused] {
            enqueue_on(conn, &queued(id, "x"), &[]).unwrap();
        }
        conn.execute("UPDATE message SET send_state = NULL WHERE id = ?1", [&sent]).unwrap();
        mark_state_on(conn, &refused, REJECTED).unwrap();
        let ids = vec![out.clone(), sent.clone(), refused.clone(), "unknown".to_string()];
        assert_eq!(mark_suspended_sends_on(conn, &ids).unwrap(), 1);
        assert_eq!(send_state(conn, &out).as_deref(), Some(FAILED), "shows Not sent");
        assert_eq!(send_state(conn, &sent), None, "a delivered row is never demoted");
        assert_eq!(send_state(conn, &refused).as_deref(), Some(REJECTED));
        // Still queued: the resume or next launch's drain delivers it.
        assert_eq!(unsent_ids_on(conn).unwrap(), vec![out]);
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
            sensitive: false,
            poster: None,
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

    /// Bytes are stored before the row and read back one attachment at a time.
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
            poster: None,
        }];
        assert_eq!(
            final_content("", &atts, &uploaded),
            r#"{"_att":[{"key":"media/abc.enc","url":"https://r2/media/abc.enc","name":"clip.mp4","ct":"video/mp4","size":1,"hash":"abc","kv":2,"sha":"d1g","bh":"LKO2","w":640,"h":3}]}"#
        );
    }

    /// #1568: a sensitive attachment round-trips as `sens: true`; an unmarked one has no `sens`.
    #[test]
    fn a_sensitive_attachment_carries_sens_through_the_outbox() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let id = new_message_id();
        let metas = [
            AttMeta { sensitive: true, ..meta("a.png", 5) },
            meta("b.png", 6),
        ];
        store_attachment_on(conn, &id, 0, &metas[0], b"first").unwrap();
        store_attachment_on(conn, &id, 1, &metas[1], b"second").unwrap();
        enqueue_on(conn, &queued(&id, ""), &metas).unwrap();

        let stored = attachment_metas_on(conn, &id).unwrap();
        assert!(stored[0].sensitive);
        assert!(!stored[1].sensitive);
        let (row, _) = load(conn, &id).unwrap().unwrap();
        assert_eq!(
            row.content.unwrap(),
            r#"{"_att":[{"name":"a.png","ct":"image/png","size":5,"bh":"LKO2","w":4,"h":3,"sens":true},{"name":"b.png","ct":"image/png","size":6,"bh":"LKO2","w":4,"h":3}]}"#
        );

        let upload = |n: &str| crate::commands::r2::MediaUploadResult {
            key: format!("media/{n}.enc"),
            url: String::new(),
            filename: format!("{n}.png"),
            content_type: "image/png".to_string(),
            size_bytes: 1,
            content_hash: n.to_string(),
            key_version: 1,
            sha256: "s".to_string(),
            blurhash: None,
            width: None,
            height: None,
            poster: None,
        };
        let done = final_content("", &stored, &[upload("a"), upload("b")]);
        let v: serde_json::Value = serde_json::from_str(&done).unwrap();
        assert_eq!(v["_att"][0]["sens"], serde_json::Value::Bool(true));
        assert!(v["_att"][1].get("sens").is_none());
    }

    /// #1837: a video's poster is stored with its bytes and travels as `pv` in
    /// both the waiting and the finished envelope; nothing else gets one.
    #[test]
    fn a_video_poster_round_trips_through_the_outbox_as_pv() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let id = new_message_id();
        let metas = [
            AttMeta {
                content_type: "video/mp4".to_string(),
                poster: Some("/9j/AAAA".to_string()),
                ..meta("clip.mp4", 5)
            },
            meta("b.png", 6),
        ];
        store_attachment_on(conn, &id, 0, &metas[0], b"first").unwrap();
        store_attachment_on(conn, &id, 1, &metas[1], b"second").unwrap();
        enqueue_on(conn, &queued(&id, ""), &metas).unwrap();

        let stored = attachment_metas_on(conn, &id).unwrap();
        assert_eq!(stored[0].poster.as_deref(), Some("/9j/AAAA"));
        assert_eq!(stored[1].poster, None);
        let (row, _) = load(conn, &id).unwrap().unwrap();
        assert_eq!(
            row.content.unwrap(),
            r#"{"_att":[{"name":"clip.mp4","ct":"video/mp4","size":5,"bh":"LKO2","w":4,"h":3,"pv":"/9j/AAAA"},{"name":"b.png","ct":"image/png","size":6,"bh":"LKO2","w":4,"h":3}]}"#
        );

        let upload = |n: &str, ct: &str| crate::commands::r2::MediaUploadResult {
            key: format!("media/{n}.enc"),
            url: String::new(),
            filename: n.to_string(),
            content_type: ct.to_string(),
            size_bytes: 1,
            content_hash: n.to_string(),
            key_version: 1,
            sha256: "s".to_string(),
            blurhash: None,
            width: None,
            height: None,
            poster: None,
        };
        let done = final_content(
            "",
            &stored,
            &[upload("clip.mp4", "video/mp4"), upload("b.png", "image/png")],
        );
        let v: serde_json::Value = serde_json::from_str(&done).unwrap();
        assert_eq!(v["_att"][0]["pv"], "/9j/AAAA");
        assert_eq!(v["_att"][0]["bh"], "LKO2");
        assert!(v["_att"][1].get("pv").is_none());
    }

    /// #1837: the frame handed over becomes the poster, and its blurhash and
    /// size replace the renderer's; a frame that does not decode sends without one.
    #[tokio::test]
    async fn planning_a_video_turns_its_frame_into_a_bounded_poster() {
        use base64::Engine as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        std::fs::write(&path, b"not really a video").unwrap();
        let img = image::RgbImage::from_fn(640, 360, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
        });
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let frame = base64::engine::general_purpose::STANDARD.encode(png);
        let att = |ct: &str, poster: Option<String>| OutgoingAttachment {
            path: Some(path.to_string_lossy().into_owned()),
            staged_id: None,
            filename: "clip.mp4".to_string(),
            content_type: ct.to_string(),
            blurhash: Some("renderer".to_string()),
            width: Some(1),
            height: Some(1),
            sensitive: false,
            poster,
        };
        let metas = plan_attachments(&[
            att("video/mp4", Some(frame.clone())),
            att("video/mp4", Some("garbage".to_string())),
            att("image/png", Some(frame)),
        ])
        .await
        .unwrap();

        let poster = metas[0].poster.as_deref().expect("a poster");
        let jpeg = base64::engine::general_purpose::STANDARD.decode(poster).unwrap();
        assert!(jpeg.len() <= crate::commands::poster::POSTER_MAX_JPEG_BYTES);
        assert_ne!(metas[0].blurhash.as_deref(), Some("renderer"));
        assert_eq!((metas[0].width, metas[0].height), (Some(640), Some(360)));
        // An undecodable frame keeps the renderer's values and sends without a poster.
        assert_eq!(metas[1].poster, None);
        assert_eq!(metas[1].blurhash.as_deref(), Some("renderer"));
        // Only a video has a poster.
        assert_eq!(metas[2].poster, None);
    }

    /// #1837, old clients: every pre-poster parser ignores `pv`, and an old
    /// renderer's attachment (no `poster` field) still deserializes.
    #[test]
    fn pv_is_invisible_to_clients_that_predate_it() {
        let with = r#"{"_att":[{"key":"media/a.enc","url":"","name":"a.mp4","ct":"video/mp4","size":1,"hash":"aa","kv":1,"sha":"s","bh":"LKO2","w":4,"h":3,"pv":"/9j/AAAA"}],"_txt":"look"}"#;
        let without = r#"{"_att":[{"key":"media/a.enc","url":"","name":"a.mp4","ct":"video/mp4","size":1,"hash":"aa","kv":1,"sha":"s","bh":"LKO2","w":4,"h":3}],"_txt":"look"}"#;
        let refs = |c: &str| {
            super::super::edit_delete::parse_attachment_refs(c)
                .into_iter()
                .map(|r| (r.content_hash, r.r2_key))
                .collect::<Vec<_>>()
        };
        assert_eq!(refs(with), refs(without));
        assert_eq!(
            crate::db::search_text::search_text(with),
            crate::db::search_text::search_text(without)
        );
        assert_eq!(caption_of(with), "look");

        let old: OutgoingAttachment = serde_json::from_str(
            r#"{"path":"/x/a.mp4","filename":"a.mp4","contentType":"video/mp4","blurhash":"LKO2","width":4,"height":3}"#,
        )
        .unwrap();
        assert_eq!(old.poster, None);
    }

    /// #1568, old clients: every pre-flag parser ignores `sens`.
    #[test]
    fn sens_is_invisible_to_clients_that_predate_it() {
        let marked = r#"{"_att":[{"key":"media/a.enc","url":"","name":"a.png","ct":"image/png","size":1,"hash":"aa","kv":1,"sha":"s","bh":"LKO2","sens":true}],"_txt":"look"}"#;
        let plain = r#"{"_att":[{"key":"media/a.enc","url":"","name":"a.png","ct":"image/png","size":1,"hash":"aa","kv":1,"sha":"s","bh":"LKO2"}],"_txt":"look"}"#;

        let refs = |c: &str| {
            super::super::edit_delete::parse_attachment_refs(c)
                .into_iter()
                .map(|r| (r.content_hash, r.r2_key))
                .collect::<Vec<_>>()
        };
        assert_eq!(refs(marked), refs(plain));
        assert_eq!(refs(marked).len(), 1);
        assert_eq!(
            crate::db::search_text::search_text(marked),
            crate::db::search_text::search_text(plain)
        );
        assert!(crate::db::search_text::has_attachment(marked));
        assert_eq!(caption_of(marked), "look");

        let old: OutgoingAttachment = serde_json::from_str(
            r#"{"stagedId":"s1","filename":"a.png","contentType":"image/png"}"#,
        )
        .unwrap();
        assert!(!old.sensitive);
        let new: OutgoingAttachment = serde_json::from_str(
            r#"{"stagedId":"s1","filename":"a.png","contentType":"image/png","sensitive":true}"#,
        )
        .unwrap();
        assert!(new.sensitive);
    }

    /// A redaction reaching an unsent row applies and settles it.
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

    /// A caller that waited on a running drain is answered with the whole drain (#1335):
    /// the rerun it asked for finds nothing, and an empty report hid every delivery from
    /// the renderer, which then showed the rows as sending until something else refreshed.
    #[test]
    fn a_rerun_that_finds_nothing_keeps_what_the_first_pass_delivered() {
        let mut total = OutboxReport::default();
        total.absorb(OutboxReport {
            delivered: vec!["a".into(), "b".into()],
            failed: vec!["c".into()],
            edits_delivered: vec!["e".into()],
            edits_failed: vec![],
        });
        total.absorb(OutboxReport::default());
        assert_eq!(total.delivered, vec!["a", "b"]);
        assert_eq!(total.failed, vec!["c"]);
        assert_eq!(total.edits_delivered, vec!["e"]);

        // Failed, then delivered on the rerun: delivered only.
        total.absorb(OutboxReport { delivered: vec!["c".into()], ..Default::default() });
        assert_eq!(total.delivered, vec!["a", "b", "c"]);
        assert!(total.failed.is_empty());

        // Delivered stays delivered; a repeat is not counted twice.
        total.absorb(OutboxReport { delivered: vec!["a".into()], failed: vec!["b".into()], ..Default::default() });
        assert_eq!(total.delivered, vec!["a", "b", "c"]);
        assert!(total.failed.is_empty());
    }
}
