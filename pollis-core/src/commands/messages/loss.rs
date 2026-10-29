//! Making a lost message visible (#1312).
//!
//! Under `max_past_epochs = 0` an envelope not decrypted while the group sat at
//! its epoch never can be, and the cursor must still move past it. When it
//! moves past a message envelope this device was entitled to read, a
//! placeholder row is written (`message.undecryptable` = the [`Reason`]).
//!
//! "Entitled" excludes exactly two cases:
//!
//! - Sealed below the epoch this device joined the lineage at
//!   ([`crate::commands::mls::first_join`]): the accepted pre-join loss. A
//!   lineage joined before #1312 gets its floor from [`fallback_floor`].
//! - Envelopes already accounted for: this device's own posts (MLS cannot
//!   decrypt for the sealing leaf), and frames it opened without keeping a row.
//!   [`record_own_envelope`] and [`record_opened_envelope`] keep that list.
//!
//! Nothing outside the ciphertext says whether an envelope held text or a
//! control frame, so a placeholder claims only that something sent here could
//! not be read; a lost edit or delete is logged, not shown.

use rusqlite::OptionalExtension;

/// Why a placeholder was written. Stored in `message.undecryptable`, whose
/// CHECK constraint admits exactly these strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    /// The group had moved past the envelope's epoch before it was fetched, so
    /// it was never attempted.
    Stranded,
    /// Attempted at its own epoch and the decrypt failed.
    Failed,
    /// Sealed in a lineage (#454 generation) this device no longer holds.
    Lineage,
}

impl Reason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Reason::Stranded => "stranded",
            Reason::Failed => "failed",
            Reason::Lineage => "lineage",
        }
    }
}

/// Decide whether an envelope the cursor is about to pass is a loss.
///
/// Pure, so the matrix is tested without a database. `first_join` is the epoch
/// at which this device first held `sealed_at`'s generation, if ever;
/// `attempted` means the replay reached `sealed_at` and the decrypt failed.
/// `None` means "not a loss this device was entitled to avoid".
pub(crate) fn classify(
    sealed_at: (i64, u64),
    lineage: i64,
    first_join: Option<u64>,
    attempted: bool,
) -> Option<Reason> {
    let (generation, epoch) = sealed_at;
    // Never held that lineage, or sealed before this device joined it.
    let floor = first_join?;
    if epoch < floor {
        return None;
    }
    if generation != lineage {
        return Some(Reason::Lineage);
    }
    if attempted {
        Some(Reason::Failed)
    } else {
        Some(Reason::Stranded)
    }
}

/// The floor for a lineage joined before #1312: one above the epoch the first
/// catch-up started at, since the device may have posted control frames there
/// before own envelopes were recorded. A real loss at that one epoch stays
/// silent.
pub(crate) fn fallback_floor(start_epoch: u64) -> u64 {
    start_epoch.saturating_add(1)
}

/// Remember that this device posted envelope `id`. Called before the post, so a
/// crash cannot leave an own envelope unrecorded.
pub(crate) fn record_own_envelope(conn: &rusqlite::Connection, id: &str) {
    record_known(conn, id, "own");
}

/// Remember that this device opened envelope `id` and kept no row for it (a
/// control frame or an edit). A second fetch of it cannot decrypt again.
pub(crate) fn record_opened_envelope(conn: &rusqlite::Connection, id: &str) {
    record_known(conn, id, "opened");
}

fn record_known(conn: &rusqlite::Connection, id: &str, kind: &str) {
    if let Err(e) = conn.execute(
        "INSERT OR IGNORE INTO known_envelope (id, kind) VALUES (?1, ?2)",
        rusqlite::params![id, kind],
    ) {
        crate::diag!("[loss] could not record {kind} envelope {id}: {e}");
    }
}

/// Has this device already accounted for envelope `id` — posted it, or opened
/// it without keeping a row?
pub(crate) fn is_known_envelope(conn: &rusqlite::Connection, id: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM known_envelope WHERE id = ?1",
        rusqlite::params![id],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

/// Drop records older than any envelope the DS still holds
/// (`POLLIS_DS_WATERMARK_STALE_MONTHS` = 12, plus a margin).
pub(crate) fn prune_known_envelopes(conn: &rusqlite::Connection) {
    if let Err(e) = conn.execute(
        "DELETE FROM known_envelope WHERE recorded_at < datetime('now', '-400 days')",
        [],
    ) {
        crate::diag!("[loss] known-envelope prune failed (non-fatal): {e}");
    }
}

/// Is there already a real (decrypted) row for this id? A placeholder does not
/// count — a later successful decrypt replaces it.
pub(crate) fn has_real_row(conn: &rusqlite::Connection, id: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM message WHERE id = ?1 AND undecryptable IS NULL",
        rusqlite::params![id],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

/// Write the placeholder for a lost envelope. Idempotent.
///
/// The envelope's `sender_id` is copied as-is: under sealed sender that is the
/// `sealed` sentinel, so the UI shows no author, which is the truth.
pub(crate) fn write_placeholder(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    envelope_id: &str,
    sender_id: &str,
    sent_at: &str,
    reason: Reason,
) -> bool {
    match conn.execute(
        "INSERT OR IGNORE INTO message
         (id, conversation_id, sender_id, ciphertext, content, sent_at, undecryptable)
         VALUES (?1, ?2, ?3, X'', NULL, ?4, ?5)",
        rusqlite::params![envelope_id, conversation_id, sender_id, sent_at, reason.as_str()],
    ) {
        Ok(n) => n > 0,
        Err(e) => {
            crate::diag!("[loss] could not write placeholder for {envelope_id}: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_joined_or_pre_join_is_not_a_loss() {
        assert_eq!(classify((0, 5), 0, None, false), None);
        assert_eq!(classify((0, 2), 0, Some(3), true), None);
        assert_eq!(classify((0, 2), 0, Some(3), false), None);
    }

    #[test]
    fn at_or_after_the_join_is_a_loss() {
        assert_eq!(classify((0, 3), 0, Some(3), false), Some(Reason::Stranded));
        assert_eq!(classify((0, 7), 0, Some(3), true), Some(Reason::Failed));
    }

    #[test]
    fn a_retired_lineage_this_device_held_is_a_loss() {
        assert_eq!(classify((0, 4), 1, Some(0), false), Some(Reason::Lineage));
        assert_eq!(classify((0, 4), 1, None, false), None);
    }

    #[test]
    fn the_fallback_floor_skips_the_start_epoch() {
        assert_eq!(fallback_floor(4), 5);
        assert_eq!(fallback_floor(u64::MAX), u64::MAX);
    }

    fn db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::local::apply_local_schema(&conn).unwrap();
        conn
    }

    /// The schema refuses a placeholder with content or an unknown reason.
    #[test]
    fn a_placeholder_cannot_hold_content() {
        let conn = db();
        let bad_content = conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, undecryptable)
             VALUES ('a', 'c', 's', X'', 'hello', 't', 'failed')",
            [],
        );
        assert!(bad_content.is_err());
        let bad_reason = conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, undecryptable)
             VALUES ('b', 'c', 's', X'', NULL, 't', 'whatever')",
            [],
        );
        assert!(bad_reason.is_err());
        assert!(write_placeholder(&conn, "c", "d", "sealed", "t", Reason::Stranded));
        assert!(!write_placeholder(&conn, "c", "d", "sealed", "t", Reason::Stranded), "idempotent");
        assert!(!has_real_row(&conn, "d"), "a placeholder is not a real row");
    }

    #[test]
    fn known_envelopes_are_remembered() {
        let conn = db();
        assert!(!is_known_envelope(&conn, "e"));
        record_own_envelope(&conn, "e");
        record_opened_envelope(&conn, "e");
        assert!(is_known_envelope(&conn, "e"));
        record_opened_envelope(&conn, "f");
        assert!(is_known_envelope(&conn, "f"));
        prune_known_envelopes(&conn);
        assert!(is_known_envelope(&conn, "e"), "a fresh record survives the prune");
    }
}
