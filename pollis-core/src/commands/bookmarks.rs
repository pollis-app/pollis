//! Saved messages (bookmarks) and message permalinks (#854), entirely in the
//! encrypted local DB. No DS endpoint: a bookmark set is interest metadata the
//! server must not see. See `.codesight/wiki/commands.md`.
//!
//! A permalink (`pollis://m/<conversation_id>/<message_id>`) is a pointer, not a
//! capability. [`resolve_permalink`] answers strictly against the local DB; a
//! server-side "fetch message by id" would turn it into a leak. A miss returns a
//! bare `found: false` that hints at nothing.

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

/// One saved item. Body fields are `Option` because a bookmark outlives its
/// message (retention eviction, soft delete) and renders as a placeholder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedMessage {
    pub message_id: String,
    pub conversation_id: String,
    /// When the user saved it, not when it was sent.
    pub saved_at: String,
    pub available: bool,
    pub content: Option<String>,
    pub sender_id: Option<String>,
    pub sent_at: Option<String>,
}

/// Result of resolving a permalink. On a miss every field but `found` is `None`,
/// so it can't probe for messages this device isn't entitled to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermalinkTarget {
    pub found: bool,
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
}

impl PermalinkTarget {
    /// The single unresolvable answer; every failure mode funnels here so they are
    /// indistinguishable.
    fn unresolved() -> Self {
        Self { found: false, conversation_id: None, message_id: None }
    }
}

// Free of `AppState` so these unit-test against an in-memory `LocalDb`.

/// Save a message. The conversation comes from the local `message` row, not the
/// caller, so a bookmark can't point at the wrong one; a message this device
/// doesn't hold is refused. Idempotent (primary key).
pub fn add_bookmark(conn: &Connection, message_id: &str) -> Result<()> {
    let conversation_id: Option<String> = conn
        .query_row(
            "SELECT conversation_id FROM message WHERE id = ?1",
            rusqlite::params![message_id],
            |row| row.get(0),
        )
        .optional()?;

    let Some(conversation_id) = conversation_id else {
        return Err(Error::Other(anyhow::anyhow!(
            "cannot save a message this device does not hold"
        )));
    };

    conn.execute(
        "INSERT OR IGNORE INTO bookmark (message_id, conversation_id) VALUES (?1, ?2)",
        rusqlite::params![message_id, conversation_id],
    )?;
    Ok(())
}

/// Unsave a message; true if one was removed. Works after the body is gone.
pub fn remove_bookmark(conn: &Connection, message_id: &str) -> Result<bool> {
    let removed = conn.execute(
        "DELETE FROM bookmark WHERE message_id = ?1",
        rusqlite::params![message_id],
    )?;
    Ok(removed > 0)
}

pub fn is_bookmarked(conn: &Connection, message_id: &str) -> Result<bool> {
    let hit: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM bookmark WHERE message_id = ?1",
            rusqlite::params![message_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(hit.is_some())
}

/// Flip the saved state and return the state after the flip.
pub fn toggle_bookmark(conn: &Connection, message_id: &str) -> Result<bool> {
    if remove_bookmark(conn, message_id)? {
        return Ok(false);
    }
    add_bookmark(conn, message_id)?;
    Ok(true)
}

/// Every saved message, newest save first. LEFT JOIN so evicted or soft-deleted
/// messages come back as `available: false`.
pub fn list_bookmarks(conn: &Connection) -> Result<Vec<SavedMessage>> {
    let mut stmt = conn.prepare(
        "SELECT b.message_id, b.conversation_id, b.created_at,
                m.content, m.sender_id, m.sent_at, m.deleted_at
         FROM bookmark b
         LEFT JOIN message m ON m.id = b.message_id
         ORDER BY b.created_at DESC, b.message_id DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        let content: Option<String> = row.get(3)?;
        let sender_id: Option<String> = row.get(4)?;
        let sent_at: Option<String> = row.get(5)?;
        let deleted_at: Option<String> = row.get(6)?;
        let available = sender_id.is_some() && deleted_at.is_none();
        Ok(SavedMessage {
            message_id: row.get(0)?,
            conversation_id: row.get(1)?,
            saved_at: row.get(2)?,
            available,
            content: if available { content } else { None },
            sender_id: if available { sender_id } else { None },
            sent_at: if available { sent_at } else { None },
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Resolve a permalink against the local DB only. The message must be in the
/// conversation the link claims, so a real message id can't be paired with a
/// guessed conversation to confirm where it lives.
pub fn resolve_permalink(
    conn: &Connection,
    conversation_id: &str,
    message_id: &str,
) -> Result<PermalinkTarget> {
    let hit: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM message WHERE id = ?1 AND conversation_id = ?2",
            rusqlite::params![message_id, conversation_id],
            |row| row.get(0),
        )
        .optional()?;

    if hit.is_none() {
        return Ok(PermalinkTarget::unresolved());
    }

    Ok(PermalinkTarget {
        found: true,
        conversation_id: Some(conversation_id.to_string()),
        message_id: Some(message_id.to_string()),
    })
}

/// `f` runs with no `.await`, so the non-`Send` connection never crosses a
/// suspension point.
async fn with_local<T, F>(state: &Arc<AppState>, f: F) -> Result<T>
where
    F: FnOnce(&Connection) -> Result<T> + Send,
    T: Send,
{
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    f(db.conn())
}

pub async fn save_message(message_id: String, state: &Arc<AppState>) -> Result<()> {
    with_local(state, |conn| add_bookmark(conn, &message_id)).await
}

pub async fn unsave_message(message_id: String, state: &Arc<AppState>) -> Result<bool> {
    with_local(state, |conn| remove_bookmark(conn, &message_id)).await
}

pub async fn toggle_saved_message(message_id: String, state: &Arc<AppState>) -> Result<bool> {
    with_local(state, |conn| toggle_bookmark(conn, &message_id)).await
}

pub async fn list_saved_messages(state: &Arc<AppState>) -> Result<Vec<SavedMessage>> {
    with_local(state, list_bookmarks).await
}

pub async fn resolve_message_permalink(
    conversation_id: String,
    message_id: String,
    state: &Arc<AppState>,
) -> Result<PermalinkTarget> {
    with_local(state, |conn| {
        resolve_permalink(conn, &conversation_id, &message_id)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::local::LocalDb;

    fn db() -> LocalDb {
        LocalDb::open_in_memory().expect("in-memory db")
    }

    fn seed_message(conn: &Connection, id: &str, conversation_id: &str, content: &str) {
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES (?1, ?2, 'alice', X'01', ?3, '2024-01-01T00:00:00Z')",
            rusqlite::params![id, conversation_id, content],
        )
        .expect("seed message");
    }

    #[test]
    fn save_unsave_round_trips() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "m1", "conv1", "hello");

        assert!(!is_bookmarked(conn, "m1").unwrap(), "starts unsaved");

        add_bookmark(conn, "m1").expect("save");
        assert!(is_bookmarked(conn, "m1").unwrap(), "saved");

        let saved = list_bookmarks(conn).expect("list");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].message_id, "m1");
        assert_eq!(saved[0].conversation_id, "conv1");
        assert!(saved[0].available);
        assert_eq!(saved[0].content.as_deref(), Some("hello"));

        assert!(remove_bookmark(conn, "m1").expect("unsave"), "removed a row");
        assert!(!is_bookmarked(conn, "m1").unwrap(), "unsaved");
        assert!(list_bookmarks(conn).unwrap().is_empty(), "list is empty again");
    }

    #[test]
    fn toggle_returns_the_state_after_the_flip() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "m1", "conv1", "hello");

        assert!(toggle_bookmark(conn, "m1").unwrap(), "first toggle saves");
        assert!(!toggle_bookmark(conn, "m1").unwrap(), "second toggle unsaves");
        assert!(toggle_bookmark(conn, "m1").unwrap(), "third toggle saves again");
    }

    #[test]
    fn saving_twice_is_idempotent() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "m1", "conv1", "hello");

        add_bookmark(conn, "m1").expect("save");
        add_bookmark(conn, "m1").expect("save again");

        assert_eq!(list_bookmarks(conn).unwrap().len(), 1);
    }

    #[test]
    fn bookmark_conversation_is_derived_not_supplied() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "m1", "the-real-conversation", "hello");

        add_bookmark(conn, "m1").expect("save");

        let saved = list_bookmarks(conn).unwrap();
        assert_eq!(saved[0].conversation_id, "the-real-conversation");
    }

    #[test]
    fn cannot_save_a_message_this_device_does_not_hold() {
        let db = db();
        let conn = db.conn();

        add_bookmark(conn, "never-seen").expect_err("must refuse");
        assert!(list_bookmarks(conn).unwrap().is_empty());
    }

    #[test]
    fn bookmark_survives_message_eviction_as_unavailable() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "m1", "conv1", "hello");
        add_bookmark(conn, "m1").expect("save");

        conn.execute("DELETE FROM message WHERE id = 'm1'", [])
            .expect("evict");

        let saved = list_bookmarks(conn).expect("list");
        assert_eq!(saved.len(), 1, "the bookmark is still listed");
        assert!(!saved[0].available);
        assert_eq!(saved[0].content, None, "no body is invented");
        assert_eq!(saved[0].sender_id, None, "no sender is leaked");
        assert!(remove_bookmark(conn, "m1").unwrap(), "still removable");
    }

    #[test]
    fn soft_deleted_message_is_reported_unavailable() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "m1", "conv1", "hello");
        add_bookmark(conn, "m1").expect("save");
        conn.execute(
            "UPDATE message SET deleted_at = '2024-02-01T00:00:00Z', content = NULL WHERE id = 'm1'",
            [],
        )
        .expect("soft delete");

        let saved = list_bookmarks(conn).expect("list");
        assert_eq!(saved.len(), 1);
        assert!(!saved[0].available);
        assert_eq!(saved[0].content, None);
    }

    #[test]
    fn permalink_resolves_for_a_message_the_device_holds() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "m1", "conv1", "hello");

        let target = resolve_permalink(conn, "conv1", "m1").expect("resolve");
        assert!(target.found);
        assert_eq!(target.conversation_id.as_deref(), Some("conv1"));
        assert_eq!(target.message_id.as_deref(), Some("m1"));
    }

    /// A device never in the group never decrypted the message, so it was never
    /// written locally; this empty DB is that device.
    #[test]
    fn permalink_to_a_message_the_device_lacks_resolves_to_nothing() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "mine", "my-conv", "my own message");

        let target =
            resolve_permalink(conn, "secret-conv", "01HXSECRETMESSAGEID").expect("resolve");

        assert!(!target.found, "must not resolve");
        assert_eq!(target.conversation_id, None, "leaks no conversation id");
        assert_eq!(target.message_id, None, "leaks no message id");
        assert_eq!(
            target,
            PermalinkTarget::unresolved(),
            "identical to every other unresolvable answer"
        );
    }

    /// Unknown and inaccessible must look identical, or the resolver is an existence
    /// oracle.
    #[test]
    fn unresolvable_answers_are_indistinguishable() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "m1", "conv1", "hello");

        let mismatched = resolve_permalink(conn, "conv-i-am-not-in", "m1").unwrap();
        let fabricated = resolve_permalink(conn, "conv-i-am-not-in", "no-such-message").unwrap();
        let missing = resolve_permalink(conn, "conv1", "no-such-message").unwrap();

        assert_eq!(mismatched, PermalinkTarget::unresolved());
        assert_eq!(fabricated, PermalinkTarget::unresolved());
        assert_eq!(missing, PermalinkTarget::unresolved());
        assert_eq!(mismatched, fabricated);
        assert_eq!(mismatched, missing);
    }

    #[test]
    fn bookmarks_do_not_cross_devices() {
        let device_a = db();
        let device_b = db();
        seed_message(device_a.conn(), "m1", "conv1", "hello");
        seed_message(device_b.conn(), "m1", "conv1", "hello");

        add_bookmark(device_a.conn(), "m1").expect("save on A");

        assert!(is_bookmarked(device_a.conn(), "m1").unwrap());
        assert!(
            !is_bookmarked(device_b.conn(), "m1").unwrap(),
            "device B learns nothing about A's saved items"
        );
    }
}
