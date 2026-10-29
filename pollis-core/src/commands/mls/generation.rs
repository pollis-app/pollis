//! Suite generations — the client half of #454 P4.
//!
//! MLS fixes the suite at creation, so a suite change is a successor group joined by Welcome, restarting at epoch 0; the log key is `(conversation, generation, epoch)`.
//!
//! ## Why the local GroupId carries the generation
//!
//! Suffixing the successor's `GroupId` lets both lineages coexist in one openmls store:
//!
//! 1. A migrator that loses the race just deletes the successor.
//! 2. A joiner can apply a successor Welcome before draining its predecessor.
//! 3. [`super::provider::load_stored_group`] resolves the lineage, so callers need not know it.
//!
//! The row advances only after the predecessor is drained.

use openmls::prelude::GroupId;

/// Separator before the generation in a local `GroupId`; ULIDs never contain it.
const GENERATION_MARK: &str = "#g";

/// The local openmls `GroupId` for `conversation_id` at suite `generation`.
///
/// Generation 0 is the conversation id verbatim.
pub(crate) fn mls_group_id(conversation_id: &str, generation: i64) -> GroupId {
    GroupId::from_slice(mls_group_id_str(conversation_id, generation).as_bytes())
}

/// String form of [`mls_group_id`].
pub(crate) fn mls_group_id_str(conversation_id: &str, generation: i64) -> String {
    if generation <= 0 {
        conversation_id.to_string()
    } else {
        format!("{conversation_id}{GENERATION_MARK}{generation}")
    }
}

/// Inverse of [`mls_group_id_str`], for the group id carried by an inbound Welcome.
///
/// Anything malformed reads as generation 0.
pub(crate) fn split_mls_group_id(raw: &str) -> (String, i64) {
    match raw.rsplit_once(GENERATION_MARK) {
        Some((conversation_id, suffix)) => match suffix.parse::<i64>() {
            Ok(generation) if generation >= 1 && !conversation_id.is_empty() => {
                (conversation_id.to_string(), generation)
            }
            _ => (raw.to_string(), 0),
        },
        None => (raw.to_string(), 0),
    }
}

/// This device's suite generation for `conversation_id`; a missing row or error is 0.
pub(crate) fn local_generation(conn: &rusqlite::Connection, conversation_id: &str) -> i64 {
    conn.query_row(
        "SELECT generation FROM mls_generation WHERE conversation_id = ?1",
        rusqlite::params![conversation_id],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

/// Advance this device onto `generation` for `conversation_id`.
///
/// Monotone: moving back onto a closed lineage would wedge the device.
pub(crate) fn set_local_generation(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
) {
    if let Err(e) = conn.execute(
        "INSERT INTO mls_generation (conversation_id, generation, updated_at) \
         VALUES (?1, ?2, datetime('now')) \
         ON CONFLICT(conversation_id) DO UPDATE SET \
             generation = excluded.generation, updated_at = excluded.updated_at \
         WHERE excluded.generation > mls_generation.generation",
        rusqlite::params![conversation_id, generation],
    ) {
        crate::diag!(
            "[mls] generation: recording generation {generation} for {conversation_id} failed: {e}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mls_generation ( \
                 conversation_id TEXT PRIMARY KEY, \
                 generation      INTEGER NOT NULL, \
                 updated_at      TEXT NOT NULL DEFAULT (datetime('now')) \
             );",
        )
        .unwrap();
        conn
    }

    /// A never-migrated conversation keeps its `GroupId` unchanged.
    #[test]
    fn generation_zero_is_the_conversation_id_verbatim() {
        assert_eq!(mls_group_id_str("01KQYX89", 0), "01KQYX89");
        assert_eq!(split_mls_group_id("01KQYX89"), ("01KQYX89".to_string(), 0));
    }

    #[test]
    fn successor_ids_round_trip() {
        for generation in 1..=3 {
            let raw = mls_group_id_str("01KQYX89", generation);
            assert_eq!(split_mls_group_id(&raw), ("01KQYX89".to_string(), generation));
        }
    }

    /// A conversation id that merely *looks* like it carries a suffix must not
    /// be silently re-homed onto another lineage.
    #[test]
    fn malformed_suffixes_read_as_generation_zero() {
        for raw in ["conv#gx", "conv#g", "conv#g0", "conv#g-1", "#g1", "conv"] {
            assert_eq!(split_mls_group_id(raw).1, 0, "{raw}");
        }
    }

    #[test]
    fn absent_row_is_generation_zero() {
        let conn = mem();
        assert_eq!(local_generation(&conn, "conv"), 0);
    }

    /// The generation only ever moves forward.
    #[test]
    fn set_local_generation_only_moves_forward() {
        let conn = mem();
        set_local_generation(&conn, "conv", 1);
        assert_eq!(local_generation(&conn, "conv"), 1);
        set_local_generation(&conn, "conv", 3);
        assert_eq!(local_generation(&conn, "conv"), 3);
        set_local_generation(&conn, "conv", 2);
        assert_eq!(local_generation(&conn, "conv"), 3);
        set_local_generation(&conn, "conv", 0);
        assert_eq!(local_generation(&conn, "conv"), 3);
    }

    #[test]
    fn generations_are_per_conversation() {
        let conn = mem();
        set_local_generation(&conn, "a", 2);
        assert_eq!(local_generation(&conn, "a"), 2);
        assert_eq!(local_generation(&conn, "b"), 0);
    }
}
