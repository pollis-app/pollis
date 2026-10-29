//! Load-bearing MLS control-plane decisions as small **pure** functions, called
//! by the real runtime paths and proved exhaustively by Kani
//! (`docs/machine-checked-correctness-design.md` §4.2–§4.4). The DS-side head
//! arithmetic is proved in `pollis-delivery::commit`.
//!
//! Kani discipline (see `messages/watermark.rs`): CBMC OOMs on heap allocation,
//! so harnesses use fixed-size stack arrays, tiny symbolic domains,
//! `#[kani::unwind(N)]`, and `#[kani::should_panic]` mutant harnesses.

/// I5, the recovery gate: this device is still `registered` (not revoked) AND
/// its user `is_member` of the group. The pure core of
/// `group_state::may_rejoin_via_external_join`; a revoked or removed device never
/// rejoins the tree.
pub fn may_rejoin(registered: bool, is_member: bool) -> bool {
    registered && is_member
}

/// What a device with NO local MLS group does when it finds pending commits for a
/// conversation it cannot yet open.
///
/// A fresh DM recipient has two ways in, its Welcome and external-join recovery,
/// and running both makes the device race its own Welcome and clobber the group.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JoinRecovery {
    /// An undelivered Welcome targets this conversation (or there is no
    /// GroupInfo yet): defer to the Welcome path.
    AwaitWelcome,
    /// No Welcome, and the device is a current registered member: external-join.
    ExternalJoin,
    /// No Welcome and the device may not rejoin (revoked or removed) — stay out.
    StayOut,
}

/// I6: decide the no-local-group recovery action; the pure core of the gate in
/// `group_state::process_pending_commits_locked_impl`.
///
/// **Welcome-first**, regardless of the rejoin gate: `apply_welcome` marks the
/// row delivered even on failure, so a stuck Welcome self-heals into the
/// external-join path on a later pass. Kani proves `ExternalJoin` ⟹
/// `!welcome_pending` and `ExternalJoin` ⟹ `group_info_available` (#832).
pub fn join_recovery(
    welcome_pending: bool,
    may_rejoin: bool,
    group_info_available: bool,
) -> JoinRecovery {
    if welcome_pending {
        JoinRecovery::AwaitWelcome
    } else if !group_info_available {
        // An external join builds on the published GroupInfo; without one it
        // cannot succeed, so wait for the Welcome. A later pass finds GroupInfo
        // (`heal_group_info`) if the Welcome never comes.
        JoinRecovery::AwaitWelcome
    } else if may_rejoin {
        JoinRecovery::ExternalJoin
    } else {
        JoinRecovery::StayOut
    }
}

/// The outcome of submitting a commit through the delivery seam. `LostRace` and
/// `Failed` are both ambiguous (a stale `LostRace` can be a retry of our own
/// accepted commit; a `Failed` can hide a landed one), so both are resolved
/// against the canonical log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubmitOutcome {
    Committed,
    LostRace,
    Failed,
}

/// Whether to ADOPT the locally-staged commit (merge it, advance the epoch) or
/// ROLL it back (clear the pending commit and converge on the winner).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    Adopt,
    Rollback,
}

/// I2 (#411): decide adopt-vs-rollback for a submitted own-commit, given the
/// exact bytes we staged (`ours`) and what the canonical log holds at this epoch.
///
/// Kani proves `Adopt` ⟹ `stored_at_epoch == Some(ours)` (no foreign adopt, no
/// fork) and `Rollback` ⟹ `stored_at_epoch != Some(ours)` (no wedge).
/// `Committed` adopts because the DS only returns it after writing our bytes.
pub fn resolve(outcome: SubmitOutcome, ours: &[u8], stored_at_epoch: Option<&[u8]>) -> Resolution {
    match outcome {
        SubmitOutcome::Committed => Resolution::Adopt,
        // Ambiguous: the canonical log is the arbiter.
        SubmitOutcome::LostRace | SubmitOutcome::Failed => {
            if stored_at_epoch == Some(ours) {
                Resolution::Adopt
            } else {
                Resolution::Rollback
            }
        }
    }
}

/// What the replay loop should do with the next commit row, given where the
/// local group currently is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReplayStep {
    /// The next row bridges `current_epoch → current_epoch + 1`: apply it.
    Apply,
    /// The bridging commit is missing while a higher epoch is present — a
    /// permanent gap. Drop the local group and recover via external-join.
    GapRecover,
    /// No further commit row to replay this pass (caught up).
    Wait,
}

/// I1: classify the next step of the commit-replay loop, given the local group's
/// epoch and the next `mls_commit_log` row's epoch (`epoch ASC`, `None` when out
/// of rows). Mirrors the gap detector in
/// `group_state::process_pending_commits_locked_impl`.
///
/// Kani proves replay never `Apply`s across a gap. Unlike the design sketch
/// (§4.2) it takes no `head`: the real detector does not consult one.
pub fn classify(current_epoch: u64, next_row_epoch: Option<u64>) -> ReplayStep {
    match next_row_epoch {
        None => ReplayStep::Wait,
        Some(e) if e == current_epoch => ReplayStep::Apply,
        // The bridging commit is missing while a higher epoch is present.
        Some(_) => ReplayStep::GapRecover,
    }
}

/// What a replay pass does when the next commit row does not bridge the local
/// group's epoch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GapRecovery {
    /// The server DECLARED why the bridging commit is absent (a retention prune
    /// above us, or a hole found inside the snapshot transaction): rebuild by
    /// external join.
    Rebuild,
    /// Non-contiguous with no declared explanation: hold position and retry.
    HoldPosition,
}

/// Decide whether a gap at `current_epoch` licenses DESTROYING this device's MLS
/// crypto state (#1161 M6).
///
/// Deleting the group loses every un-ingested envelope on this lineage, so only
/// an explanation from the same read transaction that covers our epoch licenses
/// it: `pruned_floor > current_epoch`, or `hole_expected == current_epoch`.
/// Anything else holds position, which deletes nothing; a real loss rebuilds on
/// a later pass.
pub fn gap_recovery(
    current_epoch: u64,
    hole_expected: Option<u64>,
    pruned_floor: Option<u64>,
) -> GapRecovery {
    if pruned_floor.is_some_and(|floor| floor > current_epoch) {
        return GapRecovery::Rebuild;
    }
    if hole_expected == Some(current_epoch) {
        return GapRecovery::Rebuild;
    }
    GapRecovery::HoldPosition
}

/// The standing of the leaf that SIGNED a GroupInfo:
/// [`super::device::LeafVerdict`] without payloads, so this decision stays pure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SignerStanding {
    /// The signing leaf is the claimed user's certified, unrevoked device key.
    Certified,
    /// The inputs to decide are missing.
    Unverifiable,
    /// The inputs are present and the leaf is NOT that user's device.
    Uncertified,
}

/// Whether a published GroupInfo may be adopted (external-joined) as a lineage
/// of this conversation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineageAdoption {
    Adopt,
    Refuse,
}

/// I7: may the device that signed a GroupInfo have founded the lineage we are
/// about to adopt (#1161 M6 residual)?
///
/// A `GroupId` is a name, not a capability; a DS holds no account identity key,
/// so it cannot produce a leaf the roster's cross-signing certifies.
/// `signer_on_roster` is `None` when the roster could not be read.
///
/// Refuses only on POSITIVE evidence (`Uncertified`, or a signer demonstrably
/// off the roster): refusing on `Unverifiable`, an ordinary race on a legitimate
/// migration, would strand real users on the old lineage.
pub fn lineage_adoption(
    signer_on_roster: Option<bool>,
    standing: SignerStanding,
) -> LineageAdoption {
    if matches!(standing, SignerStanding::Uncertified) {
        return LineageAdoption::Refuse;
    }
    if signer_on_roster == Some(false) {
        return LineageAdoption::Refuse;
    }
    LineageAdoption::Adopt
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Symbolic bytes over `0..=1` in a FIXED-SIZE stack array (a `Vec` OOMs CBMC).
    fn symbolic_bytes<const N: usize>() -> [u8; N] {
        let mut a = [0u8; N];
        for b in a.iter_mut() {
            let v: u8 = kani::any();
            kani::assume(v <= 1);
            *b = v;
        }
        a
    }

    impl kani::Arbitrary for SubmitOutcome {
        fn any() -> Self {
            match kani::any::<u8>() % 3 {
                0 => SubmitOutcome::Committed,
                1 => SubmitOutcome::LostRace,
                _ => SubmitOutcome::Failed,
            }
        }
    }

    /// I5: a revoked OR removed device NEVER rejoins (exhaustive 2-bit table).
    #[kani::proof]
    fn i5_gate_never_leaks() {
        let registered: bool = kani::any();
        let is_member: bool = kani::any();

        let allowed = may_rejoin(registered, is_member);

        if !registered || !is_member {
            assert!(!allowed);
        }
        if allowed {
            assert!(registered && is_member);
        }
    }

    /// Negative harness: an OR gate fails open. `should_panic` passes exactly
    /// when Kani finds the leak, certifying the harness has teeth.
    fn may_rejoin_mutant(registered: bool, is_member: bool) -> bool {
        // BUG: OR fails open.
        registered || is_member
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i5_mutant_refuted() {
        let registered: bool = kani::any();
        let is_member: bool = kani::any();

        let allowed = may_rejoin_mutant(registered, is_member);

        if !registered || !is_member {
            assert!(!allowed);
        }
    }

    /// I6: never external-join with a Welcome pending or with no GroupInfo.
    #[kani::proof]
    fn i6_welcome_first_never_double_joins() {
        let welcome_pending: bool = kani::any();
        let may_rejoin: bool = kani::any();
        let group_info_available: bool = kani::any();

        match join_recovery(welcome_pending, may_rejoin, group_info_available) {
            JoinRecovery::ExternalJoin => {
                assert!(!welcome_pending);
                assert!(may_rejoin);
                assert!(group_info_available);
            }
            JoinRecovery::AwaitWelcome => assert!(welcome_pending || !group_info_available),
            JoinRecovery::StayOut => {
                assert!(!welcome_pending);
                assert!(group_info_available);
                assert!(!may_rejoin);
            }
        }
    }

    /// Negative harness: external-joins whenever the rejoin gate passes, ignoring
    /// a pending Welcome and GroupInfo availability (#832).
    fn join_recovery_mutant(
        welcome_pending: bool,
        may_rejoin: bool,
        _group_info_available: bool,
    ) -> JoinRecovery {
        // BUG: rejoin gate checked before the Welcome; GroupInfo ignored.
        if may_rejoin {
            JoinRecovery::ExternalJoin
        } else if welcome_pending {
            JoinRecovery::AwaitWelcome
        } else {
            JoinRecovery::StayOut
        }
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i6_mutant_refuted() {
        let welcome_pending: bool = kani::any();
        let may_rejoin: bool = kani::any();

        let group_info_available: bool = kani::any();
        if let JoinRecovery::ExternalJoin =
            join_recovery_mutant(welcome_pending, may_rejoin, group_info_available)
        {
            assert!(!welcome_pending);
            assert!(group_info_available);
        }
    }

    // I2: small fixed-size byte arrays (N = 3, bytes 0..=1).
    const N: usize = 3;

    /// I2: `resolve` never adopts a foreign commit and never rolls back our own.
    #[kani::proof]
    #[kani::unwind(4)]
    fn i2_resolve_sound() {
        let ours: [u8; N] = symbolic_bytes();
        let stored_bytes: [u8; N] = symbolic_bytes();
        let has_stored: bool = kani::any();
        let stored: Option<&[u8]> = if has_stored {
            Some(&stored_bytes[..])
        } else {
            None
        };
        let outcome: SubmitOutcome = kani::any();

        // DS contract: `Committed` means the DS wrote OUR bytes at this epoch.
        if outcome == SubmitOutcome::Committed {
            kani::assume(stored == Some(&ours[..]));
        }

        match resolve(outcome, &ours[..], stored) {
            Resolution::Adopt => assert!(stored == Some(&ours[..])),
            Resolution::Rollback => assert!(stored != Some(&ours[..])),
        }
    }

    /// Negative harness: adopts an ambiguous outcome without checking the log.
    fn resolve_mutant(outcome: SubmitOutcome, _ours: &[u8], _stored: Option<&[u8]>) -> Resolution {
        match outcome {
            SubmitOutcome::Committed => Resolution::Adopt,
            // BUG: adopts LostRace/Failed unconditionally (#411).
            SubmitOutcome::LostRace | SubmitOutcome::Failed => Resolution::Adopt,
        }
    }

    #[kani::proof]
    #[kani::should_panic]
    #[kani::unwind(4)]
    fn i2_mutant_refuted() {
        let ours: [u8; N] = symbolic_bytes();
        let stored_bytes: [u8; N] = symbolic_bytes();
        let has_stored: bool = kani::any();
        let stored: Option<&[u8]> = if has_stored {
            Some(&stored_bytes[..])
        } else {
            None
        };
        let outcome: SubmitOutcome = kani::any();

        if outcome == SubmitOutcome::Committed {
            kani::assume(stored == Some(&ours[..]));
        }

        if let Resolution::Adopt = resolve_mutant(outcome, &ours[..], stored) {
            assert!(stored == Some(&ours[..]));
        }
    }

    fn symbolic_epoch_opt() -> Option<u64> {
        if kani::any() {
            let e: u64 = kani::any();
            kani::assume(e <= 3);
            Some(e)
        } else {
            None
        }
    }

    /// I1: `Apply` ⟺ the next row's epoch is exactly `current_epoch`.
    #[kani::proof]
    fn i1_classify_no_gap_apply() {
        let current: u64 = kani::any();
        kani::assume(current <= 3);
        let next = symbolic_epoch_opt();

        match classify(current, next) {
            ReplayStep::Apply => assert!(next == Some(current)),
            ReplayStep::GapRecover => {
                assert!(next.is_some());
                assert!(next != Some(current));
            }
            ReplayStep::Wait => assert!(next.is_none()),
        }
    }

    /// Negative harness: applies any present row, even across a gap.
    fn classify_mutant(current_epoch: u64, next_row_epoch: Option<u64>) -> ReplayStep {
        match next_row_epoch {
            None => ReplayStep::Wait,
            // BUG: applies regardless of whether the row bridges the epoch.
            Some(_) => {
                let _ = current_epoch;
                ReplayStep::Apply
            }
        }
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i1_mutant_refuted() {
        let current: u64 = kani::any();
        kani::assume(current <= 3);
        let next = symbolic_epoch_opt();

        if let ReplayStep::Apply = classify_mutant(current, next) {
            assert!(next == Some(current));
        }
    }
}

// Plain tests of the same predicates, since Kani does not run under `cargo test`.
#[cfg(test)]
mod tests {
    use super::*;

    /// I6: a pending Welcome always wins and external-join never runs with one.
    #[test]
    fn join_recovery_welcome_first_never_double_joins() {
        assert_eq!(join_recovery(true, true, true), JoinRecovery::AwaitWelcome);
        assert_eq!(join_recovery(true, false, true), JoinRecovery::AwaitWelcome);

        assert_eq!(join_recovery(false, true, true), JoinRecovery::ExternalJoin);
        assert_eq!(join_recovery(false, false, true), JoinRecovery::StayOut);

        for welcome_pending in [false, true] {
            for may_rejoin in [false, true] {
                for group_info in [false, true] {
                    if join_recovery(welcome_pending, may_rejoin, group_info)
                        == JoinRecovery::ExternalJoin
                    {
                        assert!(
                            !welcome_pending,
                            "external-join must never run while a Welcome is pending"
                        );
                    }
                }
            }
        }
    }

    /// #832: a member with no Welcome and no published GroupInfo must wait for
    /// the Welcome, not retry an external join that cannot succeed.
    #[test]
    fn no_group_info_never_selects_an_impossible_external_join() {
        assert_eq!(
            join_recovery(false, true, false),
            JoinRecovery::AwaitWelcome,
            "a member with no published GroupInfo must await the Welcome, not \
             external-join onto a group that has nothing to join onto"
        );

        // Even a device that may not rejoin waits: without GroupInfo we cannot
        // tell "removed" from "not yet published", and StayOut gives up for good.
        assert_eq!(
            join_recovery(false, false, false),
            JoinRecovery::AwaitWelcome
        );

        for welcome_pending in [false, true] {
            for may_rejoin in [false, true] {
                for group_info in [false, true] {
                    if join_recovery(welcome_pending, may_rejoin, group_info)
                        == JoinRecovery::ExternalJoin
                    {
                        assert!(
                            group_info,
                            "external-join must never be selected without a GroupInfo to join onto"
                        );
                    }
                }
            }
        }
    }

    /// `Rebuild` implies an explanation covering the missing epoch.
    #[test]
    fn a_gap_destroys_state_only_when_the_server_explained_it() {
        for current in 0u64..4 {
            for hole in [None, Some(0), Some(1), Some(2), Some(3)] {
                for floor in [None, Some(0), Some(1), Some(2), Some(3)] {
                    if gap_recovery(current, hole, floor) == GapRecovery::Rebuild {
                        assert!(
                            hole == Some(current) || floor.is_some_and(|f| f > current),
                            "rebuilt at {current} on hole={hole:?} floor={floor:?} with no \
                             explanation covering the missing epoch"
                        );
                    }
                }
            }
        }
    }

    /// An unexplained non-contiguous batch must not be able to delete anything.
    #[test]
    fn an_unexplained_gap_holds_position() {
        assert_eq!(gap_recovery(7, None, None), GapRecovery::HoldPosition);
        assert_eq!(gap_recovery(7, Some(9), None), GapRecovery::HoldPosition);
        assert_eq!(gap_recovery(7, None, Some(7)), GapRecovery::HoldPosition);
    }

    #[test]
    fn a_declared_prune_or_hole_still_rebuilds() {
        assert_eq!(gap_recovery(7, Some(7), None), GapRecovery::Rebuild);
        assert_eq!(gap_recovery(7, None, Some(8)), GapRecovery::Rebuild);
    }

    /// #1161 M6 residual: the whole truth table of [`lineage_adoption`], so a
    /// change that refuses on `Unverifiable` fails here.
    #[test]
    fn a_lineage_is_refused_only_on_positive_evidence() {
        for roster in [None, Some(true), Some(false)] {
            for standing in [
                SignerStanding::Certified,
                SignerStanding::Unverifiable,
                SignerStanding::Uncertified,
            ] {
                let expected = if matches!(standing, SignerStanding::Uncertified)
                    || roster == Some(false)
                {
                    LineageAdoption::Refuse
                } else {
                    LineageAdoption::Adopt
                };
                assert_eq!(
                    lineage_adoption(roster, standing),
                    expected,
                    "roster={roster:?} standing={standing:?}"
                );
            }
        }
    }

    /// The failure bias: unverifiable or unreadable-roster signers still adopt.
    #[test]
    fn an_unverifiable_signer_does_not_block_a_migration() {
        assert_eq!(
            lineage_adoption(Some(true), SignerStanding::Unverifiable),
            LineageAdoption::Adopt
        );
        assert_eq!(
            lineage_adoption(None, SignerStanding::Unverifiable),
            LineageAdoption::Adopt
        );
        assert_eq!(
            lineage_adoption(None, SignerStanding::Certified),
            LineageAdoption::Adopt
        );
    }
}
