//! Load-bearing MLS control-plane decisions, extracted as small **pure**
//! functions so they can be (1) called by the real runtime paths and (2) proved
//! exhaustively by Kani over symbolic inputs.
//!
//! The M2 tranche (`docs/machine-checked-correctness-design.md` §4.2–§4.4):
//!
//! * [`may_rejoin`] — I5, the recovery gate (§4.4): a revoked *or* removed device
//!   never takes an external-join recovery path back into the tree.
//! * [`resolve`] — I2, own-commit canonicalization (§4.3, the #411 property): the
//!   adopt-vs-rollback decision never adopts a foreign commit and never rolls
//!   back our own landed commit.
//! * [`classify`] — I1, client-side gap detection (§4.2): replay never `Apply`s a
//!   commit across an epoch gap.
//!
//! The DS-side head arithmetic is proved in `pollis-delivery::commit`.
//!
//! ## Kani discipline (learned in M1 — see `messages/watermark.rs`)
//!
//! CBMC OOMs on heap allocation, so harnesses use fixed-size arrays, tiny domains and bounded unwinds.

/// Both cooperative gates on the external-join *recovery* paths, as a pure
/// predicate: this device is still `registered` (not revoked) AND its user
/// `is_member` of the group. A `false` from either means "do not rebuild/rejoin".
pub fn may_rejoin(registered: bool, is_member: bool) -> bool {
    registered && is_member
}

/// What a device with NO local MLS group does when it finds pending commits for a
/// conversation it cannot yet open.
///
/// A fresh DM recipient has two ways in (Welcome, external join); running both races its own Welcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JoinRecovery {
    /// An undelivered Welcome for this device targets this conversation — defer to
    /// the Welcome path; do NOT external-join (that would race our own Welcome).
    AwaitWelcome,
    /// No Welcome available and the device is a current, registered member —
    /// external-join is the genuine recovery (Secret-Key recovery / dropped Welcome).
    ExternalJoin,
    /// No Welcome and the device may not rejoin (revoked or removed) — stay out.
    StayOut,
}

/// Decide the no-local-group recovery action from the three guards the runtime
/// evaluates: `welcome_pending` (an undelivered Welcome targets this device for
/// this conversation), `may_rejoin` (the I5 gate: registered ∧ member), and
/// `group_info_available` (the group has a published GroupInfo to join onto).
///
/// **Welcome-first**: a pending Welcome always wins; a failed one is acked, so later passes fall through to the I5 gate.
///
/// The Kani harness proves two load-bearing properties:
///
/// * **`ExternalJoin` ⟹ `!welcome_pending`**
/// * **`ExternalJoin` ⟹ `group_info_available`** (#832)
pub fn join_recovery(
    welcome_pending: bool,
    may_rejoin: bool,
    group_info_available: bool,
) -> JoinRecovery {
    if welcome_pending {
        JoinRecovery::AwaitWelcome
    } else if !group_info_available {
        // An external join needs a published GroupInfo; without one only a Welcome can get us in, so wait.
        //
        // If none comes, a later pass finds GroupInfo (via `heal_group_info`) and joins then.
        JoinRecovery::AwaitWelcome
    } else if may_rejoin {
        JoinRecovery::ExternalJoin
    } else {
        JoinRecovery::StayOut
    }
}

/// The submit outcome, as the pure decision sees it:
/// * `Committed` — the DS confirmed our insert (our bytes are canonical at this
///   epoch by construction).
/// * `LostRace` — the DS reported someone else committed this epoch first
///   (`SubmitResult::LostRace`).
/// * `Failed` — the submit errored (network / stream eviction); the response was
///   lost, so whether our commit landed is *ambiguous*.
///
/// `LostRace` and `Failed` are both ambiguous, so both are resolved against the canonical log.
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

/// Decide adopt-vs-rollback for a submitted own-commit, given the submit
/// `outcome`, `ours` (the exact commit bytes we staged), and `stored_at_epoch`
/// (the bytes the canonical log actually holds at this epoch, or `None` if the
/// log has no row / the read failed).
///
/// The #411 core; Kani proves:
/// * **`Adopt` ⟹ `stored_at_epoch == Some(ours)`** — never adopt a foreign
///   commit (no phantom epoch, no fork).
/// * **`Rollback` ⟹ `stored_at_epoch != Some(ours)`** — never discard a landed
///   own commit (no wedge).
///
/// `Committed` always adopts; `LostRace`/`Failed` adopt IFF the log holds our exact bytes.
pub fn resolve(outcome: SubmitOutcome, ours: &[u8], stored_at_epoch: Option<&[u8]>) -> Resolution {
    match outcome {
        // The DS confirmed our insert — canonical by construction.
        SubmitOutcome::Committed => Resolution::Adopt,
        // Ambiguous: the canonical log is the arbiter. Adopt only if our exact
        // bytes sit at this epoch; otherwise roll back and converge.
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

/// Classify the next step of the commit-replay loop, given `current_epoch` (the
/// local group's epoch) and `next_row_epoch` (the epoch of the next
/// `mls_commit_log` row in `epoch ASC` order, or `None` when the loop is out of
/// rows). Mirrors the gap detector in
/// `group_state::process_pending_commits_locked_impl` exactly:
/// `if commit.epoch != current_epoch → gap-recover, else apply`.
///
/// Kani proves replay never `Apply`s across a gap.
pub fn classify(current_epoch: u64, next_row_epoch: Option<u64>) -> ReplayStep {
    match next_row_epoch {
        None => ReplayStep::Wait,
        Some(e) if e == current_epoch => ReplayStep::Apply,
        // Missing bridge: a permanent gap.
        Some(_) => ReplayStep::GapRecover,
    }
}

/// What a replay pass does when the next commit row does not bridge the local
/// group's epoch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GapRecovery {
    /// The server DECLARED the commit absent (pruned floor above us, or a snapshot hole): rebuild by external join.
    Rebuild,
    /// The batch is non-contiguous with no declared explanation. Hold position:
    /// advance nothing, delete nothing, retry on the next pass.
    HoldPosition,
}

/// Decide whether a gap at `current_epoch` licenses DESTROYING this device's MLS
/// crypto state (#1161 M6).
///
/// `hole_expected` is the epoch the DS reported missing (`ConversationState::hole`)
/// and `pruned_floor` its retention floor (`pruned_below`); both come from the
/// same read transaction as the batch.
///
/// Rebuilding deletes the only keys for this lineage, so a gap licenses it only when the server declared a covering cause:
///
/// * `pruned_floor > current_epoch` — the bridging commit was retention-collected;
/// * `hole_expected == current_epoch` — the DS's own contiguity check found
///   exactly this epoch missing.
///
/// Anything else holds position: nothing is deleted and the next pass re-reads.
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

/// The standing of the leaf that SIGNED a GroupInfo, against the roster's
/// cross-signing material — [`super::device::LeafVerdict`] with its payloads
/// dropped, so this decision stays pure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SignerStanding {
    /// The signing leaf is the device key the claimed user's account key
    /// certified, and that device is not revoked.
    Certified,
    /// The inputs to decide are missing — unknown user, no account key, no
    /// device row, cert columns not yet published.
    Unverifiable,
    /// The inputs are present and the leaf is NOT that user's device: revoked,
    /// a cert that does not verify, or a leaf key that is not the certified one.
    Uncertified,
}

/// Whether a published GroupInfo may be adopted as a lineage of this
/// conversation — i.e. external-joined, with everything that follows from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineageAdoption {
    /// Nothing positively contradicts the signer's claim to have built this
    /// group as a member of this conversation.
    Adopt,
    /// Positive evidence the signer is not a current member's certified device.
    Refuse,
}

/// Decide whether the device that signed a GroupInfo is allowed to have founded
/// the lineage we are about to adopt (#1161 M6 residual).
///
/// A `GroupId` is only a name; a DS holds no account key, so it cannot forge a leaf the roster's cross-signing certifies.
///
/// `signer_on_roster` is `None` when the roster could not be read, which is not evidence.
///
/// **The failure bias is load-bearing:** `Unverifiable` is an ordinary race, so refuse only on POSITIVE evidence:
///
/// * `Uncertified` — the cross-signing chain is present and says no;
/// * a signer whose claimed user is demonstrably not on the roster.
///
/// Everything else adopts.
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

    /// Fill a FIXED-SIZE stack array with symbolic bytes over the tiny domain
    /// `0..=1` — enough to exercise byte-equal and byte-unequal commits while
    /// keeping CBMC's state space small. Never a `Vec` (heap → OOM).
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

    /// I5: a revoked or removed device NEVER rejoins (all four inputs).
    #[kani::proof]
    fn i5_gate_never_leaks() {
        let registered: bool = kani::any();
        let is_member: bool = kani::any();

        let allowed = may_rejoin(registered, is_member);

        // Rejoin is permitted ONLY when both gates pass. Equivalently: a device
        // that is either revoked or removed can never rejoin.
        if !registered || !is_member {
            assert!(!allowed);
        }
        // And the converse — the sole admitting input is (true, true) — so a
        // permitted rejoin proves both gates held.
        if allowed {
            assert!(registered && is_member);
        }
    }

    /// Negative harness: OR instead of AND fails open; `should_panic` proves the harness has teeth.
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

    /// I6: a device with a pending Welcome NEVER external-joins.
    #[kani::proof]
    fn i6_welcome_first_never_double_joins() {
        let welcome_pending: bool = kani::any();
        let may_rejoin: bool = kani::any();
        let group_info_available: bool = kani::any();

        match join_recovery(welcome_pending, may_rejoin, group_info_available) {
            // The two headline implications: never external-join while a Welcome
            // is queued, and never external-join with nothing to join onto (#832).
            JoinRecovery::ExternalJoin => {
                assert!(!welcome_pending);
                assert!(may_rejoin);
                assert!(group_info_available);
            }
            // Deferring to the Welcome happens when one is pending OR when there
            // is no GroupInfo, in which case a Welcome is the only way in.
            JoinRecovery::AwaitWelcome => assert!(welcome_pending || !group_info_available),
            // Stay-out is exactly "no Welcome, GroupInfo exists, not allowed in".
            JoinRecovery::StayOut => {
                assert!(!welcome_pending);
                assert!(group_info_available);
                assert!(!may_rejoin);
            }
        }
    }

    /// Negative harness: external-joins despite a pending Welcome; Kani must find the double-join.
    fn join_recovery_mutant(
        welcome_pending: bool,
        may_rejoin: bool,
        _group_info_available: bool,
    ) -> JoinRecovery {
        // BUG: rejoin gate before the Welcome, and ignores `group_info_available` (#832).
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
            // Refuted on BOTH implications: the mutant external-joins with a
            // Welcome pending, and with no GroupInfo to join onto.
            assert!(!welcome_pending);
            assert!(group_info_available);
        }
    }

    // Fixed-size arrays (N = 3, bytes 0..=1), symbolic stored value and outcome.
    const N: usize = 3;

    /// I2: `resolve` never adopts a foreign commit and never rolls back our own
    /// landed one.
    ///   * `Adopt` ⟹ `stored_at_epoch == Some(ours)` (no phantom epoch / fork)
    ///   * `Rollback` ⟹ `stored_at_epoch != Some(ours)` (no wedge)
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

        // DS contract: `Committed` means it wrote OUR bytes; model that coupling.
        if outcome == SubmitOutcome::Committed {
            kani::assume(stored == Some(&ours[..]));
        }

        match resolve(outcome, &ours[..], stored) {
            // No foreign adopt: an adopted commit is exactly the one at this epoch.
            Resolution::Adopt => assert!(stored == Some(&ours[..])),
            // No own rollback: we only roll back when the log does NOT hold ours.
            Resolution::Rollback => assert!(stored != Some(&ours[..])),
        }
    }

    /// Negative harness: adopts an ambiguous outcome unconditionally.
    fn resolve_mutant(outcome: SubmitOutcome, _ours: &[u8], _stored: Option<&[u8]>) -> Resolution {
        match outcome {
            SubmitOutcome::Committed => Resolution::Adopt,
            // BUG: adopts a LostRace/Failed outcome without checking the log —
            // would graft a foreign commit / phantom epoch (#411).
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

    /// I1: `classify` never `Apply`s across a gap — `Apply` ⟺ the next row's
    /// epoch is exactly `current_epoch`; any mismatch is `GapRecover` and `None`
    /// is `Wait`.
    #[kani::proof]
    fn i1_classify_no_gap_apply() {
        let current: u64 = kani::any();
        kani::assume(current <= 3);
        let next = symbolic_epoch_opt();

        match classify(current, next) {
            // The headline: an Apply is only ever the exact bridging commit.
            ReplayStep::Apply => assert!(next == Some(current)),
            // A gap is a present-but-non-bridging row — never silently skipped.
            ReplayStep::GapRecover => {
                assert!(next.is_some());
                assert!(next != Some(current));
            }
            // Wait is exactly "no more rows".
            ReplayStep::Wait => assert!(next.is_none()),
        }
    }

    /// Negative harness: applies any present row, across a gap.
    fn classify_mutant(current_epoch: u64, next_row_epoch: Option<u64>) -> ReplayStep {
        match next_row_epoch {
            None => ReplayStep::Wait,
            // BUG: applies regardless of whether the row bridges the current
            // epoch — a forward-gap row is applied across the hole.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// I6: a pending Welcome ALWAYS wins, so a fresh DM recipient never races its own Welcome.
    #[test]
    fn join_recovery_welcome_first_never_double_joins() {
        // A pending Welcome wins even for a member that could external-join.
        assert_eq!(join_recovery(true, true, true), JoinRecovery::AwaitWelcome);
        assert_eq!(join_recovery(true, false, true), JoinRecovery::AwaitWelcome);

        // No Welcome available: the I5 gate decides. A current member recovers via
        // external-join (Secret-Key recovery / genuinely dropped Welcome)...
        assert_eq!(join_recovery(false, true, true), JoinRecovery::ExternalJoin);
        // ...and a revoked/removed device stays out (never climbs back in).
        assert_eq!(join_recovery(false, false, true), JoinRecovery::StayOut);

        // ExternalJoin ⟹ no Welcome pending, over the whole 3-bit space.
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

    /// #832: no Welcome and NO GroupInfo must wait, not external-join.
    #[test]
    fn no_group_info_never_selects_an_impossible_external_join() {
        assert_eq!(
            join_recovery(false, true, false),
            JoinRecovery::AwaitWelcome,
            "a member with no published GroupInfo must await the Welcome, not \
             external-join onto a group that has nothing to join onto"
        );

        // Even a device that may NOT rejoin waits rather than reporting StayOut:
        // with no GroupInfo we cannot yet distinguish "removed" from "not yet
        // published", and StayOut is the one verdict that gives up permanently.
        assert_eq!(
            join_recovery(false, false, false),
            JoinRecovery::AwaitWelcome
        );

        // The new implication, exhaustively: ExternalJoin ⟹ group_info_available.
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

    /// The headline property: `Rebuild` implies the server declared an
    /// explanation that covers the epoch we are missing.
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

    /// An unexplained non-contiguous batch — the shape a malicious or broken DS
    /// serves — must not be able to delete anything.
    #[test]
    fn an_unexplained_gap_holds_position() {
        assert_eq!(gap_recovery(7, None, None), GapRecovery::HoldPosition);
        // A hole declared somewhere else is a self-contradicting response, not a
        // licence.
        assert_eq!(gap_recovery(7, Some(9), None), GapRecovery::HoldPosition);
        // A retention floor at or below us explains nothing: the commit we need
        // is not below the floor.
        assert_eq!(gap_recovery(7, None, Some(7)), GapRecovery::HoldPosition);
    }

    /// …and the two genuine causes still rebuild, so a conversation that really
    /// has lost the commit is not stuck forever.
    #[test]
    fn a_declared_prune_or_hole_still_rebuilds() {
        assert_eq!(gap_recovery(7, Some(7), None), GapRecovery::Rebuild);
        assert_eq!(gap_recovery(7, None, Some(8)), GapRecovery::Rebuild);
    }

    /// #1161 M6 residual: the whole 3×3 truth table of [`lineage_adoption`].
    ///
    /// Refusal is exactly the two POSITIVE cases; widening it into `Unverifiable` would brick migrations.
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

    /// The failure bias, pinned on its own: an unverifiable signer on a roster
    /// we could read, and a certified signer whose roster we could NOT read,
    /// both still adopt. Both are ordinary races on a legitimate migration.
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
