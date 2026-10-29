//! Unconfirmed external-join branches, and who may replace a local group (#1220).
//!
//! An external join merges its commit locally before the commit-log CAS decides
//! it; until then the stored group is a *branch*. Every other local group is
//! *confirmed* and, under `max_past_epochs = 0`, holds the only keys this device
//! will ever have for its epoch. [`external_join_may_replace`] and
//! [`welcome_may_replace`] make "discard a confirmed group for a branch"
//! unrepresentable; see `.codesight/wiki/mls.md` → "Join paths never replace a
//! confirmed group".

/// `mls_kv` scope for the marker rows, keyed by the lineage's raw MLS GroupId.
/// The value (branch epoch, little-endian) is diagnostic only.
const UNCONFIRMED_JOIN_SCOPE: &str = "pollis_unconfirmed_join";

fn key(conversation_id: &str, generation: i64) -> Vec<u8> {
    super::generation::mls_group_id_str(conversation_id, generation).into_bytes()
}

/// Record that the stored group is an external-join branch the log has not
/// accepted yet. Written in the same critical section that stores the branch.
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

pub(super) fn clear(conn: &rusqlite::Connection, conversation_id: &str, generation: i64) {
    if let Err(e) = conn.execute(
        "DELETE FROM mls_kv WHERE scope = ?1 AND key = ?2",
        rusqlite::params![UNCONFIRMED_JOIN_SCOPE, key(conversation_id, generation)],
    ) {
        crate::diag!(
            "[mls] could not clear the unconfirmed-join marker for {conversation_id} \
             generation {generation}: {e}"
        );
    }
}

/// A failed read answers `false`: treating a branch as confirmed only ever keeps
/// a group, and a doomed branch that is kept is caught by the replay's fork
/// recovery.
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

/// May an external join replace what is stored for its lineage? Only nothing,
/// or this device's own unconfirmed branch; a confirmed group means the device
/// is already a member and the join stands down.
pub(super) fn external_join_may_replace(stored: bool, unconfirmed: bool) -> bool {
    !stored || unconfirmed
}

/// May a Welcome at `welcome_epoch` replace the group stored at `local_epoch`?
///
/// A confirmed group yields only to a STRICTLY newer Welcome (#1161 C1). An
/// unconfirmed branch also yields to one at its own epoch: that Welcome comes
/// from a commit that already holds the epoch the branch is trying to claim.
pub(super) fn welcome_may_replace(local_epoch: u64, welcome_epoch: u64, unconfirmed: bool) -> bool {
    if unconfirmed {
        welcome_epoch >= local_epoch
    } else {
        welcome_epoch > local_epoch
    }
}

/// `mls_kv` scope for the epoch from which this device is entitled to read each
/// lineage (#1312), separating "never sent to this device" from a real loss.
///
/// * **Added** (a Welcome, [`record_added`]): replaces any earlier value.
/// * **Held** (creating the group, an external join, [`record_first_join`]):
///   only the FIRST is kept, so a recovery rebuild keeps the original floor.
const FIRST_JOIN_SCOPE: &str = "pollis_first_join";

fn write_join(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    epoch: u64,
    verb: &str,
) {
    if let Err(e) = conn.execute(
        &format!("{verb} INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)"),
        rusqlite::params![
            FIRST_JOIN_SCOPE,
            key(conversation_id, generation),
            epoch.to_le_bytes().to_vec()
        ],
    ) {
        crate::diag!(
            "[mls] could not record the join of {conversation_id} generation {generation}: {e}"
        );
    }
}

/// Record that this device holds the lineage from `epoch` on, unless a join is
/// already recorded. Best-effort: a missing floor only silences the loss check.
pub(crate) fn record_first_join(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    epoch: u64,
) {
    write_join(conn, conversation_id, generation, epoch, "INSERT OR IGNORE");
}

/// Record that this device was ADDED to the lineage's tree at `epoch` (a
/// Welcome). Replaces any earlier floor — see [`FIRST_JOIN_SCOPE`].
pub(crate) fn record_added(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    epoch: u64,
) {
    write_join(conn, conversation_id, generation, epoch, "INSERT OR REPLACE");
}

pub(crate) fn first_join(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
) -> Option<u64> {
    use rusqlite::OptionalExtension;
    let raw: Vec<u8> = conn
        .query_row(
            "SELECT value FROM mls_kv WHERE scope = ?1 AND key = ?2",
            rusqlite::params![FIRST_JOIN_SCOPE, key(conversation_id, generation)],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()?;
    let bytes: [u8; 8] = raw.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rebuild_keeps_the_floor_and_a_re_add_moves_it() {
        let conn = kv();
        assert_eq!(first_join(&conn, "c", 0), None);
        record_first_join(&conn, "c", 0, 3);
        record_first_join(&conn, "c", 0, 9);
        assert_eq!(first_join(&conn, "c", 0), Some(3));
        assert_eq!(first_join(&conn, "c", 1), None, "a successor lineage has its own floor");
        record_first_join(&conn, "c", 1, 0);
        assert_eq!(first_join(&conn, "c", 1), Some(0));
        record_added(&conn, "c", 0, 7);
        assert_eq!(first_join(&conn, "c", 0), Some(7));
    }

    fn kv() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mls_kv (scope TEXT NOT NULL, key BLOB NOT NULL, value BLOB NOT NULL, \
             PRIMARY KEY (scope, key));",
        )
        .unwrap();
        conn
    }

    #[test]
    fn an_external_join_never_replaces_a_confirmed_group() {
        assert!(!external_join_may_replace(true, false));
        assert!(external_join_may_replace(true, true));
        assert!(external_join_may_replace(false, false));
        assert!(external_join_may_replace(false, true));
    }

    #[test]
    fn a_confirmed_group_is_replaced_only_by_a_strictly_newer_welcome() {
        assert!(!welcome_may_replace(1, 0, false));
        assert!(!welcome_may_replace(1, 1, false));
        assert!(welcome_may_replace(1, 2, false));
    }

    /// #1220 ordering B: a branch at epoch 1 yields to the creator's Welcome at
    /// epoch 1, not to an older one.
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

    #[test]
    fn an_unreadable_marker_reads_as_confirmed() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        assert!(!is_unconfirmed(&conn, "c", 0));
    }
}
