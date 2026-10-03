//! Unconfirmed external-join branches, and who may replace a local group (#1220).
//!
//! An external join builds and MERGES its commit locally before the commit-log
//! compare-and-swap decides whether it won. Until then the stored group is a
//! *branch*: if another member's commit took the same epoch, it was never part
//! of the canonical log and has to go. Every other local group, however it got
//! here (a Welcome, our own creation, an external join that won), is
//! *confirmed*: it sits on the canonical log, and under `max_past_epochs = 0` it
//! holds the only keys this device will ever have for its current epoch.
//!
//! Before #1220 the two looked the same, so both join paths replaced whatever
//! was stored:
//!
//! * A recipient that lost the epoch-0 race dropped its branch and, on the
//!   retry, external-joined again. If its Welcome had been applied in between,
//!   that retry deleted the confirmed Welcome group (epoch 1) and joined at
//!   epoch 2. Anything the creator sealed at epoch 1 was undecryptable from then
//!   on, and the ingest cursor (correctly, for an epoch the device no longer
//!   holds) moved past it: the message was lost.
//! * The same Welcome, polled while the doomed branch was still stored at its
//!   epoch, was refused as a replay and acknowledged — gone for good — and the
//!   retry ended in the same place.
//!
//! The marker here tells the two apart, and two predicates decide every
//! replacement:
//!
//! * [`external_join_may_replace`]: an external join replaces nothing but its
//!   own unconfirmed branch. A confirmed group is never thrown away to build
//!   another branch; the join stands down instead (the device is already a
//!   member). Callers that genuinely need to rebuild a broken group say so by
//!   deleting it first (`forget_local_mls_group_at`).
//! * [`welcome_may_replace`]: a Welcome replaces a strictly older group, as
//!   before, and ALSO an unconfirmed branch at the same epoch. A Welcome at or
//!   above a branch's epoch was produced by a commit at or above the epoch the
//!   branch is trying to claim, so the branch's compare-and-swap cannot win.
//!
//! Together they make "a device discards a group the canonical log carries,
//! for a branch it does not" unrepresentable on these paths, which is what the
//! watermark's rule "an envelope below the epoch this pass started at is
//! permanently undecryptable" relies on.
//!
//! Stored in `mls_kv` beside the group state it describes, under its own scope,
//! so it needs no local schema change and travels with the MLS state in every
//! database that has one.

/// `mls_kv` scope for the marker rows. Key: the raw MLS GroupId bytes of the
/// lineage (`mls_group_id(conversation_id, generation)`). Value: the epoch the
/// branch was built at, little-endian — kept for diagnostics only; the
/// decisions below need only the marker's presence.
const UNCONFIRMED_JOIN_SCOPE: &str = "pollis_unconfirmed_join";

fn key(conversation_id: &str, generation: i64) -> Vec<u8> {
    super::generation::mls_group_id_str(conversation_id, generation).into_bytes()
}

/// Record that the group stored for `(conversation_id, generation)` is an
/// external-join branch whose commit has not been accepted by the log yet.
/// Written in the same local-DB critical section that stores the branch.
pub(super) fn mark_unconfirmed(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    branch_epoch: u64,
) -> crate::error::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
        rusqlite::params![
            UNCONFIRMED_JOIN_SCOPE,
            key(conversation_id, generation),
            branch_epoch.to_le_bytes().to_vec()
        ],
    )?;
    Ok(())
}

/// Drop the marker: the branch won (it is now confirmed), or it was deleted.
pub(super) fn clear(conn: &rusqlite::Connection, conversation_id: &str, generation: i64) {
    if let Err(e) = conn.execute(
        "DELETE FROM mls_kv WHERE scope = ?1 AND key = ?2",
        rusqlite::params![UNCONFIRMED_JOIN_SCOPE, key(conversation_id, generation)],
    ) {
        eprintln!(
            "[mls] could not clear the unconfirmed-join marker for {conversation_id} \
             generation {generation}: {e}"
        );
    }
}

/// Is the group stored for `(conversation_id, generation)` an unconfirmed
/// external-join branch? A failed read answers `false`: treating a branch as
/// confirmed only ever keeps a group, never destroys one, and a doomed branch
/// that is kept is caught by the replay's fork recovery.
pub(super) fn is_unconfirmed(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
) -> bool {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT 1 FROM mls_kv WHERE scope = ?1 AND key = ?2",
        rusqlite::params![UNCONFIRMED_JOIN_SCOPE, key(conversation_id, generation)],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

/// May an external join replace what is stored for its lineage?
///
/// `stored` = a group exists for the lineage; `unconfirmed` = it is this
/// device's own branch that the log has not accepted. Only nothing, or an
/// unconfirmed branch, may be replaced. A confirmed group means the device is
/// already a member, and the join must stand down rather than delete it.
pub(super) fn external_join_may_replace(stored: bool, unconfirmed: bool) -> bool {
    !stored || unconfirmed
}

/// May a Welcome at `welcome_epoch` replace the group stored for its lineage at
/// `local_epoch`?
///
/// A confirmed group is replaced only by a STRICTLY newer Welcome; one at or
/// below it is a replay or a fabrication (#1161 C1). An unconfirmed branch is
/// also replaced by a Welcome at its own epoch: the branch was built on the
/// GroupInfo at `local_epoch - 1` and is trying to claim that epoch, and a
/// Welcome at `local_epoch` or above comes from a commit that already holds it,
/// so the branch is doomed and the Welcome is the canonical way in.
pub(super) fn welcome_may_replace(local_epoch: u64, welcome_epoch: u64, unconfirmed: bool) -> bool {
    if unconfirmed {
        welcome_epoch >= local_epoch
    } else {
        welcome_epoch > local_epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mls_kv (scope TEXT NOT NULL, key BLOB NOT NULL, value BLOB NOT NULL, \
             PRIMARY KEY (scope, key));",
        )
        .unwrap();
        conn
    }

    /// The invariant itself: a confirmed group is never replaced by an external
    /// join, whatever else is true.
    #[test]
    fn an_external_join_never_replaces_a_confirmed_group() {
        assert!(!external_join_may_replace(true, false));
        assert!(external_join_may_replace(true, true));
        assert!(external_join_may_replace(false, false));
        assert!(external_join_may_replace(false, true));
    }

    /// A confirmed group keeps the #1161 C1 rule: only a strictly newer Welcome.
    #[test]
    fn a_confirmed_group_is_replaced_only_by_a_strictly_newer_welcome() {
        assert!(!welcome_may_replace(1, 0, false));
        assert!(!welcome_may_replace(1, 1, false));
        assert!(welcome_may_replace(1, 2, false));
    }

    /// The #1220 ordering B: the branch is at epoch 1 (built on GroupInfo 0),
    /// the Welcome from the creator's Add is at epoch 1. The Welcome wins; an
    /// older Welcome still does not.
    #[test]
    fn an_unconfirmed_branch_yields_to_a_welcome_at_its_own_epoch() {
        assert!(welcome_may_replace(1, 1, true));
        assert!(welcome_may_replace(1, 2, true));
        assert!(!welcome_may_replace(2, 1, true));
    }

    #[test]
    fn the_marker_is_per_lineage_and_clears() {
        let conn = kv();
        assert!(!is_unconfirmed(&conn, "c", 0));
        mark_unconfirmed(&conn, "c", 0, 1).unwrap();
        assert!(is_unconfirmed(&conn, "c", 0));
        assert!(!is_unconfirmed(&conn, "c", 1), "a successor lineage is not marked");
        assert!(!is_unconfirmed(&conn, "d", 0));
        clear(&conn, "c", 0);
        assert!(!is_unconfirmed(&conn, "c", 0));
    }

    /// No `mls_kv` at all reads as "confirmed" — the bias that keeps groups.
    #[test]
    fn an_unreadable_marker_reads_as_confirmed() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        assert!(!is_unconfirmed(&conn, "c", 0));
    }
}
