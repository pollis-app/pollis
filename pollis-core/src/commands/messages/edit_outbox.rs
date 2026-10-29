//! The durable outbox for edits (#1555).
//!
//! An edit is stored in `outbox_edit` first; the `message` row changes only once the DS holds the envelope.
//!
//! The same drain as unsent messages redelivers it; one edit per target.
//!
//! **Idempotency.** Every attempt posts under the same envelope id; confirming is conditional on that id.

use std::sync::Arc;

use rusqlite::OptionalExtension;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::outbox::{is_permanent, OutboxLock, FAILED, PENDING, REJECTED};
use super::types::ChannelMessage;

/// An edit as the outbox stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueuedEdit {
    pub target_message_id: String,
    pub envelope_id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub content: String,
}

fn not_signed_in() -> Error {
    Error::Other(anyhow::anyhow!("Not signed in"))
}

/// Store an edit — the first thing `edit_message` does, before any network.
/// The same text keeps the envelope id and re-arms a failed edit; new text is a new edit.
pub(super) fn enqueue_edit_on(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    target_message_id: &str,
    sender_id: &str,
    content: &str,
) -> Result<QueuedEdit> {
    let target: Option<(String, String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT conversation_id, sender_id, deleted_at, send_state FROM message WHERE id = ?1",
            rusqlite::params![target_message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((conv, author, deleted_at, send_state)) = target else {
        return Err(not_editable());
    };
    if author != sender_id || deleted_at.is_some() || conv != conversation_id {
        return Err(not_editable());
    }
    // An unsent message (#1335) has no envelope for an edit to target.
    if send_state.is_some() {
        return Err(Error::Other(anyhow::anyhow!("message has not been sent yet")));
    }
    let existing = load_edit_on(conn, target_message_id)?;
    let envelope_id = match existing {
        Some((e, _)) if e.content == content => e.envelope_id,
        _ => Ulid::new().to_string(),
    };
    conn.execute(
        "INSERT INTO outbox_edit (target_message_id, envelope_id, conversation_id, sender_id, content, send_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(target_message_id) DO UPDATE SET
           envelope_id = excluded.envelope_id,
           content = excluded.content,
           send_state = excluded.send_state,
           created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        rusqlite::params![target_message_id, envelope_id, conversation_id, sender_id, content, PENDING],
    )?;
    Ok(QueuedEdit {
        target_message_id: target_message_id.to_string(),
        envelope_id,
        conversation_id: conversation_id.to_string(),
        sender_id: sender_id.to_string(),
        content: content.to_string(),
    })
}

fn not_editable() -> Error {
    Error::Other(anyhow::anyhow!(
        "Message not found, already deleted, or you are not the sender"
    ))
}

/// The stored edit of `target`, with its state.
pub(super) fn load_edit_on(
    conn: &rusqlite::Connection,
    target: &str,
) -> Result<Option<(QueuedEdit, String)>> {
    Ok(conn
        .query_row(
            "SELECT target_message_id, envelope_id, conversation_id, sender_id, content, send_state
             FROM outbox_edit WHERE target_message_id = ?1",
            rusqlite::params![target],
            |row| {
                Ok((
                    QueuedEdit {
                        target_message_id: row.get(0)?,
                        envelope_id: row.get(1)?,
                        conversation_id: row.get(2)?,
                        sender_id: row.get(3)?,
                        content: row.get(4)?,
                    },
                    row.get(5)?,
                ))
            },
        )
        .optional()?)
}

/// Drop the outbox entry and write the text in one transaction; a newer edit stays queued.
pub(super) fn confirm_edit_on(
    conn: &rusqlite::Connection,
    edit: &QueuedEdit,
    edited_at: &str,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let ours = tx.execute(
        "DELETE FROM outbox_edit WHERE target_message_id = ?1 AND envelope_id = ?2",
        rusqlite::params![edit.target_message_id, edit.envelope_id],
    )?;
    // Superseded by a newer edit while it was in flight: that one's text is
    // what the row will hold, once it is confirmed in turn.
    if ours > 0 {
        tx.execute(
            "UPDATE message SET content = ?1, edited_at = ?2
             WHERE id = ?3 AND sender_id = ?4 AND deleted_at IS NULL",
            rusqlite::params![edit.content, edited_at, edit.target_message_id, edit.sender_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Record a failed attempt at the edit `envelope_id` (not at a newer one).
pub(super) fn mark_edit_state_on(
    conn: &rusqlite::Connection,
    target: &str,
    envelope_id: &str,
    state: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE outbox_edit SET send_state = ?1 WHERE target_message_id = ?2 AND envelope_id = ?3",
        rusqlite::params![state, target, envelope_id],
    )?;
    Ok(())
}

/// Forget the pending edit of a deleted `target`.
pub(crate) fn drop_edit_on(conn: &rusqlite::Connection, target: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM outbox_edit WHERE target_message_id = ?1",
        rusqlite::params![target],
    )?;
    Ok(())
}

/// The targets of every edit the drain should try, oldest first. A `rejected`
/// one is not among them.
pub(super) fn pending_edit_targets_on(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT target_message_id FROM outbox_edit
         WHERE send_state IN ('pending', 'failed') ORDER BY created_at, target_message_id",
    )?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids)
}

/// Show each row's unconfirmed edit over it, with `edit_state`; a deleted row keeps its tombstone.
pub(crate) fn overlay_pending_edits(
    conn: &rusqlite::Connection,
    messages: &mut [ChannelMessage],
) -> Result<()> {
    if messages.is_empty() {
        return Ok(());
    }
    let mut stmt =
        conn.prepare("SELECT target_message_id, content, send_state, created_at FROM outbox_edit")?;
    let pending: std::collections::HashMap<String, (String, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?))))?
        .collect::<std::result::Result<_, _>>()?;
    if pending.is_empty() {
        return Ok(());
    }
    for m in messages.iter_mut() {
        if m.deleted_at.is_some() || m.undecryptable {
            continue;
        }
        if let Some((content, state, at)) = pending.get(&m.id) {
            m.content = Some(content.clone());
            m.edited_at = Some(at.clone());
            m.edit_state = Some(state.clone());
        }
    }
    Ok(())
}

/// Whether `target` has an edit the outbox still holds.
pub(super) async fn has_pending_edit(state: &Arc<AppState>, target: &str) -> Result<bool> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_signed_in)?;
    Ok(load_edit_on(db.conn(), target)?.is_some())
}

/// Run one delivery attempt for the stored edit of `target`.
/// Re-reads the entry under the target's outbox lock; a failed attempt marks it `failed` or `rejected`.
pub(super) async fn deliver_edit(state: &Arc<AppState>, target: &str) -> Result<()> {
    let lock = OutboxLock::acquire(state, &format!("edit:{target}")).await;
    let result = deliver_edit_locked(state, target).await;
    lock.release().await;
    result
}

async fn deliver_edit_locked(state: &Arc<AppState>, target: &str) -> Result<()> {
    let edit = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(not_signed_in)?;
        let Some((edit, _)) = load_edit_on(db.conn(), target)? else {
            return Ok(());
        };
        let live: Option<Option<String>> = db
            .conn()
            .query_row(
                "SELECT deleted_at FROM message WHERE id = ?1",
                rusqlite::params![target],
                |row| row.get(0),
            )
            .optional()?;
        if !matches!(live, Some(None)) {
            drop_edit_on(db.conn(), target)?;
            return Ok(());
        }
        edit
    };

    match super::edit_delete::post_edit(state, &edit).await {
        Ok(()) => Ok(()),
        Err(e) => {
            if !super::send::test_hooks::is_simulated_crash(&e) {
                let mark = if is_permanent(&e) { REJECTED } else { FAILED };
                let guard = state.local_db.lock().await;
                if let Some(db) = guard.as_ref() {
                    if let Err(m) = mark_edit_state_on(db.conn(), target, &edit.envelope_id, mark) {
                        crate::diag!("[outbox] could not mark the edit of {target} {mark}: {m}");
                    }
                }
            }
            Err(e)
        }
    }
}

/// Every unconfirmed edit the drain should try, as `(target, outcome)`.
pub(super) async fn drain_edits(state: &Arc<AppState>) -> Result<(Vec<String>, Vec<String>)> {
    let targets = {
        let guard = state.local_db.lock().await;
        match guard.as_ref() {
            Some(db) => pending_edit_targets_on(db.conn())?,
            None => return Ok((Vec::new(), Vec::new())),
        }
    };
    let (mut delivered, mut failed) = (Vec::new(), Vec::new());
    for target in targets {
        match deliver_edit(state, &target).await {
            Ok(()) => delivered.push(target),
            Err(e) => {
                crate::diag!("[outbox] delivery of the edit of {target} failed: {e}");
                failed.push(target);
            }
        }
    }
    Ok((delivered, failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::local::LocalDb;

    fn delivered_row(conn: &rusqlite::Connection, id: &str, content: &str) {
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES (?1, 'c1', 'u1', X'01', ?2, '2026-10-01T00:00:00.000Z')",
            rusqlite::params![id, content],
        )
        .unwrap();
    }

    fn row_content(conn: &rusqlite::Connection, id: &str) -> Option<String> {
        conn.query_row("SELECT content FROM message WHERE id = ?1", [id], |r| r.get(0))
            .unwrap()
    }

    fn page_row(id: &str, content: &str) -> ChannelMessage {
        ChannelMessage {
            id: id.to_string(),
            conversation_id: "c1".to_string(),
            sender_id: "u1".to_string(),
            sender_username: None,
            ciphertext: String::new(),
            content: Some(content.to_string()),
            reply_to_id: None,
            thread_id: None,
            sent_at: "2026-10-01T00:00:00.000Z".to_string(),
            edited_at: None,
            deleted_at: None,
            undecryptable: false,
            send_state: None,
            edit_state: None,
        }
    }

    /// Storing an edit leaves the message row untouched.
    #[test]
    fn storing_an_edit_does_not_touch_the_row() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        delivered_row(conn, "m1", "draft");
        let e = enqueue_edit_on(conn, "c1", "m1", "u1", "final").unwrap();
        assert_eq!(row_content(conn, "m1").as_deref(), Some("draft"));
        assert_eq!(pending_edit_targets_on(conn).unwrap(), vec!["m1".to_string()]);
        let (stored, st) = load_edit_on(conn, "m1").unwrap().unwrap();
        assert_eq!(stored, e);
        assert_eq!(st, PENDING);

        confirm_edit_on(conn, &e, "2026-10-02T00:00:00.000Z").unwrap();
        assert_eq!(row_content(conn, "m1").as_deref(), Some("final"));
        assert!(load_edit_on(conn, "m1").unwrap().is_none());
    }

    /// Same text keeps the envelope id; a late confirmation of an older edit changes nothing.
    #[test]
    fn a_newer_edit_replaces_an_older_and_survives_its_confirmation() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        delivered_row(conn, "m1", "draft");
        let first = enqueue_edit_on(conn, "c1", "m1", "u1", "one").unwrap();
        mark_edit_state_on(conn, "m1", &first.envelope_id, FAILED).unwrap();
        let again = enqueue_edit_on(conn, "c1", "m1", "u1", "one").unwrap();
        assert_eq!(again.envelope_id, first.envelope_id);
        assert_eq!(load_edit_on(conn, "m1").unwrap().unwrap().1, PENDING);

        let second = enqueue_edit_on(conn, "c1", "m1", "u1", "two").unwrap();
        assert_ne!(second.envelope_id, first.envelope_id);
        // A stale attempt cannot mark the newer edit failed either.
        mark_edit_state_on(conn, "m1", &first.envelope_id, REJECTED).unwrap();
        assert_eq!(load_edit_on(conn, "m1").unwrap().unwrap().1, PENDING);

        confirm_edit_on(conn, &first, "2026-10-02T00:00:00.000Z").unwrap();
        assert_eq!(row_content(conn, "m1").as_deref(), Some("draft"));
        assert_eq!(load_edit_on(conn, "m1").unwrap().unwrap().0, second);
        confirm_edit_on(conn, &second, "2026-10-02T00:00:01.000Z").unwrap();
        assert_eq!(row_content(conn, "m1").as_deref(), Some("two"));
    }

    /// Only the sender's own delivered, undeleted message can be edited.
    #[test]
    fn only_an_own_delivered_message_takes_an_edit() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        delivered_row(conn, "m1", "x");
        assert!(enqueue_edit_on(conn, "c1", "m1", "u2", "y").is_err());
        assert!(enqueue_edit_on(conn, "c2", "m1", "u1", "y").is_err());
        assert!(enqueue_edit_on(conn, "c1", "nope", "u1", "y").is_err());
        conn.execute("UPDATE message SET send_state = 'failed' WHERE id = 'm1'", []).unwrap();
        assert!(enqueue_edit_on(conn, "c1", "m1", "u1", "y").is_err());
        conn.execute(
            "UPDATE message SET send_state = NULL, content = NULL, deleted_at = 't' WHERE id = 'm1'",
            [],
        )
        .unwrap();
        assert!(enqueue_edit_on(conn, "c1", "m1", "u1", "y").is_err());
        assert!(pending_edit_targets_on(conn).unwrap().is_empty());
    }

    /// Reads show the pending text with its state; a rejected edit is shown
    /// but not drained again.
    #[test]
    fn reads_show_the_pending_edit_and_its_state() {
        let db = LocalDb::open_in_memory().unwrap();
        let conn = db.conn();
        delivered_row(conn, "m1", "draft");
        delivered_row(conn, "m2", "other");
        let e = enqueue_edit_on(conn, "c1", "m1", "u1", "final").unwrap();
        let mut rows = vec![page_row("m1", "draft"), page_row("m2", "other")];
        overlay_pending_edits(conn, &mut rows).unwrap();
        assert_eq!(rows[0].content.as_deref(), Some("final"));
        assert_eq!(rows[0].edit_state.as_deref(), Some(PENDING));
        assert!(rows[0].edited_at.is_some());
        assert_eq!(rows[1].content.as_deref(), Some("other"));
        assert_eq!(rows[1].edit_state, None);

        mark_edit_state_on(conn, "m1", &e.envelope_id, REJECTED).unwrap();
        assert!(pending_edit_targets_on(conn).unwrap().is_empty());
        let mut rows = vec![page_row("m1", "draft")];
        overlay_pending_edits(conn, &mut rows).unwrap();
        assert_eq!(rows[0].edit_state.as_deref(), Some(REJECTED));

        drop_edit_on(conn, "m1").unwrap();
        let mut rows = vec![page_row("m1", "draft")];
        overlay_pending_edits(conn, &mut rows).unwrap();
        assert_eq!(rows[0].content.as_deref(), Some("draft"));
    }
}
