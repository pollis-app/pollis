//! Saved messages (bookmarks) and message permalinks (#854).
//!
//! # Where bookmarks live, and why
//!
//! Entirely in the **device-local, SQLCipher-encrypted** database (the
//! `bookmark` table in `db/local_schema.sql`). Nothing here talks to the DS or
//! to Turso, and no DS endpoint was added or extended for it.
//!
//! That is a deliberate privacy choice, not an omission. Which messages a user
//! saved is per-message *interest* metadata: a ranked list of the exact moments
//! and conversations a user cares most about. The server already sees envelope
//! routing metadata; a bookmark set would tell it strictly more, in the most
//! sensitive possible shape, and it would do so for a feature whose entire value
//! is a personal reading list. The cost of keeping it local is that bookmarks do
//! not follow a user to a new device — which is consistent with accepted loss
//! (2) in CLAUDE.md, since a new device starts with no history to bookmark.
//!
//! # Permalinks
//!
//! A permalink is `pollis://m/<conversation_id>/<message_id>` — two opaque ids.
//! It is a **pointer, not a capability**: it carries no key material and grants
//! no access, so forwarding one to a stranger is safe.
//!
//! [`resolve_permalink`] answers "do I hold this message?" **strictly against
//! the local database**. There is deliberately no server-side lookup, because a
//! "fetch message by id" endpoint is precisely what would convert this harmless
//! pointer into a leak.
//!
//! The underlying guarantee is cryptographic rather than a permission check: a
//! device that was not in the MLS group when the message was sent never received
//! the key material, so it never decrypted the envelope, so the message was never
//! written to its local `message` table. The lookup below misses for exactly that
//! reason — there is no code path in which it could succeed. That is accepted
//! loss (1) in CLAUDE.md working as intended.
//!
//! When the lookup misses, the caller gets a bare `found: false` and nothing
//! else. No sender, no timestamp, no conversation name, no existence claim — the
//! honest empty state must not hint at whether the message exists elsewhere.

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

/// One entry in the saved-items list.
///
/// The message body fields are `Option` because a bookmark deliberately outlives
/// its message: local retention eviction (or a soft delete) can remove the row
/// from `message` while the bookmark remains. A saved item whose message is gone
/// renders as an honest placeholder instead of silently vanishing from the list.
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
///
/// Intentionally minimal. On a miss every field but `found` is `None`, so the
/// response cannot be used to probe for the existence of a message this device
/// is not entitled to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermalinkTarget {
    pub found: bool,
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
}

impl PermalinkTarget {
    /// The single unresolvable answer. Every failure mode — unknown message,
    /// evicted message, message that belongs to a different conversation than
    /// the link claims, malformed input — funnels through here so they are
    /// externally indistinguishable.
    fn unresolved() -> Self {
        Self { found: false, conversation_id: None, message_id: None }
    }
}

// ─── Pure connection-level operations ────────────────────────────────────────
//
// Kept free of `AppState` so they unit-test against an in-memory `LocalDb`
// under `--no-default-features`, with no Turso, keystore or network in play.

/// Save a message.
///
/// The conversation is read from the local `message` row rather than taken from
/// the caller, so a bookmark cannot be created pointing at a conversation the
/// message is not in. Saving a message this device does not hold is refused for
/// the same reason: there would be nothing to anchor the row to.
///
/// Idempotent — `message_id` is the primary key, so "saved twice" is
/// unrepresentable rather than merely avoided.
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
/// Works even when the message body is long gone, which is the point: an
/// unavailable saved item must still be removable from the list.
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
/// A LEFT JOIN so bookmarks whose message has been evicted still come back, with
/// `available: false`. Soft-deleted messages are reported as unavailable too —
/// the body is gone, and the saved list should say so rather than render an
/// empty bubble.
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
/// Requires the message to exist locally **and** to be in the conversation the
/// link claims. A link whose two halves disagree is treated as unresolvable, so
/// a caller cannot pair a real message id with an arbitrary conversation id to
/// confirm which conversation the message is really in.
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

// ─── AppState wrappers (the command surface) ─────────────────────────────────

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

    /// Saving twice must not create a second row — the primary key makes the
    /// duplicate unrepresentable rather than merely unlikely.
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

    // ─── Permalink resolution ────────────────────────────────────────────────

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

    /// The security property. A device that was never in the group never got the
    /// key material, so the envelope never decrypted, so the message was never
    /// written locally — this empty database IS that device. Resolution must
    /// fail, and must hand back nothing at all.
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
