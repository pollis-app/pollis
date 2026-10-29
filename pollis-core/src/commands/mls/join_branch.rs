//! Unconfirmed external-join branches, and who may replace a local group (#1220).
//!
//! An external join merges its commit before the log's CAS decides; until then the stored group is an unconfirmed *branch*. Every other local group is *confirmed* and holds the only keys for its epoch.
//!
//! The marker here tells the two apart, and two predicates decide every
//! replacement:
//!
//! * [`external_join_may_replace`]: only its own unconfirmed branch; a confirmed group makes the join stand down.
//! * [`welcome_may_replace`]: a strictly older group, or an unconfirmed branch at the same epoch.
//!
//! Together they make discarding a log-carried group for a branch unrepresentable.

/// `mls_kv` scope for the marker rows, keyed by lineage GroupId; the stored epoch is diagnostic only.
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
        crate::diag!(
            "[mls] could not clear the unconfirmed-join marker for {conversation_id} \
             generation {generation}: {e}"
        );
    }
}

/// Is the stored group an unconfirmed external-join branch? A failed read answers `false`, which only ever keeps a group.
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
pub(super) fn external_join_may_replace(stored: bool, unconfirmed: bool) -> bool {
    !stored || unconfirmed
}

/// May a Welcome at `welcome_epoch` replace the group stored for its lineage at
/// `local_epoch`?
///
/// Confirmed: only a STRICTLY newer Welcome (#1161 C1). Unconfirmed: also one at its own epoch, since the branch is doomed.
pub(super) fn welcome_may_replace(local_epoch: u64, welcome_epoch: u64, unconfirmed: bool) -> bool {
    if unconfirmed {
        welcome_epoch >= local_epoch
    } else {
        welcome_epoch > local_epoch
    }
}

/// `mls_kv` scope for the epoch from which this device is entitled to read each
/// lineage (#1312). Key: as [`UNCONFIRMED_JOIN_SCOPE`]. Value: the epoch,
/// little-endian.
///
/// It separates the never-sent loss from real losses. Two ways in:
///
/// * **Added** ([`record_added`]): replaces any earlier value; earlier messages were never sent to it.
/// * **Held** ([`record_first_join`]): only the FIRST is kept, so a recovery rebuild's gap stays a real loss.
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

/// Record that this device holds the lineage `(conversation_id, generation)`
/// from `epoch` on, unless a join is already recorded. Best-effort: a missing
/// floor only means the loss check stays quiet for that lineage.
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

/// The epoch at which this device first held `(conversation_id, generation)`,
/// or `None` if no join was ever recorded for it.
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

/// `mls_kv` scope for the "entitled since" marker (#1571). Key: as
/// [`UNCONFIRMED_JOIN_SCOPE`]. Value: an RFC 3339 UTC instant, UTF-8.
///
/// A device registered before its user joined should have been in the tree all along, so a later first hold's gap is a real loss; this marks that case for `messages::ingest::record_losses`.
const ENTITLED_SINCE_SCOPE: &str = "pollis_entitled_since";

/// Parse the DS's two timestamp shapes: RFC 3339 and SQLite `datetime('now')` (UTC). Answers the start of the window; see [`ds_instant_window`].
pub(crate) fn parse_ds_instant(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    ds_instant_window(raw).map(|(start, _)| start)
}

/// The window a stored stamp names: `datetime('now')` truncates to the second. Comparisons take the side that keeps an undecidable case silent.
pub(crate) fn ds_instant_window(
    raw: &str,
) -> Option<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)> {
    let second = chrono::Duration::seconds(1);
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(raw) {
        let t = t.with_timezone(&chrono::Utc);
        let fractional = raw.contains('.') || raw.contains(',');
        return Some(if fractional { (t, t) } else { (t, t + second) });
    }
    if let Ok(n) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S") {
        let t = n.and_utc();
        return Some((t, t + second));
    }
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f")
        .ok()
        .map(|n| {
            let t = n.and_utc();
            (t, t)
        })
}

/// The instant from which this device was entitled despite not holding the group: only when registered strictly before its user's membership. Answers the end of the membership window; missing input is `None`.
pub(crate) fn entitlement(
    device_registered_at: Option<&str>,
    member_since: Option<&str>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let (_, registered_end) = ds_instant_window(device_registered_at?)?;
    let (since_start, since_end) = ds_instant_window(member_since?)?;
    if registered_end <= since_start {
        Some(since_end)
    } else {
        None
    }
}

/// `mls_kv` scope marking a join as this device's FIRST hold of a lineage
/// (#1571), written just before the join records its floor and consumed by
/// [`record_entitlement`]. Value: empty.
///
/// Only a never-held lineage qualifies; a re-add keeps #1524's silent gap.
const FRESH_JOIN_SCOPE: &str = "pollis_fresh_join";

/// Mark the join about to record its floor as this device's first hold of
/// `(conversation_id, generation)`, if no floor exists yet. Call BEFORE
/// [`record_first_join`] / [`record_added`].
pub(crate) fn mark_fresh_if_first(conn: &rusqlite::Connection, conversation_id: &str, generation: i64) {
    if first_join(conn, conversation_id, generation).is_some() {
        return;
    }
    if let Err(e) = conn.execute(
        "INSERT OR REPLACE INTO mls_kv (scope, key, value) VALUES (?1, ?2, X'')",
        rusqlite::params![FRESH_JOIN_SCOPE, key(conversation_id, generation)],
    ) {
        crate::diag!("[mls] could not mark the first join of {conversation_id}: {e}");
    }
}

/// Consume the [`mark_fresh_if_first`] marker: was the join just recorded this
/// device's first hold of the lineage?
pub(crate) fn take_fresh(conn: &rusqlite::Connection, conversation_id: &str, generation: i64) -> bool {
    conn.execute(
        "DELETE FROM mls_kv WHERE scope = ?1 AND key = ?2",
        rusqlite::params![FRESH_JOIN_SCOPE, key(conversation_id, generation)],
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// Record, for the lineage this device just joined, whether (and since when) it
/// was entitled to what was sent before that join — see [`ENTITLED_SINCE_SCOPE`].
/// Only a FIRST hold (`fresh`) qualifies. Best-effort.
pub(crate) fn record_entitlement(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    fresh: bool,
    device_registered_at: Option<&str>,
    member_since: Option<&str>,
) {
    if !fresh {
        return;
    }
    let Some(since) = entitlement(device_registered_at, member_since) else {
        return;
    };
    // Only what was sealed below the first-hold floor; a later re-add moves the floor but not this bound (#1524).
    let Some(below_epoch) = first_join(conn, conversation_id, generation) else {
        return;
    };
    let value = format!("{} {below_epoch}", since.to_rfc3339());
    if let Err(e) = conn.execute(
        "INSERT OR REPLACE INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
        rusqlite::params![ENTITLED_SINCE_SCOPE, key(conversation_id, generation), value.into_bytes()],
    ) {
        crate::diag!(
            "[mls] could not record the entitlement of {conversation_id} generation {generation}: {e}"
        );
    }
}

/// What [`record_entitlement`] stored: the device was entitled to envelopes of
/// the lineage sent at or after `since` and sealed below `below_epoch` (its
/// first hold).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entitlement {
    pub since: chrono::DateTime<chrono::Utc>,
    pub below_epoch: u64,
}

/// The entitlement this device recorded for `(conversation_id, generation)`
/// while it was outside the tree, if [`record_entitlement`] found one.
pub(crate) fn entitled_since(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
) -> Option<Entitlement> {
    use rusqlite::OptionalExtension;
    let raw: Vec<u8> = conn
        .query_row(
            "SELECT value FROM mls_kv WHERE scope = ?1 AND key = ?2",
            rusqlite::params![ENTITLED_SINCE_SCOPE, key(conversation_id, generation)],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()?;
    let raw = std::str::from_utf8(&raw).ok()?;
    let (since, below) = raw.split_once(' ')?;
    Some(Entitlement {
        since: parse_ds_instant(since)?,
        below_epoch: below.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1571: only a device registered before its user's membership is entitled.
    #[test]
    fn only_a_device_registered_before_the_membership_is_entitled() {
        let since = "2026-10-20T01:49:21.000+00:00";
        assert!(entitlement(Some("2026-10-20 01:38:40"), Some(since)).is_some());
        assert_eq!(entitlement(Some("2026-10-20 01:50:00"), Some(since)), None);
        assert_eq!(entitlement(None, Some(since)), None);
        assert_eq!(entitlement(Some("2026-10-20 01:38:40"), None), None);
        assert_eq!(entitlement(Some("not a time"), Some(since)), None);
    }

    /// `datetime('now')` stamps are whole seconds: a registration or a
    /// membership in the same second as the other decides nothing, and the
    /// entitlement starts after the membership's second.
    #[test]
    fn second_granular_stamps_are_windows_not_instants() {
        assert_eq!(entitlement(Some("2026-10-20 01:49:21"), Some("2026-10-20 01:49:21")), None);
        assert_eq!(entitlement(Some("2026-10-20 01:49:20"), Some("2026-10-20T01:49:20.500Z")), None);
        assert_eq!(
            entitlement(Some("2026-10-20 01:49:20"), Some("2026-10-20 01:49:21")),
            parse_ds_instant("2026-10-20T01:49:22Z"),
        );
        assert_eq!(
            entitlement(Some("2026-10-20 01:49:20"), Some("2026-10-20T01:49:21.250Z")),
            parse_ds_instant("2026-10-20T01:49:21.250Z"),
        );
    }

    #[test]
    fn the_entitlement_marker_round_trips_per_lineage() {
        let conn = kv();
        assert_eq!(entitled_since(&conn, "c", 0), None);
        record_first_join(&conn, "c", 0, 4);
        record_entitlement(&conn, "c", 0, true, Some("2026-10-20 01:50:00"), Some("2026-10-20T01:49:21Z"));
        assert_eq!(entitled_since(&conn, "c", 0), None, "a newer device records nothing");
        record_entitlement(&conn, "c", 0, false, Some("2026-10-20 01:38:40"), Some("2026-10-20T01:49:21.000Z"));
        assert_eq!(entitled_since(&conn, "c", 0), None, "a re-join of a held lineage records nothing");
        record_entitlement(&conn, "c", 0, true, Some("2026-10-20 01:38:40"), Some("2026-10-20T01:49:21.000Z"));
        assert_eq!(
            entitled_since(&conn, "c", 0),
            Some(Entitlement {
                since: parse_ds_instant("2026-10-20T01:49:21.000Z").unwrap(),
                below_epoch: 4,
            }),
            "bounded by the first hold's floor"
        );
        // A removal and re-add moves the floor; the bound stays.
        record_added(&conn, "c", 0, 11);
        assert_eq!(entitled_since(&conn, "c", 0).map(|e| e.below_epoch), Some(4));
        assert_eq!(entitled_since(&conn, "c", 1), None);
    }

    #[test]
    fn only_a_first_hold_is_fresh() {
        let conn = kv();
        mark_fresh_if_first(&conn, "c", 0);
        assert!(take_fresh(&conn, "c", 0));
        assert!(!take_fresh(&conn, "c", 0), "consumed");
        record_first_join(&conn, "c", 0, 2);
        mark_fresh_if_first(&conn, "c", 0);
        assert!(!take_fresh(&conn, "c", 0), "a lineage held before is not fresh");
    }

    /// A rebuild keeps the first floor; being ADDED again moves it.
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
