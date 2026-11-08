//! Changes that arrive before the message they change (#1735).
//!
//! An edit, a self-redaction or an admin tombstone can reach this device while
//! its target is absent (sent before this device joined — only a history
//! transfer can bring it) or a loss placeholder (#1312: no author, so the
//! author check cannot run). Dropping it, as ingest did before, meant a
//! deleted or edited message copied in later from a sibling came back in its
//! old state. So ingest RECORDS such a change ([`record`]) and every path that
//! creates or heals a real row — transfer import and ingest itself — runs
//! [`apply`] on it in the same transaction.
//!
//! The rules are ingest's own, applied late: a redaction or an edit counts only
//! when its MLS author is the row's author; a tombstone (DS-authorised) always
//! counts; a delete beats any edit; of several edits the latest (`seq`) wins.
//! Applied or discarded, the changes for that target are then gone.
//!
//! A change for a real row is never recorded: an author mismatch there is
//! dropped exactly as before, so ingest outcomes for present targets are
//! unchanged.

use rusqlite::OptionalExtension;

/// Most pending changes kept per conversation; edits go first, oldest first.
pub(crate) const MAX_PER_CONVERSATION: i64 = 5000;

/// Most bytes of pending edit text kept per conversation; the oldest edits go first.
///
/// Every device accumulates these (tombstones for messages it never fetched, edits of messages
/// sent before it joined), so the table is bounded in rows AND bytes. Deletes are tiny and never
/// evicted by this cap. The 400-day horizon stays (see [`prune`]): a backfill can come at any time
/// while a sibling still holds the target, and a change pruned before it arrives would let an
/// edited or deleted message come back as it was.
pub(crate) const MAX_EDIT_BYTES_PER_CONVERSATION: i64 = 4 * 1024 * 1024;

/// What a pending change does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// New text by `actor`.
    Edit { actor: String, content: String },
    /// `actor` redacted their own message.
    Redact { actor: String },
    /// The DS-authorised delete envelope (author or admin).
    Tombstone,
}

/// How a target stands for a change that just arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// A real row: apply now (or drop on an author mismatch), never record.
    Real,
    /// A loss placeholder: record.
    Placeholder,
    /// No row: record.
    Absent,
}

/// Classify `target_id` in `conversation_id`. A row with that id in ANOTHER
/// conversation is treated as real (ingest's checks then refuse the change),
/// so nothing is recorded against a row it could never apply to.
pub(crate) fn target(conn: &rusqlite::Connection, target_id: &str) -> Target {
    let row: Option<Option<String>> = conn
        .query_row(
            "SELECT undecryptable FROM message WHERE id = ?1",
            rusqlite::params![target_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    match row {
        None => Target::Absent,
        Some(Some(_)) => Target::Placeholder,
        Some(None) => Target::Real,
    }
}

/// A plausible target id: a ULID no later than the change itself (plus skew).
/// Bounds what a member can park here with ids that name nothing.
fn plausible_target(target_id: &str, change_id: &str) -> bool {
    let (Ok(t), Ok(c)) = (ulid::Ulid::from_string(target_id), ulid::Ulid::from_string(change_id)) else {
        return false;
    };
    t.timestamp_ms() <= c.timestamp_ms().saturating_add(10 * 60 * 1000)
}

/// Record a change whose target is absent or a placeholder. Idempotent per
/// envelope; keeps at most [`MAX_PER_CONVERSATION`] per conversation.
/// Best-effort like the rest of ingest: a failure is logged, never fatal.
pub(crate) fn record(
    conn: &rusqlite::Connection,
    envelope_id: &str,
    target_id: &str,
    conversation_id: &str,
    change: &Change,
    seq: i64,
) {
    if !plausible_target(target_id, envelope_id) {
        return;
    }
    // An edit no real message could carry is not kept (bounds the table's bytes).
    if let Change::Edit { content, .. } = change {
        if content.len() > crate::commands::history_transfer::wire::MAX_CONTENT_BYTES {
            return;
        }
    }
    let (kind, actor, content) = match change {
        Change::Edit { actor, content } => ("edit", Some(actor.as_str()), Some(content.as_str())),
        Change::Redact { actor } => ("redact", Some(actor.as_str()), None),
        Change::Tombstone => ("tombstone", None, None),
    };
    if let Err(e) = conn.execute(
        "INSERT OR IGNORE INTO pending_message_change
         (envelope_id, target_id, conversation_id, kind, actor_id, content, seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![envelope_id, target_id, conversation_id, kind, actor, content, seq],
    ) {
        crate::diag!("[pending-change] could not record {kind} {envelope_id}: {e}");
        return;
    }
    // Only the latest edit by an author can ever apply: keep one per
    // (target, author), so edits cannot pile up.
    if let Change::Edit { actor, .. } = change {
        let _ = conn.execute(
            "DELETE FROM pending_message_change
             WHERE target_id = ?1 AND conversation_id = ?2 AND kind = 'edit' AND actor_id = ?3
               AND envelope_id != (
                   SELECT envelope_id FROM pending_message_change
                   WHERE target_id = ?1 AND conversation_id = ?2 AND kind = 'edit' AND actor_id = ?3
                   ORDER BY seq DESC, envelope_id DESC LIMIT 1)",
            rusqlite::params![target_id, conversation_id, actor],
        );
    }
    // The caps evict EDITS first, oldest first: flooding edits can never push
    // out a delete (which would let a deleted message come back). Each eviction
    // runs only when its cap is actually exceeded.
    let (count, edit_bytes): (i64, i64) = conn
        .prepare_cached(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN kind = 'edit' THEN length(CAST(content AS BLOB)) END), 0)
             FROM pending_message_change WHERE conversation_id = ?1",
        )
        .and_then(|mut st| st.query_row(rusqlite::params![conversation_id], |r| Ok((r.get(0)?, r.get(1)?))))
        .unwrap_or((0, 0));
    if count > MAX_PER_CONVERSATION {
        let _ = conn.execute(
            "DELETE FROM pending_message_change WHERE conversation_id = ?1 AND envelope_id NOT IN (
                 SELECT envelope_id FROM pending_message_change WHERE conversation_id = ?1
                 ORDER BY (kind = 'edit'), recorded_at DESC, seq DESC, envelope_id DESC LIMIT ?2)",
            rusqlite::params![conversation_id, MAX_PER_CONVERSATION],
        );
    }
    if edit_bytes > MAX_EDIT_BYTES_PER_CONVERSATION {
        let _ = conn.execute(
            "DELETE FROM pending_message_change WHERE envelope_id IN (
                 SELECT envelope_id FROM (
                     SELECT envelope_id,
                            SUM(length(CAST(content AS BLOB))) OVER (
                                ORDER BY recorded_at DESC, seq DESC, envelope_id DESC
                                ROWS UNBOUNDED PRECEDING) AS kept
                     FROM pending_message_change WHERE conversation_id = ?1 AND kind = 'edit')
                 WHERE kept > ?2)",
            rusqlite::params![conversation_id, MAX_EDIT_BYTES_PER_CONVERSATION],
        );
    }
}

/// What [`apply`] did to the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Applied {
    /// No pending change applied (none, or none by the row's author).
    Nothing,
    Edited,
    Deleted,
}

/// Apply and clear the pending changes for `message_id`, now a real row in
/// `conversation_id`. Run inside the caller's transaction/savepoint.
pub(crate) fn apply(
    conn: &rusqlite::Connection,
    message_id: &str,
    conversation_id: &str,
) -> rusqlite::Result<Applied> {
    let row: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT sender_id, deleted_at, undecryptable FROM message
             WHERE id = ?1 AND conversation_id = ?2",
            rusqlite::params![message_id, conversation_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((author, deleted_at, undecryptable)) = row else {
        return Ok(Applied::Nothing);
    };
    // Still a placeholder: its author is unknown, keep waiting.
    if undecryptable.is_some() {
        return Ok(Applied::Nothing);
    }
    let mut stmt = conn.prepare_cached(
        "SELECT kind, actor_id, content, recorded_at FROM pending_message_change
         WHERE target_id = ?1 AND conversation_id = ?2
         ORDER BY seq DESC, envelope_id DESC",
    )?;
    let changes: Vec<(String, Option<String>, Option<String>, String)> = stmt
        .query_map(rusqlite::params![message_id, conversation_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if changes.is_empty() {
        return Ok(Applied::Nothing);
    }
    let by_author = |actor: &Option<String>| actor.as_deref() == Some(author.as_str());
    let delete = changes
        .iter()
        .any(|(kind, actor, _, _)| kind == "tombstone" || (kind == "redact" && by_author(actor)));
    let latest_edit = changes
        .iter()
        .find(|(kind, actor, _, _)| kind == "edit" && by_author(actor))
        .and_then(|(_, _, content, at)| content.clone().map(|c| (c, at.clone())));
    let now = chrono::Utc::now().to_rfc3339();
    let mut applied = Applied::Nothing;
    if delete {
        if deleted_at.is_none() {
            conn.execute(super::outbox::SOFT_DELETE_SQL, rusqlite::params![now, message_id])?;
        }
        applied = Applied::Deleted;
    } else if let Some((text, recorded_at)) = latest_edit {
        // The edit's own time — when this device received it — as ingest would
        // have stamped it then, not the moment its target finally arrived.
        let edited_at = chrono::NaiveDateTime::parse_from_str(&recorded_at, "%Y-%m-%d %H:%M:%S")
            .map(|t| t.and_utc().to_rfc3339())
            .unwrap_or_else(|_| now.clone());
        let n = conn.execute(
            "UPDATE message SET content = ?1, edited_at = ?2 WHERE id = ?3 AND deleted_at IS NULL",
            rusqlite::params![text, edited_at, message_id],
        )?;
        if n > 0 {
            applied = Applied::Edited;
        }
    }
    conn.execute(
        "DELETE FROM pending_message_change WHERE target_id = ?1 AND conversation_id = ?2",
        rusqlite::params![message_id, conversation_id],
    )?;
    Ok(applied)
}

/// Drop changes older than any envelope the DS still holds (same horizon as
/// `known_envelope`).
pub(crate) fn prune(conn: &rusqlite::Connection) {
    if let Err(e) = conn.execute(
        "DELETE FROM pending_message_change WHERE recorded_at < datetime('now', '-400 days')",
        [],
    ) {
        crate::diag!("[pending-change] prune failed (non-fatal): {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::local::apply_local_schema(&conn).unwrap();
        conn
    }

    fn ulid_at(ms: u64) -> String {
        ulid::Ulid::from_parts(ms, rand::random()).to_string()
    }

    fn insert_real(conn: &rusqlite::Connection, id: &str, conv: &str, sender: &str, content: &str) {
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES (?1, ?2, ?3, X'', ?4, '2026-01-01T00:00:00Z')",
            rusqlite::params![id, conv, sender, content],
        )
        .unwrap();
    }

    fn content(conn: &rusqlite::Connection, id: &str) -> (Option<String>, Option<String>) {
        conn.query_row(
            "SELECT content, deleted_at FROM message WHERE id = ?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    fn pending(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM pending_message_change", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn the_latest_edit_by_the_author_wins_and_a_strangers_edit_is_ignored() {
        let conn = db();
        let target = ulid_at(1_000);
        record(&conn, &ulid_at(2_000), &target, "c", &Change::Edit { actor: "bob".into(), content: "v2".into() }, 2);
        record(&conn, &ulid_at(3_000), &target, "c", &Change::Edit { actor: "bob".into(), content: "v3".into() }, 3);
        record(&conn, &ulid_at(4_000), &target, "c", &Change::Edit { actor: "eve".into(), content: "evil".into() }, 4);
        insert_real(&conn, &target, "c", "bob", "v1");
        assert_eq!(apply(&conn, &target, "c").unwrap(), Applied::Edited);
        assert_eq!(content(&conn, &target).0.as_deref(), Some("v3"));
        assert_eq!(pending(&conn), 0);
    }

    #[test]
    fn a_delete_beats_any_edit_and_a_strangers_redaction_does_not_delete() {
        let conn = db();
        let a = ulid_at(1_000);
        record(&conn, &ulid_at(2_000), &a, "c", &Change::Edit { actor: "bob".into(), content: "v2".into() }, 9);
        record(&conn, &ulid_at(3_000), &a, "c", &Change::Redact { actor: "bob".into() }, 1);
        insert_real(&conn, &a, "c", "bob", "v1");
        assert_eq!(apply(&conn, &a, "c").unwrap(), Applied::Deleted);
        let (c, d) = content(&conn, &a);
        assert!(c.is_none() && d.is_some());

        let b = ulid_at(1_000);
        record(&conn, &ulid_at(2_000), &b, "c", &Change::Redact { actor: "eve".into() }, 1);
        insert_real(&conn, &b, "c", "bob", "keep");
        assert_eq!(apply(&conn, &b, "c").unwrap(), Applied::Nothing);
        assert_eq!(content(&conn, &b).0.as_deref(), Some("keep"));
        assert_eq!(pending(&conn), 0, "a change that cannot apply is discarded, not kept");

        let t = ulid_at(1_000);
        record(&conn, &ulid_at(2_000), &t, "c", &Change::Tombstone, 1);
        insert_real(&conn, &t, "c", "bob", "x");
        assert_eq!(apply(&conn, &t, "c").unwrap(), Applied::Deleted);
        assert!(content(&conn, &t).0.is_none());
    }

    #[test]
    fn a_change_never_crosses_conversations() {
        let conn = db();
        let a = ulid_at(1_000);
        record(&conn, &ulid_at(2_000), &a, "other", &Change::Tombstone, 1);
        insert_real(&conn, &a, "c", "bob", "x");
        assert_eq!(apply(&conn, &a, "c").unwrap(), Applied::Nothing);
        assert_eq!(content(&conn, &a).0.as_deref(), Some("x"));
        assert_eq!(pending(&conn), 1, "the other conversation's change stays");
    }

    #[test]
    fn a_placeholder_keeps_its_changes_until_it_is_healed() {
        let conn = db();
        let a = ulid_at(1_000);
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, undecryptable)
             VALUES (?1, 'c', 'sealed', X'', NULL, 's', 'failed')",
            rusqlite::params![a],
        )
        .unwrap();
        assert_eq!(target(&conn, &a), Target::Placeholder);
        record(&conn, &ulid_at(2_000), &a, "c", &Change::Redact { actor: "bob".into() }, 1);
        assert_eq!(apply(&conn, &a, "c").unwrap(), Applied::Nothing);
        assert_eq!(pending(&conn), 1);
    }

    #[test]
    fn implausible_targets_are_not_recorded_and_the_table_is_capped() {
        let conn = db();
        record(&conn, &ulid_at(2_000), "not-a-ulid", "c", &Change::Tombstone, 1);
        // A target minted far after the change that names it.
        record(&conn, &ulid_at(2_000), &ulid_at(2_000 + 3_600_000), "c", &Change::Tombstone, 1);
        assert_eq!(pending(&conn), 0);
        for i in 0..(MAX_PER_CONVERSATION + 10) {
            record(&conn, &ulid_at(5_000 + i as u64), &ulid_at(1_000), "c", &Change::Tombstone, i);
        }
        assert_eq!(pending(&conn), MAX_PER_CONVERSATION);
    }

    #[test]
    fn a_flood_of_edits_never_evicts_a_delete_and_keeps_one_edit_per_author() {
        let conn = db();
        let target = ulid_at(1_000);
        record(&conn, &ulid_at(2_000), &target, "c", &Change::Tombstone, 1);
        let other = ulid_at(1_500);
        record(&conn, &ulid_at(2_500), &other, "c", &Change::Redact { actor: "bob".into() }, 2);
        // Thousands of edits from many authors on many targets.
        for i in 0..(MAX_PER_CONVERSATION + 50) {
            let t = ulid_at(3_000 + i as u64);
            record(&conn, &ulid_at(4_000 + i as u64), &t, "c", &Change::Edit { actor: format!("eve{i}"), content: "x".into() }, 10 + i);
        }
        assert_eq!(pending(&conn), MAX_PER_CONVERSATION);
        let deletes: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_message_change WHERE kind != 'edit'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(deletes, 2, "the tombstone and the redaction survive the flood");
        // Many edits by one author on one target collapse to the latest.
        let t = ulid_at(9_000);
        for i in 0..20 {
            record(&conn, &ulid_at(9_100 + i), &t, "d", &Change::Edit { actor: "bob".into(), content: format!("v{i}") }, 100 + i as i64);
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_message_change WHERE target_id = ?1", [&t], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // An oversize edit is not kept.
        record(&conn, &ulid_at(9_500), &ulid_at(9_400), "e", &Change::Edit { actor: "bob".into(), content: "x".repeat(300 * 1024) }, 1);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM pending_message_change WHERE conversation_id = 'e'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn edit_bytes_are_capped_per_conversation_and_deletes_survive() {
        let conn = db();
        record(&conn, &ulid_at(1_100), &ulid_at(1_000), "c", &Change::Tombstone, 1);
        let big = "x".repeat(200 * 1024);
        for i in 0..40u64 {
            record(&conn, &ulid_at(3_000 + i), &ulid_at(2_000 + i), "c", &Change::Edit { actor: format!("a{i}"), content: big.clone() }, 10 + i as i64);
        }
        let (n, bytes, tombs): (i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(length(content)), 0), SUM(kind = 'tombstone') FROM pending_message_change WHERE conversation_id = 'c'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(bytes <= MAX_EDIT_BYTES_PER_CONVERSATION, "{bytes}");
        assert_eq!(tombs, 1, "the delete survives the byte cap");
        assert_eq!(n, 1 + MAX_EDIT_BYTES_PER_CONVERSATION / (200 * 1024));
    }

    #[test]
    fn the_byte_cap_counts_bytes_not_characters() {
        let conn = db();
        // 4-byte characters: 64 K chars = 256 KiB.
        let wide = "😀".repeat(64 * 1024);
        for i in 0..40u64 {
            record(&conn, &ulid_at(3_000 + i), &ulid_at(2_000 + i), "w", &Change::Edit { actor: format!("a{i}"), content: wide.clone() }, 10 + i as i64);
        }
        let bytes: i64 = conn
            .query_row("SELECT COALESCE(SUM(length(CAST(content AS BLOB))), 0) FROM pending_message_change WHERE conversation_id = 'w'", [], |r| r.get(0))
            .unwrap();
        assert!(bytes <= MAX_EDIT_BYTES_PER_CONVERSATION, "{bytes}");
        assert!(bytes > 0);
    }

    #[test]
    fn an_applied_edit_carries_the_time_it_was_received() {
        let conn = db();
        let target = ulid_at(1_000);
        record(&conn, &ulid_at(2_000), &target, "c", &Change::Edit { actor: "bob".into(), content: "v2".into() }, 2);
        conn.execute("UPDATE pending_message_change SET recorded_at = '2020-01-02 03:04:05'", []).unwrap();
        insert_real(&conn, &target, "c", "bob", "v1");
        apply(&conn, &target, "c").unwrap();
        let at: String = conn.query_row("SELECT edited_at FROM message WHERE id = ?1", [&target], |r| r.get(0)).unwrap();
        assert!(at.starts_with("2020-01-02T03:04:05"), "{at}");
    }

    #[test]
    fn the_schema_refuses_malformed_changes() {
        let conn = db();
        let ins = |kind: &str, actor: Option<&str>, content: Option<&str>| {
            conn.execute(
                "INSERT INTO pending_message_change (envelope_id, target_id, conversation_id, kind, actor_id, content)
                 VALUES (?1, 't', 'c', ?2, ?3, ?4)",
                rusqlite::params![ulid::Ulid::new().to_string(), kind, actor, content],
            )
        };
        assert!(ins("edit", Some("a"), None).is_err(), "an edit without text");
        assert!(ins("redact", Some("a"), Some("x")).is_err(), "a redaction with text");
        assert!(ins("redact", None, None).is_err(), "a redaction without an author");
        assert!(ins("tombstone", Some("a"), None).is_err(), "a tombstone claiming an author");
        assert!(ins("rewrite", Some("a"), None).is_err(), "an unknown kind");
        assert!(ins("edit", Some("a"), Some("x")).is_ok());
    }
}
