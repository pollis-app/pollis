//! Where the human has read up to, and the unread counts derived from it (#844).
//!
//! # Why a cursor and not a count
//!
//! Unread state used to live in one in-memory MobX field whose only writer was
//! the live-event dispatcher. Two consequences, both of which made users miss
//! messages — the failure mode the product promise is written against:
//!
//!   * a restart marked every conversation read, because the field started
//!     empty and nothing rehydrated it;
//!   * anything that arrived while the app was closed never badged, because no
//!     live event fired for it.
//!
//! A count cannot be persisted honestly: it records *how many* without
//! recording *which*, so it cannot be reconciled against the messages actually
//! on disk, nor merged with another device's idea of the same number. A cursor
//! can. Ingest already writes every message it accepts to the local `message`
//! table, so once the read position is durable the count is a query — and a
//! message that arrived while the app was shut is counted for free.
//!
//! # The position
//!
//! `(last_read_at, last_read_message_id)`, compared as a row value so it
//! matches the `ORDER BY sent_at DESC, id DESC` the message list itself uses.
//! `sent_at` alone is sender-supplied and two messages can carry the same one.
//!
//! # Why the cursor only moves forward
//!
//! [`advance_read_cursor`] refuses to move a cursor backwards, and that single
//! rule is what makes the multi-device story tractable. Merging two devices'
//! cursors is `max()`: commutative, idempotent, and order-independent, so there
//! is no lock, no last-writer-wins race and no conflict to resolve. Replaying a
//! stale update is a no-op rather than a regression that would re-badge
//! messages the human already read.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::AppState;

/// One conversation's unread tally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnreadCount {
    pub conversation_id: String,
    pub count: i64,
}

/// A read position, as stored and as synced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadCursor {
    pub conversation_id: String,
    pub last_read_at: String,
    pub last_read_message_id: String,
}

/// Is `candidate` strictly newer than `current`?
///
/// The whole ordering rule in one place, so the SQL, the merge and the tests
/// cannot drift apart. Mirrors `ORDER BY sent_at DESC, id DESC`.
pub(crate) fn is_newer(candidate: (&str, &str), current: (&str, &str)) -> bool {
    candidate > current
}

/// Move a conversation's cursor to `(at, message_id)`, or leave it alone if
/// that is not strictly newer than where it already is.
///
/// Returns whether the cursor actually moved, which is what lets a caller skip
/// a pointless sync push.
pub(crate) fn advance_read_cursor(
    conn: &Connection,
    conversation_id: &str,
    at: &str,
    message_id: &str,
) -> Result<bool> {
    let current: Option<(String, String)> = conn
        .query_row(
            "SELECT last_read_at, last_read_message_id FROM read_cursor
             WHERE conversation_id = ?1",
            rusqlite::params![conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| Error::Other(anyhow::anyhow!("read cursor lookup: {e}")))?;

    if let Some((cur_at, cur_id)) = &current {
        if !is_newer((at, message_id), (cur_at.as_str(), cur_id.as_str())) {
            return Ok(false);
        }
    }

    conn.execute(
        "INSERT INTO read_cursor (conversation_id, last_read_at, last_read_message_id, updated_at)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT(conversation_id) DO UPDATE SET
             last_read_at         = excluded.last_read_at,
             last_read_message_id = excluded.last_read_message_id,
             updated_at           = excluded.updated_at",
        rusqlite::params![conversation_id, at, message_id],
    )
    .map_err(|e| Error::Other(anyhow::anyhow!("advance read cursor: {e}")))?;
    Ok(true)
}

/// Every cursor this device holds — the payload the cross-device sync encrypts.
pub(crate) fn all_read_cursors(conn: &Connection) -> Result<Vec<ReadCursor>> {
    let mut stmt = conn
        .prepare(
            "SELECT conversation_id, last_read_at, last_read_message_id
             FROM read_cursor ORDER BY conversation_id",
        )
        .map_err(|e| Error::Other(anyhow::anyhow!("prepare cursors: {e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ReadCursor {
                conversation_id: row.get(0)?,
                last_read_at: row.get(1)?,
                last_read_message_id: row.get(2)?,
            })
        })
        .map_err(|e| Error::Other(anyhow::anyhow!("query cursors: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| Error::Other(anyhow::anyhow!("row: {e}")))?);
    }
    Ok(out)
}

/// Apply another device's cursors, keeping whichever position is newer.
///
/// Returns how many actually moved this device forward.
pub(crate) fn merge_read_cursors(conn: &Connection, incoming: &[ReadCursor]) -> Result<usize> {
    let mut moved = 0;
    for c in incoming {
        if advance_read_cursor(
            conn,
            &c.conversation_id,
            &c.last_read_at,
            &c.last_read_message_id,
        )? {
            moved += 1;
        }
    }
    Ok(moved)
}

/// Unread counts for every conversation this device holds messages for.
///
/// A conversation with nothing unread is omitted rather than reported as zero,
/// so the caller can treat the result as the complete set of badges to show.
///
/// Three exclusions, all deliberate:
///   * **own messages** — reading your own is not a signal, and counting them
///     would badge every conversation you just spoke in;
///   * **deleted messages** — a redacted message must not hold a badge open
///     that the user can never clear by reading;
///   * **no cursor yet** — everything from someone else counts, which is what
///     makes a conversation that arrived entirely while offline badge correctly
///     on first launch.
pub(crate) fn unread_counts(conn: &Connection, user_id: &str) -> Result<Vec<UnreadCount>> {
    let mut stmt = conn
        .prepare(
            "SELECT m.conversation_id, COUNT(*)
             FROM message m
             LEFT JOIN read_cursor c ON c.conversation_id = m.conversation_id
             WHERE m.sender_id != ?1
               AND m.deleted_at IS NULL
               AND (
                     c.conversation_id IS NULL
                     OR (m.sent_at, m.id) > (c.last_read_at, c.last_read_message_id)
                   )
             GROUP BY m.conversation_id
             HAVING COUNT(*) > 0
             ORDER BY m.conversation_id",
        )
        .map_err(|e| Error::Other(anyhow::anyhow!("prepare unread: {e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![user_id], |row| {
            Ok(UnreadCount {
                conversation_id: row.get(0)?,
                count: row.get(1)?,
            })
        })
        .map_err(|e| Error::Other(anyhow::anyhow!("query unread: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| Error::Other(anyhow::anyhow!("row: {e}")))?);
    }
    Ok(out)
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// Unread counts to hydrate the UI with at startup.
pub async fn get_unread_counts(user_id: String, state: &Arc<AppState>) -> Result<Vec<UnreadCount>> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    unread_counts(db.conn(), &user_id)
}

/// Record that the human has read up to `message_id` in this conversation.
///
/// Takes the message's own `sent_at` rather than "now": the cursor is a
/// position in the conversation, not a wall-clock moment, and using the clock
/// would let a device with a fast clock mark unseen messages read.
pub async fn set_read_cursor(
    conversation_id: String,
    message_id: String,
    sent_at: String,
    state: &Arc<AppState>,
) -> Result<bool> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    advance_read_cursor(db.conn(), &conversation_id, &sent_at, &message_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "u-me";
    const THEM: &str = "u-them";
    const CONV: &str = "c1";

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::db::local::schema_sql()).unwrap();
        conn
    }

    /// `sent_at` values are ordered strings, so the fixtures read in order.
    fn msg(conn: &Connection, id: &str, sender: &str, sent_at: &str) {
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES (?1, ?2, ?3, X'00', 'body', ?4)",
            rusqlite::params![id, CONV, sender, sent_at],
        )
        .unwrap();
    }

    fn count(conn: &Connection) -> i64 {
        unread_counts(conn, ME)
            .unwrap()
            .iter()
            .find(|u| u.conversation_id == CONV)
            .map(|u| u.count)
            .unwrap_or(0)
    }

    // The reported bug, from the other side: messages that arrived while the
    // app was shut have no cursor, and must all badge on first launch.
    #[test]
    fn with_no_cursor_every_message_from_someone_else_is_unread() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");
        assert_eq!(count(&conn), 2);
    }

    #[test]
    fn reading_your_own_messages_is_not_a_signal() {
        let conn = db();
        msg(&conn, "m1", ME, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", ME, "2026-01-01T00:00:02Z");
        assert_eq!(count(&conn), 0, "your own messages badged the conversation");
    }

    // A redacted message must not hold a badge open that reading cannot clear.
    #[test]
    fn deleted_messages_do_not_hold_a_badge_open() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        conn.execute(
            "UPDATE message SET deleted_at = '2026-01-01T00:00:05Z' WHERE id = 'm1'",
            [],
        )
        .unwrap();
        assert_eq!(count(&conn), 0);
    }

    #[test]
    fn advancing_the_cursor_clears_what_it_passes() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");
        msg(&conn, "m3", THEM, "2026-01-01T00:00:03Z");
        assert_eq!(count(&conn), 3);

        assert!(advance_read_cursor(&conn, CONV, "2026-01-01T00:00:02Z", "m2").unwrap());
        assert_eq!(count(&conn), 1, "only the message past the cursor is unread");
    }

    /// The invariant the whole design rests on. If a cursor could move
    /// backwards, a stale device replaying an old position would re-badge
    /// messages the human has already read — and multi-device merge would stop
    /// being order-independent.
    #[test]
    fn a_cursor_never_moves_backwards() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");

        assert!(advance_read_cursor(&conn, CONV, "2026-01-01T00:00:02Z", "m2").unwrap());
        assert_eq!(count(&conn), 0);

        // An older position, and the same position again.
        assert!(!advance_read_cursor(&conn, CONV, "2026-01-01T00:00:01Z", "m1").unwrap());
        assert!(!advance_read_cursor(&conn, CONV, "2026-01-01T00:00:02Z", "m2").unwrap());
        assert_eq!(count(&conn), 0, "a stale update re-badged read messages");
    }

    /// Two messages can share a `sent_at` — it is sender-supplied. The cursor
    /// compares the id as well, exactly as the message list sorts.
    #[test]
    fn messages_sharing_a_timestamp_are_ordered_by_id() {
        let conn = db();
        msg(&conn, "m-a", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m-b", THEM, "2026-01-01T00:00:01Z");
        assert_eq!(count(&conn), 2);

        advance_read_cursor(&conn, CONV, "2026-01-01T00:00:01Z", "m-a").unwrap();
        assert_eq!(count(&conn), 1, "the tie was not broken by id");
    }

    #[test]
    fn a_conversation_with_nothing_unread_is_absent_rather_than_zero() {
        let conn = db();
        msg(&conn, "m1", ME, "2026-01-01T00:00:01Z");
        assert!(unread_counts(&conn, ME).unwrap().is_empty());
    }

    // ── Multi-device merge ───────────────────────────────────────────────────

    fn cursor(at: &str, id: &str) -> ReadCursor {
        ReadCursor {
            conversation_id: CONV.to_string(),
            last_read_at: at.to_string(),
            last_read_message_id: id.to_string(),
        }
    }

    /// Merge is `max()`, so the order two devices' updates arrive in cannot
    /// change where the cursor ends up. This is what removes the need for any
    /// locking or last-writer-wins tie-break in the sync path.
    #[test]
    fn merging_cursors_is_order_independent() {
        let older = cursor("2026-01-01T00:00:01Z", "m1");
        let newer = cursor("2026-01-01T00:00:09Z", "m9");

        let forward = db();
        merge_read_cursors(&forward, &[older.clone(), newer.clone()]).unwrap();

        let backward = db();
        merge_read_cursors(&backward, &[newer.clone(), older.clone()]).unwrap();

        assert_eq!(
            all_read_cursors(&forward).unwrap(),
            all_read_cursors(&backward).unwrap()
        );
        assert_eq!(all_read_cursors(&forward).unwrap(), vec![newer]);
    }

    #[test]
    fn merging_the_same_cursors_twice_changes_nothing() {
        let conn = db();
        let c = vec![cursor("2026-01-01T00:00:05Z", "m5")];
        assert_eq!(merge_read_cursors(&conn, &c).unwrap(), 1);
        assert_eq!(
            merge_read_cursors(&conn, &c).unwrap(),
            0,
            "a replayed sync moved the cursor again"
        );
    }

    #[test]
    fn a_merge_reports_only_the_cursors_that_actually_moved() {
        let conn = db();
        advance_read_cursor(&conn, CONV, "2026-01-01T00:00:05Z", "m5").unwrap();
        let incoming = vec![
            cursor("2026-01-01T00:00:01Z", "m1"),
            ReadCursor {
                conversation_id: "c2".into(),
                last_read_at: "2026-01-01T00:00:01Z".into(),
                last_read_message_id: "n1".into(),
            },
        ];
        assert_eq!(merge_read_cursors(&conn, &incoming).unwrap(), 1);
    }

    #[test]
    fn cursors_round_trip_through_the_sync_payload_shape() {
        let conn = db();
        advance_read_cursor(&conn, CONV, "2026-01-01T00:00:05Z", "m5").unwrap();
        advance_read_cursor(&conn, "c2", "2026-01-02T00:00:00Z", "n7").unwrap();

        let all = all_read_cursors(&conn).unwrap();
        let json = serde_json::to_string(&all).unwrap();
        let back: Vec<ReadCursor> = serde_json::from_str(&json).unwrap();

        let fresh = db();
        assert_eq!(merge_read_cursors(&fresh, &back).unwrap(), 2);
        assert_eq!(all_read_cursors(&fresh).unwrap(), all);
    }
}
