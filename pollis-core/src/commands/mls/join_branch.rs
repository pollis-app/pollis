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

/// The epoch a successor lineage opens at: the migration's one commit moves
/// every roster device in by a Welcome at this epoch (#454 P4, "move everyone
/// or nobody").
pub(crate) const SUCCESSOR_OPENING_EPOCH: u64 = 1;

/// `mls_kv` scope recording the gap during which this device was out of a
/// lineage's tree (#1765, #1772). Key: as [`UNCONFIRMED_JOIN_SCOPE`]. Value:
/// one `from`, `until` pair (8 bytes LE each, `until` [`OPEN`] until the device
/// is back in) per stint out of the tree, oldest first.
///
/// Nothing sealed in `[from, until)` was sent to this device, so an envelope
/// there that it never reached is not its loss (#1524). `from` comes only from
/// this device's own view of the lineage, never from the DS:
///
/// * the replay reached the commit that removed it: openmls staged it as
///   removing our leaf, or refused the next commit with `UseAfterEviction`.
///   `from` is the epoch the removing commit produced. The DS keeps a removed
///   device's commits from its last report on (`departed_devices` in
///   `pollis-delivery`), so a re-added device replays to that commit before
///   its Welcome replaces the group, or before it adopts a successor lineage;
/// * the user left: `from` is the epoch the device held when it left.
///
/// A device that lost its group without seeing its removal (a retention
/// rebuild over a pruned log, a fork recovery) records no gap: it does not know
/// where the removal was, so everything it never reached from its last held
/// epoch on stays a loss ([`note_unwitnessed_exit`]).
///
/// `until` is the device's own join, narrowed (never widened) by the DS's
/// `added_device_ids` hint, so a lying hint can only show more.
const LEFT_TREE_SCOPE: &str = "pollis_left_tree";

/// A recorded gap: `[from, until)`, open-ended while `until` is `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LeftGap {
    pub from: u64,
    pub until: Option<u64>,
}

impl LeftGap {
    /// Was an envelope sealed at `epoch` sent while this device was out?
    pub(crate) fn covers(self, epoch: u64) -> bool {
        epoch >= self.from && self.until.is_none_or(|u| epoch < u)
    }
}

/// The most gaps kept per lineage; the oldest goes first. Dropping one only
/// makes an envelope from that stint that is still unread show as lost.
const MAX_GAPS: usize = 32;

/// `until` of an open gap in the stored form. #1765's single-gap layout stored
/// an open gap as `from` alone and a closed one as `from, until`, so its
/// records read back unchanged, and an open gap stored here reads as open there.
const OPEN: u64 = u64::MAX;

fn write_gaps(conn: &rusqlite::Connection, conversation_id: &str, generation: i64, gaps: &[LeftGap]) {
    let start = gaps.len().saturating_sub(MAX_GAPS);
    let mut value = Vec::with_capacity(16 * (gaps.len() - start));
    for gap in &gaps[start..] {
        value.extend_from_slice(&gap.from.to_le_bytes());
        value.extend_from_slice(&gap.until.unwrap_or(OPEN).to_le_bytes());
    }
    if let Err(e) = conn.execute(
        "INSERT OR REPLACE INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
        rusqlite::params![LEFT_TREE_SCOPE, key(conversation_id, generation), value],
    ) {
        crate::diag!(
            "[mls] could not record leaving {conversation_id} generation {generation}: {e}"
        );
    }
}

/// Every gap recorded for `(conversation_id, generation)`, oldest first. A
/// device removed, re-added and removed again has one per stint (#1772): it
/// may not have read past the first when the second is recorded.
pub(crate) fn left_gaps(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
) -> Vec<LeftGap> {
    use rusqlite::OptionalExtension;
    let Some(raw): Option<Vec<u8>> = conn
        .query_row(
            "SELECT value FROM mls_kv WHERE scope = ?1 AND key = ?2",
            rusqlite::params![LEFT_TREE_SCOPE, key(conversation_id, generation)],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()
    else {
        return Vec::new();
    };
    let word = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap_or([0; 8]));
    if raw.len() == 8 {
        return vec![LeftGap { from: word(&raw), until: None }];
    }
    raw.chunks_exact(16)
        .map(|r| {
            let until = word(&r[8..]);
            LeftGap { from: word(&r[..8]), until: (until != OPEN).then_some(until) }
        })
        .collect()
}

/// The most recent gap recorded for `(conversation_id, generation)`, if any.
pub(crate) fn left_gap(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
) -> Option<LeftGap> {
    left_gaps(conn, conversation_id, generation).pop()
}

/// Was an envelope sealed at `epoch` of `generation` sent while this device
/// was out of that lineage's tree, by any recorded gap?
pub(crate) fn in_left_gap(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    epoch: u64,
) -> bool {
    left_gaps(conn, conversation_id, generation)
        .iter()
        .any(|gap| gap.covers(epoch))
}

/// Record that this device left `(conversation_id, generation)`'s tree at
/// `from`, with `readded_at` (the DS hint) as a provisional end. A second
/// record of the same removal (every replay that reaches it records it) only
/// narrows that gap. Best-effort.
pub(crate) fn mark_left(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    from: u64,
    readded_at: Option<u64>,
) {
    let hinted = readded_at.map(|r| r.max(from));
    let mut gaps = left_gaps(conn, conversation_id, generation);
    match gaps.iter_mut().find(|gap| gap.from == from) {
        Some(gap) => {
            gap.until = match (gap.until, hinted) {
                (Some(until), Some(hint)) => Some(until.min(hint)),
                (until, None) => until,
                (None, hint) => hint,
            };
        }
        None => gaps.push(LeftGap { from, until: hinted }),
    }
    write_gaps(conn, conversation_id, generation, &gaps);
}

/// The device is back in the lineage at `join_epoch`: close an open gap there,
/// or narrow one the hint left wider. Never widens one.
pub(crate) fn close_left(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    join_epoch: u64,
) {
    let mut gaps = left_gaps(conn, conversation_id, generation);
    if gaps.is_empty() {
        return;
    }
    for gap in gaps.iter_mut().filter(|gap| gap.from <= join_epoch) {
        gap.until = Some(gap.until.map_or(join_epoch, |u| u.min(join_epoch)).max(gap.from));
    }
    write_gaps(conn, conversation_id, generation, &gaps);
}

/// `mls_kv` scope marking that this device dropped its group of a lineage
/// without seeing its own removal (#1772): a retention rebuild over a pruned
/// log, or any other recovery that deletes the group. Key: as
/// [`UNCONFIRMED_JOIN_SCOPE`]. Value: the epoch it held (8 bytes LE; a record
/// written by #1765's build carries more after it, which is ignored).
///
/// The device may have been removed somewhere after that epoch, but it cannot
/// tell where, so the Welcome that brings it back must not pass the gap as
/// time out of the tree ([`record_welcome_join`]).
const REBUILD_SCOPE: &str = "pollis_rebuild_pending";

/// Mark that this device dropped its group of `generation` at `held` without
/// seeing its removal. The first mark stands until a join consumes it.
pub(crate) fn note_unwitnessed_exit(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    held: u64,
) {
    if let Err(e) = conn.execute(
        "INSERT OR IGNORE INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
        rusqlite::params![REBUILD_SCOPE, key(conversation_id, generation), held.to_le_bytes().to_vec()],
    ) {
        crate::diag!("[mls] could not record dropping {conversation_id}: {e}");
    }
}

/// Consume the [`note_unwitnessed_exit`] mark, answering the epoch it held.
pub(crate) fn take_unwitnessed_exit(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
) -> Option<u64> {
    use rusqlite::OptionalExtension;
    let raw: Vec<u8> = conn
        .query_row(
            "SELECT value FROM mls_kv WHERE scope = ?1 AND key = ?2",
            rusqlite::params![REBUILD_SCOPE, key(conversation_id, generation)],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()?;
    let _ = conn.execute(
        "DELETE FROM mls_kv WHERE scope = ?1 AND key = ?2",
        rusqlite::params![REBUILD_SCOPE, key(conversation_id, generation)],
    );
    Some(u64::from_le_bytes(raw.get(..8)?.try_into().ok()?))
}

/// Record the floor of a Welcome that added this device to `generation` at
/// `welcome_epoch`, and close any gap there.
///
/// A first hold, or a re-add after a proven exit, keeps #1312's rule: nothing
/// sealed before the Welcome was sent to it. A re-add over an earlier stint
/// keeps that stint's floor instead (#1772), so only a proven gap passes:
///
/// * `replaced_held`: the Welcome replaced a group the device still held. It
///   never saw its removal, which may lie anywhere above that group's epoch;
/// * an unwitnessed exit ([`note_unwitnessed_exit`]): the same, for a group
///   already dropped;
/// * a recorded gap: the floor would otherwise hide what the device never
///   reached before the removal.
pub(crate) fn record_welcome_join(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
    welcome_epoch: u64,
    replaced_held: bool,
) {
    let unwitnessed = take_unwitnessed_exit(conn, conversation_id, generation).is_some();
    let proven = !left_gaps(conn, conversation_id, generation).is_empty();
    if replaced_held || unwitnessed || proven {
        record_first_join(conn, conversation_id, generation, welcome_epoch);
    } else {
        record_added(conn, conversation_id, generation, welcome_epoch);
    }
    close_left(conn, conversation_id, generation, welcome_epoch);
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

    /// #1765: a rejoin closes the gap at the join; the DS hint may only narrow it.
    #[test]
    fn a_rejoin_closes_the_gap_and_the_hint_only_narrows_it() {
        let conn = kv();
        mark_left(&conn, "c", 1, 20, None);
        close_left(&conn, "c", 1, 28);
        let gap = left_gap(&conn, "c", 1).unwrap();
        assert!(!gap.covers(19), "before the removal: a loss");
        assert!(gap.covers(20) && gap.covers(27));
        assert!(!gap.covers(28), "after the rejoin: a loss");
        mark_left(&conn, "c", 1, 20, Some(25));
        close_left(&conn, "c", 1, 28);
        assert_eq!(left_gap(&conn, "c", 1).unwrap().until, Some(25), "narrowed by the hint");
        mark_left(&conn, "c", 1, 20, Some(90));
        close_left(&conn, "c", 1, 28);
        assert_eq!(left_gap(&conn, "c", 1).unwrap().until, Some(25), "a lying hint cannot widen it");
        mark_left(&conn, "c", 1, 20, Some(3));
        assert_eq!(left_gap(&conn, "c", 1).unwrap().until, Some(20), "nor reach below the removal");
        assert_eq!(left_gaps(&conn, "c", 1).len(), 1, "the same removal, recorded again");

        mark_left(&conn, "d", 1, 30, Some(90));
        close_left(&conn, "d", 1, 34);
        assert_eq!(left_gap(&conn, "d", 1).unwrap().until, Some(34), "the join ends it first");
    }

    /// #1772: a device removed, re-added and removed again keeps both gaps, so
    /// what it had not yet read from the first stays silent.
    #[test]
    fn every_stint_keeps_its_own_gap() {
        let conn = kv();
        mark_left(&conn, "c", 1, 65, Some(79));
        close_left(&conn, "c", 1, 80);
        mark_left(&conn, "c", 1, 81, None);
        close_left(&conn, "c", 1, 82);
        assert_eq!(
            left_gaps(&conn, "c", 1),
            vec![LeftGap { from: 65, until: Some(79) }, LeftGap { from: 81, until: Some(82) }]
        );
        assert!(in_left_gap(&conn, "c", 1, 67), "the first stint's gap still passes");
        assert!(!in_left_gap(&conn, "c", 1, 79) && !in_left_gap(&conn, "c", 1, 80));
        assert!(in_left_gap(&conn, "c", 1, 81));
        assert!(!in_left_gap(&conn, "c", 1, 64) && !in_left_gap(&conn, "c", 1, 82));

        for stint in 0..(MAX_GAPS as u64 + 4) {
            mark_left(&conn, "d", 0, 10 * stint, None);
            close_left(&conn, "d", 0, 10 * stint + 5);
        }
        let kept = left_gaps(&conn, "d", 0);
        assert_eq!(kept.len(), MAX_GAPS, "bounded");
        assert_eq!(kept[0].from, 40, "the oldest go first");
    }

    /// #1772: #1765's single-gap records read back unchanged, and an open gap
    /// written now reads as open in that layout too.
    #[test]
    fn a_single_gap_record_from_1765_still_reads() {
        let conn = kv();
        let put = |value: Vec<u8>| {
            conn.execute(
                "INSERT OR REPLACE INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
                rusqlite::params![LEFT_TREE_SCOPE, key("c", 0), value],
            )
            .unwrap();
        };
        put(7u64.to_le_bytes().to_vec());
        assert_eq!(left_gaps(&conn, "c", 0), vec![LeftGap { from: 7, until: None }]);
        let mut closed = 7u64.to_le_bytes().to_vec();
        closed.extend_from_slice(&12u64.to_le_bytes());
        put(closed);
        assert_eq!(left_gaps(&conn, "c", 0), vec![LeftGap { from: 7, until: Some(12) }]);

        mark_left(&conn, "e", 0, 9, None);
        let raw: Vec<u8> = conn
            .query_row(
                "SELECT value FROM mls_kv WHERE scope = ?1 AND key = ?2",
                rusqlite::params![LEFT_TREE_SCOPE, key("e", 0)],
                |row| row.get(0),
            )
            .unwrap();
        let until = u64::from_le_bytes(raw[8..16].try_into().unwrap());
        assert!(LeftGap { from: 9, until: Some(until) }.covers(u64::MAX - 1), "open, as #1765 reads it");
    }

    /// #1772: a first Welcome sets the floor; a re-add after a stint keeps the
    /// first floor, so only a proven gap passes.
    #[test]
    fn a_welcome_moves_the_floor_only_for_a_first_hold() {
        let conn = kv();
        record_welcome_join(&conn, "c", 0, 4, false);
        assert_eq!(first_join(&conn, "c", 0), Some(4), "a first hold");

        // A plain re-add (no stint the device is aware of): #1312's rule.
        record_welcome_join(&conn, "c", 0, 9, false);
        assert_eq!(first_join(&conn, "c", 0), Some(9));

        // Replacing a group it still held: it never saw its removal.
        record_welcome_join(&conn, "d", 0, 2, false);
        record_welcome_join(&conn, "d", 0, 11, true);
        assert_eq!(first_join(&conn, "d", 0), Some(2), "the stint's floor stays");
        assert_eq!(left_gap(&conn, "d", 0), None, "and no gap: everything unread shows");

        // A group dropped without seeing the removal, then a Welcome.
        record_welcome_join(&conn, "e", 0, 2, false);
        note_unwitnessed_exit(&conn, "e", 0, 6);
        note_unwitnessed_exit(&conn, "e", 0, 8);
        record_welcome_join(&conn, "e", 0, 12, false);
        assert_eq!(first_join(&conn, "e", 0), Some(2));
        assert_eq!(take_unwitnessed_exit(&conn, "e", 0), None, "consumed by the join");

        // A proven removal at 7: the floor stays and only [7, 12) passes.
        record_welcome_join(&conn, "f", 0, 2, false);
        mark_left(&conn, "f", 0, 7, None);
        record_welcome_join(&conn, "f", 0, 12, false);
        assert_eq!(first_join(&conn, "f", 0), Some(2));
        let gap = left_gap(&conn, "f", 0).unwrap();
        assert_eq!(gap, LeftGap { from: 7, until: Some(12) });
        assert!(!gap.covers(6), "sealed while it was in: a loss");
    }

    /// #1772: the first unwitnessed exit stands; the mark is read once.
    #[test]
    fn an_unwitnessed_exit_keeps_the_first_epoch() {
        let conn = kv();
        note_unwitnessed_exit(&conn, "c", 1, 13);
        note_unwitnessed_exit(&conn, "c", 1, 20);
        assert_eq!(take_unwitnessed_exit(&conn, "c", 1), Some(13));
        assert_eq!(take_unwitnessed_exit(&conn, "c", 1), None);
        // A record from #1765's build: the held epoch, then fields since dropped.
        let mut legacy = 21u64.to_le_bytes().to_vec();
        legacy.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 9, 9]);
        conn.execute(
            "INSERT INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![REBUILD_SCOPE, key("c", 1), legacy],
        )
        .unwrap();
        assert_eq!(take_unwitnessed_exit(&conn, "c", 1), Some(21));
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
