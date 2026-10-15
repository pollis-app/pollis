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
use serde::Serialize;
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
pub(super) fn enqueue_on(conn: &rusqlite::Connection, queued: &Queued) -> Result<Enqueued> {
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
    conn.execute(
        "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, reply_to_id, thread_id, sent_at, send_state)
         VALUES (?1, ?2, ?3, X'', ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            queued.id,
            queued.conversation_id,
            queued.sender_id,
            queued.content,
            queued.reply_to_id,
            queued.thread_id,
            super::envelope_sent_at(),
            PENDING,
        ],
    )?;
    Ok(Enqueued::Unsent)
}

pub(super) async fn enqueue(state: &Arc<AppState>, queued: &Queued) -> Result<Enqueued> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_signed_in)?;
    enqueue_on(db.conn(), queued)
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

    match super::send::deliver_queued(&queued, sender_username, state).await {
        Ok(message) => Ok(message),
        Err(e) => {
            if !super::send::test_hooks::is_simulated_crash(&e) {
                let guard = state.local_db.lock().await;
                if let Some(db) = guard.as_ref() {
                    if let Err(mark) = mark_failed_on(db.conn(), id) {
                        crate::diag!("[outbox] could not mark {id} failed: {mark}");
                    }
                }
            }
            Err(e)
        }
    }
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
        assert!(matches!(enqueue_on(conn, &queued(&id, "hello")).unwrap(), Enqueued::Unsent));
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
        enqueue_on(conn, &queued(&id, "hello")).unwrap();
        assert!(matches!(enqueue_on(conn, &queued(&id, "hello")).unwrap(), Enqueued::Unsent));
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM message WHERE id = ?1", [&id], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        conn.execute("UPDATE message SET send_state = NULL WHERE id = ?1", [&id]).unwrap();
        match enqueue_on(conn, &queued(&id, "hello")).unwrap() {
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
        enqueue_on(conn, &queued(&id, "hello")).unwrap();
        let mut other = queued(&id, "hello");
        other.conversation_id = "c2".to_string();
        assert!(enqueue_on(conn, &other).is_err());
    }

    /// Failing only ever applies to an unsent row; a delivered one stays
    /// delivered.
    #[test]
    fn mark_failed_never_demotes_a_delivered_message() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        let (a, b) = (new_message_id(), new_message_id());
        enqueue_on(conn, &queued(&a, "a")).unwrap();
        enqueue_on(conn, &queued(&b, "b")).unwrap();
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

    #[test]
    fn caller_supplied_ids_must_be_ulids() {
        assert!(checked_message_id(new_message_id()).is_ok());
        assert!(checked_message_id("pending-123".to_string()).is_err());
        assert!(checked_message_id(String::new()).is_err());
    }
}
