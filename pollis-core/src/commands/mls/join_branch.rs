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
        crate::diag!(
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

/// `mls_kv` scope for the epoch from which this device is entitled to read each
/// lineage (#1312). Key: as [`UNCONFIRMED_JOIN_SCOPE`]. Value: the epoch,
/// little-endian.
///
/// It separates the one loss this device was never entitled to avoid — a
/// message sealed while it was not in the tree — from every other. Two ways in
/// set it, deliberately differently:
///
/// * **Added** (a Welcome, [`record_added`]): someone put this device in the
///   tree at that epoch. Whatever was sealed before it — first join, or the gap
///   while it was removed and before it was re-added — it was never sent to
///   this device. Replaces any earlier value.
/// * **Held** (creating the group, an external join, [`record_first_join`]):
///   only the FIRST is kept. A group rebuilt by recovery keeps the original
///   floor, because what the device missed in between was sent while it was a
///   member and is a real loss.
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
/// [`FIRST_JOIN_SCOPE`]'s floor says what this device never HELD; it cannot say
/// whether that was an accepted loss. For a NEW device it is (loss #2), and for
/// anything sent before the USER joined it is (loss #1). But a device that was
/// already registered when its user became a member here was supposed to be in
/// the tree all along: if it first holds the group later — a peer's add skipped
/// it, its Welcome failed to apply, nothing re-added it, and it external-joined
/// at the head of the log — what was sent in between is a real loss and must be
/// shown, not passed silently. This marker records that the device was in that
/// position, and from when (the user's membership), so the ingest's loss check
/// can tell the two apart (`messages::ingest::record_losses`).
const ENTITLED_SINCE_SCOPE: &str = "pollis_entitled_since";

/// Parse the two timestamp shapes the Delivery Service stores: RFC 3339
/// (client-stamped columns such as `dm_channel_member.added_at` and an
/// envelope's `sent_at`) and SQLite's `datetime('now')`, `YYYY-MM-DD HH:MM:SS`
/// in UTC (server-defaulted columns such as `user_device.created_at` and
/// `group_member.joined_at`). Answers the START of the window the stamp names;
/// see [`ds_instant_window`].
pub(crate) fn parse_ds_instant(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    ds_instant_window(raw).map(|(start, _)| start)
}

/// The window a stored stamp names: an instant for a stamp with sub-second
/// digits, the whole second for one without. `datetime('now')` truncates, so
/// "01:49:21" means "some time during that second", and comparing it as the
/// instant 01:49:21.000 would read an event later in the same second as
/// earlier than it. Every comparison below takes the side of the window that
/// keeps an undecidable case silent (the pre-#1571 behaviour).
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

/// The instant from which this device was entitled to the conversation's
/// messages despite not holding its group, or `None` when it was not: the pure
/// rule behind [`record_entitlement`]. Entitled only when the device was
/// registered strictly BEFORE its user became a member (the whole window of the
/// registration stamp ends by the start of the membership's); a device
/// registered after is a new device (loss #2) and gets nothing earlier than its
/// own join. The answer is the END of the membership stamp's window, so nothing
/// sent in the second the membership began is claimed for it. Either input
/// missing or unreadable answers `None`: the pre-#1571 behaviour, a silent
/// pass.
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
/// The entitlement is only for a device that never held the lineage — the
/// incident's shape. A device that held it, was evicted while its user stayed a
/// member, and was re-added keeps #1524's rule that the gap "while you were
/// out" was never sent to it; a recovery rebuild keeps its original floor.
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
/// Called with the snapshot that admitted the join (an external join, or the
/// corroboration read behind a Welcome). Only a FIRST hold of the lineage
/// qualifies (`fresh`, from [`take_fresh`]). Best-effort: a missing marker only
/// keeps a late join's gap silent, as it was before #1571.
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
    // The floor this first hold just recorded. The entitlement covers only what
    // was sealed BELOW it — the gap before the device ever held the lineage.
    // A later removal and re-add moves the floor (`record_added`) but not this
    // bound, so the "while you were out" gap stays #1524's silent pass rather
    // than reading as owed to the device on the strength of a membership that
    // has since ended.
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

    /// #1571: only a device registered BEFORE its user's membership is entitled
    /// to a late join's gap; a newer device (loss #2) and an unreported input
    /// stay silent. Both stored timestamp shapes are read.
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

    /// A rebuild (external join) keeps the first floor — what the device missed
    /// in between is a real loss — but being ADDED again (a Welcome after a
    /// removal) moves it: the gap was never sent to this device.
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
