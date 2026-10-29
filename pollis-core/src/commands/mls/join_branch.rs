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
//! [`external_join_may_replace`] and [`welcome_may_replace`] decide every
//! replacement, so "a device discards a group the canonical log carries, for a
//! branch it does not" is unrepresentable on these paths; the watermark relies
//! on it. See `.codesight/wiki/mls.md` → "Join paths never replace a confirmed
//! group".

/// `mls_kv` scope for the marker rows. Key: the lineage's raw MLS GroupId
/// bytes. Value: the branch's epoch, little-endian, for diagnostics only; the
/// decisions need only the marker's presence.
const UNCONFIRMED_JOIN_SCOPE: &str = "pollis_unconfirmed_join";

fn key(conversation_id: &str, generation: i64) -> Vec<u8> {
    super::generation::mls_group_id_str(conversation_id, generation).into_bytes()
}

/// Mark the stored group as an external-join branch the log has not accepted
/// yet. Must be written in the same local-DB critical section that stores it.
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

/// A failed read answers `false`: treating a branch as confirmed only ever
/// keeps a group, and a doomed branch that is kept is caught by the replay's
/// fork recovery.
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

/// Only nothing, or this device's own unconfirmed branch, may be replaced. A
/// confirmed group means the device is already a member, so the join stands
/// down; a caller that must rebuild deletes the group first
/// (`forget_local_mls_group_at`).
pub(super) fn external_join_may_replace(stored: bool, unconfirmed: bool) -> bool {
    !stored || unconfirmed
}

/// A confirmed group is replaced only by a strictly newer Welcome; one at or
/// below it is a replay or a fabrication (#1161 C1). An unconfirmed branch also
/// yields to a Welcome at its own epoch: that Welcome's commit already holds the
/// epoch the branch is trying to claim, so the branch's CAS cannot win.
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
/// A Welcome ([`record_added`]) replaces any earlier floor: the gap before it
/// was never sent to this device. Creating or external-joining
/// ([`record_first_join`]) keeps only the first: a recovery rebuild's gap was
/// sent while the device was a member and is a real loss.
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

/// Best-effort: a missing floor only means the loss check stays quiet for that
/// lineage.
pub(crate) fn record_first_join(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    epoch: u64,
) {
    write_join(conn, conversation_id, generation, epoch, "INSERT OR IGNORE");
}

/// Replaces any earlier floor; see [`FIRST_JOIN_SCOPE`].
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

/// `mls_kv` scope for the "entitled since" marker (#1571). Key: as
/// [`UNCONFIRMED_JOIN_SCOPE`]. Value: `"<RFC 3339 instant> <below_epoch>"`,
/// UTF-8.
///
/// Lets `messages::ingest::record_losses` tell a device registered before its
/// user joined (owed the gap before its first hold) from a new device; see
/// `.codesight/wiki/mls.md` → "A late join is not a new device".
const ENTITLED_SINCE_SCOPE: &str = "pollis_entitled_since";

/// Parse the two timestamp shapes the Delivery Service stores: RFC 3339
/// (client-stamped columns such as `dm_channel_member.added_at` and an
/// envelope's `sent_at`) and SQLite's `datetime('now')`, `YYYY-MM-DD HH:MM:SS`
/// in UTC (server-defaulted columns such as `user_device.created_at`). Answers
/// the start of the window the stamp names; see [`ds_instant_window`].
pub(crate) fn parse_ds_instant(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    ds_instant_window(raw).map(|(start, _)| start)
}

/// The window a stored stamp names: an instant for a stamp with sub-second
/// digits, the whole second for one without, because `datetime('now')`
/// truncates. Every comparison takes the side of the window that keeps an
/// undecidable case silent.
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

/// The pure rule behind [`record_entitlement`]: entitled only when the device
/// was registered strictly before its user became a member (a later device is a
/// new device, loss #2). Answers the end of the membership stamp's window, so
/// nothing sent in that second is claimed; a missing or unreadable input
/// answers `None` (a silent pass).
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

/// `mls_kv` scope marking a join as this device's first hold of a lineage
/// (#1571), consumed by [`record_entitlement`]. Value: empty. A re-added device
/// keeps #1524's rule that the gap "while you were out" was never sent to it.
const FRESH_JOIN_SCOPE: &str = "pollis_fresh_join";

/// Call before [`record_first_join`] / [`record_added`].
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

/// Consume the [`mark_fresh_if_first`] marker.
pub(crate) fn take_fresh(conn: &rusqlite::Connection, conversation_id: &str, generation: i64) -> bool {
    conn.execute(
        "DELETE FROM mls_kv WHERE scope = ?1 AND key = ?2",
        rusqlite::params![FRESH_JOIN_SCOPE, key(conversation_id, generation)],
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// See [`ENTITLED_SINCE_SCOPE`]. Called with the snapshot that admitted the
/// join; only a first hold qualifies (`fresh`, from [`take_fresh`]).
/// Best-effort: a missing marker only keeps a late join's gap silent.
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
    // Bound by this first hold's floor: a later re-add moves the floor but not
    // this bound, so the "while you were out" gap stays silent (#1524).
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

/// Entitled to envelopes sent at or after `since` and sealed below
/// `below_epoch` (the first hold).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entitlement {
    pub since: chrono::DateTime<chrono::Utc>,
    pub below_epoch: u64,
}

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

    #[test]
    fn only_a_device_registered_before_the_membership_is_entitled() {
        let since = "2026-10-20T01:49:21.000+00:00";
        assert!(entitlement(Some("2026-10-20 01:38:40"), Some(since)).is_some());
        assert_eq!(entitlement(Some("2026-10-20 01:50:00"), Some(since)), None);
        assert_eq!(entitlement(None, Some(since)), None);
        assert_eq!(entitlement(Some("2026-10-20 01:38:40"), None), None);
        assert_eq!(entitlement(Some("not a time"), Some(since)), None);
    }

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
