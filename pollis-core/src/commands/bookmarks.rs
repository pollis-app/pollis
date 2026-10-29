//! Saved messages (bookmarks) and message permalinks (#854).
//!
//! Device-local only (interest metadata the server must not see); permalinks resolve locally only.

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

/// One entry in the saved-items list.
///
/// Body fields are `Option` because a bookmark outlives its message (eviction or soft delete).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedMessage {
    pub message_id: String,
    pub conversation_id: String,
    /// When the user saved it (not when the message was sent).
    pub saved_at: String,
    /// True iff this device still holds the message body.
    pub available: bool,
    pub content: Option<String>,
    pub sender_id: Option<String>,
    pub sent_at: Option<String>,
}

/// Result of resolving a permalink against the local database.
/// On a miss every field but `found` is `None`, so it can't probe for existence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermalinkTarget {
    pub found: bool,
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
}

impl PermalinkTarget {
    /// The single unresolvable answer, so every failure mode is indistinguishable.
    fn unresolved() -> Self {
        Self { found: false, conversation_id: None, message_id: None }
    }
}

/// Save a message.
///
/// The conversation comes from the local `message` row, so a bookmark can't point elsewhere.
///
/// Idempotent: `message_id` is the primary key.
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

/// Unsave a message. Returns true if a bookmark was actually removed.
///
/// Works even when the message body is gone.
pub fn remove_bookmark(conn: &Connection, message_id: &str) -> Result<bool> {
    let removed = conn.execute(
        "DELETE FROM bookmark WHERE message_id = ?1",
        rusqlite::params![message_id],
    )?;
    Ok(removed > 0)
}

/// Is this message currently saved?
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

/// Flip the saved state. Returns the state *after* the toggle, so the caller
/// never has to re-read to know what to render.
pub fn toggle_bookmark(conn: &Connection, message_id: &str) -> Result<bool> {
    if remove_bookmark(conn, message_id)? {
        return Ok(false);
    }
    add_bookmark(conn, message_id)?;
    Ok(true)
}

/// Every saved message, newest save first, joined to the local message body.
///
/// LEFT JOIN so evicted or soft-deleted messages come back with `available: false`.
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
        // Present in `message` AND not soft-deleted.
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

/// Resolve a permalink against the local database only.
///
/// The message must be in the conversation the link claims, so a link can't confirm its real one.
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

/// The local DB is absent before sign-in. Bookmarks are meaningless then, and
/// this is the one place that has to say so.
///
/// `f` runs to completion with no `.await` inside, so the non-`Send` rusqlite
/// connection never crosses a suspension point and these commands stay `Send`.
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

    /// Saving twice must not create a second row.
    #[test]
    fn saving_twice_is_idempotent() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "m1", "conv1", "hello");

        add_bookmark(conn, "m1").expect("save");
        add_bookmark(conn, "m1").expect("save again");

        assert_eq!(list_bookmarks(conn).unwrap().len(), 1);
    }

    /// The bookmark's conversation comes from the message row, never the caller,
    /// so a bookmark pointing at the wrong conversation cannot be created.
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

    /// A bookmark outlives retention eviction, and degrades to an honest
    /// "unavailable" entry rather than disappearing or rendering a blank body.
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

    /// The security property: a device never in the group has no local row, so resolution yields nothing.
    #[test]
    fn permalink_to_a_message_the_device_lacks_resolves_to_nothing() {
        let db = db();
        let conn = db.conn();
        // Device holds an unrelated conversation; it was never in `secret-conv`.
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

    /// An unknown message and a known-but-inaccessible one must be
    /// indistinguishable, or the resolver becomes an existence oracle.
    #[test]
    fn unresolvable_answers_are_indistinguishable() {
        let db = db();
        let conn = db.conn();
        seed_message(conn, "m1", "conv1", "hello");

        // Real message id, wrong conversation.
        let mismatched = resolve_permalink(conn, "conv-i-am-not-in", "m1").unwrap();
        // Entirely fabricated ids.
        let fabricated = resolve_permalink(conn, "conv-i-am-not-in", "no-such-message").unwrap();
        // Real conversation, message this device never received.
        let missing = resolve_permalink(conn, "conv1", "no-such-message").unwrap();

        assert_eq!(mismatched, PermalinkTarget::unresolved());
        assert_eq!(fabricated, PermalinkTarget::unresolved());
        assert_eq!(missing, PermalinkTarget::unresolved());
        assert_eq!(mismatched, fabricated);
        assert_eq!(mismatched, missing);
    }

    /// Bookmarks are device-local, so a bookmark on one device's DB is simply
    /// absent from another's. Nothing syncs it into place.
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
