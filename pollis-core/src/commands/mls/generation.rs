//! Suite generations — the client half of #454 P4.
//!
//! MLS binds the ciphersuite into the group at creation and offers no in-band
//! way to change it, so upgrading a live conversation to the post-quantum hybrid
//! suite cannot be a commit. It is a **successor group**: a second MLS group for
//! the same conversation, in the new suite, whose roster is moved across by
//! Welcome. The successor restarts at MLS epoch 0, which is why the canonical
//! log's monotone key widened from `(conversation, epoch)` to
//! `(conversation, generation, epoch)`, ordered lexicographically — see
//! `pollis_delivery::commit`.
//!
//! ## Generation 0 is "every group that exists today"
//!
//! A conversation with no [`mls_generation`] row is on generation 0, and the
//! generation-0 local `GroupId` is the conversation id verbatim. So this module
//! renames nothing, migrates nothing, and is completely inert until some
//! conversation actually migrates. A pre-P4 client sends no generation field,
//! the DS defaults it to 0, and 0 is exactly the lineage that client is in.
//!
//! ## Why the local GroupId carries the generation
//!
//! openmls keys group state by `GroupId`, and refuses two groups with the same
//! id in one store. If the successor reused the conversation id, creating it
//! would have to destroy the predecessor first — before the predecessor had been
//! drained, and with no way back if the migration then lost its
//! compare-and-swap. Suffixing the successor's id instead makes the two lineages
//! *coexist locally*, which buys three properties outright:
//!
//! 1. A migrator whose commit loses the race rolls back by deleting the
//!    successor. Its classic group is untouched — no rebuild, no lost keys.
//! 2. A joiner can apply a successor Welcome the moment it arrives and still
//!    drain its predecessor afterwards, because applying the Welcome no longer
//!    clobbers anything. Nothing has to be sequenced inside `apply_welcome`.
//! 3. "Which lineage is this device on?" is one row, and every existing call
//!    site keeps working: [`super::provider::load_stored_group`] resolves it, so
//!    encrypt / decrypt / catch-up / reconcile all follow the generation without
//!    knowing it exists.
//!
//! The row advances only after the predecessor is drained. Until it does, the
//! successor is stored but unreachable — the transition is a single write.

use openmls::prelude::GroupId;

/// Separator between a conversation id and its suite generation inside the local
/// MLS `GroupId`. Conversation ids are ULIDs (Crockford base32), so this can
/// never occur in one; [`split_mls_group_id`] additionally requires the suffix
/// to parse as a positive integer before it will strip anything.
const GENERATION_MARK: &str = "#g";

/// The local openmls `GroupId` for `conversation_id` at suite `generation`.
///
/// Generation 0 maps to the conversation id verbatim — that is every group that
/// exists today, so no stored group is renamed by P4.
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

/// Inverse of [`mls_group_id_str`]: recover `(conversation_id, generation)` from
/// a raw openmls group id — the id carried by an inbound Welcome, which is the
/// only way a joiner learns which lineage it was just invited into.
///
/// Anything that is not a well-formed suffix (no mark, a non-numeric or
/// non-positive suffix, an empty conversation part) reads as generation 0, so a
/// pre-P4 group id round-trips unchanged.
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

/// The suite generation this device currently reads and writes for
/// `conversation_id`. A missing row — and any read error — is generation 0, the
/// lineage every pre-P4 group is on.
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
/// Monotone by construction: the `WHERE` clause refuses to move a device *back*
/// onto a lineage the DS has already closed. Reversing would put this device on
/// a lineage that rejects every commit and carries no new messages — a wedge
/// with no recovery path, since the successor's Welcome has already been acked.
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
        eprintln!(
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

    /// The property that makes P4 backward-compatible: a conversation that has
    /// never migrated keeps the exact `GroupId` it has today, so no shipped
    /// device's stored MLS state is renamed out from under it.
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

    /// The transition is a single write, and it only ever moves forward: a
    /// device that has adopted the successor can never be walked back onto the
    /// closed lineage, which rejects every commit and carries no new messages.
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
