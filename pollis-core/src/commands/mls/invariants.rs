//! Load-bearing MLS control-plane decisions, extracted as small **pure**
//! functions so they can be (1) called by the real runtime paths and (2) proved
//! exhaustively by Kani over symbolic inputs.
//!
//! This is the M2 tranche of the machine-checked-correctness program
//! (`docs/machine-checked-correctness-design.md` §4.2–§4.4):
//!
//! * [`may_rejoin`] — I5, the recovery gate (§4.4): a revoked *or* removed device
//!   never takes an external-join recovery path back into the tree.
//! * [`resolve`] — I2, own-commit canonicalization (§4.3, the #411 property): the
//!   adopt-vs-rollback decision never adopts a foreign commit and never rolls
//!   back our own landed commit.
//! * [`classify`] — I1, client-side gap detection (§4.2): replay never `Apply`s a
//!   commit across an epoch gap.
//!
//! The DS-side head arithmetic (I1's `MAX(epoch)+1`) is proved in
//! `pollis-delivery::commit` (a pure `head_epoch_of` + its own Kani harness),
//! since that code lives in the Delivery Service crate.
//!
//! ## Kani discipline (learned in M1 — see `messages/watermark.rs`)
//!
//! CBMC models heap allocation (`Vec`, `String`) at ruinous memory cost and OOMs
//! a small box. Every harness here therefore uses FIXED-SIZE stack arrays
//! `[T; N]` + a symbolic valid length, keeps symbolic domains tiny (bytes/epochs
//! in `0..=3`), bounds loops with `#[kani::unwind(N)]`, and uses
//! `#[kani::should_panic]` for the negative (mutant) harnesses.

// ─── I5: recovery gate ───────────────────────────────────────────────────────

/// Both cooperative gates on the external-join *recovery* paths, as a pure
/// predicate: this device is still `registered` (not revoked) AND its user
/// `is_member` of the group. A `false` from either means "do not rebuild/rejoin".
///
/// This is the exact `local_device_registered ∧ local_user_is_member` conjunction
/// that `group_state::may_rejoin_via_external_join` gates recovery on. Lifting it
/// to a free function lets Kani prove the whole (2-bit) truth table: the only
/// input that admits a rejoin is `(true, true)`. A revoked or removed device can
/// never climb back into the tree — the membership / forward-secrecy leak of
/// fuzzer-finding #2, proved impossible.
pub fn may_rejoin(registered: bool, is_member: bool) -> bool {
    registered && is_member
}

// ─── I6: Welcome-vs-external-join recovery arbitration ────────────────────────

/// What a device with NO local MLS group does when it finds pending commits for a
/// conversation it cannot yet open.
///
/// The recipient of a freshly-created DM has TWO ways into the group: the inbound
/// Welcome (`apply_welcome`) and, once the creator publishes GroupInfo, the
/// external-join *recovery* path. Both do "delete stale local group → (re)join",
/// so running them concurrently makes the device race its own Welcome and can
/// clobber the freshly-joined group — stranding the member with
/// `no GroupInfo stored — cannot external-join`.
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

/// Decide the no-local-group recovery action from the two guards the runtime
/// evaluates: `welcome_pending` (an undelivered Welcome targets this device for
/// this conversation) and `may_rejoin` (the I5 gate: registered ∧ member).
///
/// **Welcome-first.** Whenever a Welcome is pending we defer to it, REGARDLESS of
/// the rejoin gate — the Welcome is the canonical, cheaper join, and
/// `apply_welcome` marks the row delivered even on failure, so a stuck Welcome
/// self-heals into the external-join path on a later pass (once `welcome_pending`
/// is false). Only when no Welcome is available does the I5 gate decide
/// external-join vs stay-out.
///
/// The Kani harness proves the load-bearing property: **`ExternalJoin` ⟹
/// `!welcome_pending`** — a device with a pending Welcome NEVER external-joins, so
/// the two concurrent join mechanisms can never both run for one fresh membership
/// (the DM-accept convergence race). This is the pure core of the gate in
/// `group_state::process_pending_commits_locked_impl`.
pub fn join_recovery(welcome_pending: bool, may_rejoin: bool) -> JoinRecovery {
    if welcome_pending {
        JoinRecovery::AwaitWelcome
    } else if may_rejoin {
        JoinRecovery::ExternalJoin
    } else {
        JoinRecovery::StayOut
    }
}

// ─── I2: own-commit canonicalization (#411) ──────────────────────────────────

/// The outcome of submitting a commit through the delivery seam, as the pure
/// decision sees it. Mirrors the three real cases the reconcile / external-join
/// paths branch on:
/// * `Committed` — the DS confirmed our insert (our bytes are canonical at this
///   epoch by construction).
/// * `LostRace` — the DS reported someone else committed this epoch first
///   (`SubmitResult::LostRace`).
/// * `Failed` — the submit errored (network / stream eviction); the response was
///   lost, so whether our commit landed is *ambiguous*.
///
/// `LostRace` and `Failed` are both ambiguous — a stale `LostRace` can be a retry
/// of our OWN accepted commit, and a `Failed` can hide a commit that landed with
/// only the response lost — so both are resolved against the canonical log.
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
/// This is the #411 correctness core (`reconcile.rs` `our_commit_is_canonical` +
/// the adopt/rollback branch), lifted to a pure function so Kani can prove both
/// directions:
/// * **`Adopt` ⟹ `stored_at_epoch == Some(ours)`** — never adopt a foreign
///   commit (no phantom epoch, no fork).
/// * **`Rollback` ⟹ `stored_at_epoch != Some(ours)`** — never discard a landed
///   own commit (no wedge).
///
/// A `Committed` outcome always adopts: the DS only returns `Committed` when it
/// wrote *our* bytes at this epoch, so they are canonical there by construction
/// (the Kani harness encodes that coupling). The ambiguous `LostRace` / `Failed`
/// outcomes adopt IFF the log's bytes at this epoch are byte-for-byte ours.
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

// ─── I1: client-side gap detection ───────────────────────────────────────────

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
/// The Kani harness proves the load-bearing property: **replay never `Apply`s
/// across a gap** — `Apply` is returned ONLY when the next row's epoch is exactly
/// `current_epoch`. A missing bridge (higher epoch present) always yields
/// `GapRecover`, never a silent skip that would wedge a member.
///
/// NOTE (deviation from the design sketch `§4.2`, which typed this as
/// `classify(current_epoch, next_row_epoch, head)`): the real client gap detector
/// consults neither the log head nor any other state — its decision is purely
/// `next_row_epoch == current_epoch`. Threading an unused `head` would be less
/// faithful, so it is omitted here. The head arithmetic the design attaches to I1
/// is the *DS-side* concern and is proved separately as
/// `pollis_delivery::commit::head_epoch_of`.
pub fn classify(current_epoch: u64, next_row_epoch: Option<u64>) -> ReplayStep {
    match next_row_epoch {
        // Out of rows: nothing left to replay this pass.
        None => ReplayStep::Wait,
        // The next row bridges current_epoch → current_epoch + 1: apply it.
        Some(e) if e == current_epoch => ReplayStep::Apply,
        // The bridging commit is missing while a different (higher, by the log's
        // ascending order) epoch is present: a permanent gap — recover.
        Some(_) => ReplayStep::GapRecover,
    }
}

// ─── Kani proof harnesses ────────────────────────────────────────────────────
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

    /// I5: a revoked (`!registered`) OR removed (`!is_member`) device NEVER
    /// rejoins — the exhaustive 2-bit truth table. `registered`/`is_member` are
    /// `bool`, so `kani::any()` covers all four combinations with no unwind.
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

    /// Negative harness: a deliberately-broken gate that uses OR instead of AND
    /// (fail-open) lets a removed-but-registered device rejoin. `should_panic`:
    /// this PASSES exactly when Kani finds the leak the real `&&` avoids — a green
    /// run certifies the harness has teeth.
    fn may_rejoin_mutant(registered: bool, is_member: bool) -> bool {
        // BUG: OR fails open — a removed member (is_member = false) but still
        // registered device would rejoin the tree.
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

    // ── I6: Welcome-vs-external-join recovery arbitration ─────────────────────

    /// I6: a device with a pending Welcome NEVER external-joins — the exhaustive
    /// 2-bit truth table. This is what makes the DM-accept dual-path race
    /// unrepresentable: `ExternalJoin` is admitted ONLY when no Welcome is
    /// pending, so the Welcome-apply and external-join recovery can never both
    /// fire for one fresh membership.
    #[kani::proof]
    fn i6_welcome_first_never_double_joins() {
        let welcome_pending: bool = kani::any();
        let may_rejoin: bool = kani::any();

        match join_recovery(welcome_pending, may_rejoin) {
            // The headline: never external-join while a Welcome is queued.
            JoinRecovery::ExternalJoin => {
                assert!(!welcome_pending);
                assert!(may_rejoin);
            }
            // Deferring to the Welcome happens exactly when one is pending.
            JoinRecovery::AwaitWelcome => assert!(welcome_pending),
            // Stay-out is exactly "no Welcome and not allowed to rejoin".
            JoinRecovery::StayOut => {
                assert!(!welcome_pending);
                assert!(!may_rejoin);
            }
        }
    }

    /// Negative harness: a broken arbiter that external-joins whenever the rejoin
    /// gate passes, IGNORING a pending Welcome — the exact dual-path that strands
    /// a DM recipient. `should_panic`: Kani must find the double-join the real
    /// Welcome-first guard prevents.
    fn join_recovery_mutant(welcome_pending: bool, may_rejoin: bool) -> JoinRecovery {
        // BUG: checks the rejoin gate BEFORE the Welcome, so a device with a
        // pending Welcome still external-joins and races its own Welcome.
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

        if let JoinRecovery::ExternalJoin = join_recovery_mutant(welcome_pending, may_rejoin) {
            assert!(!welcome_pending);
        }
    }

    // ── I2: own-commit canonicalization (#411) ───────────────────────────────
    //
    // Small fixed-size byte arrays (N = 3, bytes 0..=1) + a symbolic Some/None
    // stored value + a symbolic outcome. Proves both directions of `resolve`.
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

        // DS contract: a `Committed` result means the DS wrote OUR exact bytes at
        // this epoch, so they are canonical there. Model that coupling so the
        // `Committed → Adopt` branch is only exercised in states the DS produces.
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

    /// Negative harness: a broken `resolve` that adopts an ambiguous outcome
    /// UNCONDITIONALLY (even when the log holds a foreign commit or nothing).
    /// `should_panic`: Kani must find the no-foreign-adopt violation the real
    /// `== Some(ours)` guard prevents.
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

    // ── I1: client-side gap detection ────────────────────────────────────────
    //
    // Symbolic current_epoch + next_row_epoch over the tiny domain 0..=3 (Some or
    // None). The no-gap-apply property is universal, so the small domain loses no
    // generality while keeping CBMC fast.

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

    /// Negative harness: a broken classifier that applies ANY present row (even a
    /// non-bridging one) — the exact gap-skip that wedges a member. `should_panic`:
    /// Kani must find the `Apply` across a gap that the real `e == current_epoch`
    /// guard prevents.
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

// ─── cargo-test unit coverage ─────────────────────────────────────────────────
//
// The Kani harnesses above prove these predicates exhaustively, but Kani is not
// run by `cargo test`. These plain tests pin the load-bearing decisions under the
// normal test suite too, so a regression is caught even without a Kani pass.
#[cfg(test)]
mod tests {
    use super::*;

    /// I6, the DM-accept convergence invariant: a device with a pending Welcome
    /// ALWAYS defers to it and NEVER external-joins, regardless of the rejoin
    /// gate. This is the chokepoint that stops the recipient of a fresh DM from
    /// racing its own inbound Welcome against external-join recovery (the two
    /// concurrent "delete stale group → rejoin" paths that stranded the accept).
    #[test]
    fn join_recovery_welcome_first_never_double_joins() {
        // A pending Welcome always wins — even when the device is a full member
        // that COULD external-join. This is the exact state a fresh DM recipient
        // is in, and the one that used to strand it.
        assert_eq!(join_recovery(true, true), JoinRecovery::AwaitWelcome);
        assert_eq!(join_recovery(true, false), JoinRecovery::AwaitWelcome);

        // No Welcome available: the I5 gate decides. A current member recovers via
        // external-join (Secret-Key recovery / genuinely dropped Welcome)...
        assert_eq!(join_recovery(false, true), JoinRecovery::ExternalJoin);
        // ...and a revoked/removed device stays out (never climbs back in).
        assert_eq!(join_recovery(false, false), JoinRecovery::StayOut);

        // The headline property over the whole 2-bit space: ExternalJoin ⟹ no
        // Welcome pending. If this ever holds with a Welcome pending, the dual-path
        // race is back.
        for welcome_pending in [false, true] {
            for may_rejoin in [false, true] {
                if join_recovery(welcome_pending, may_rejoin) == JoinRecovery::ExternalJoin {
                    assert!(
                        !welcome_pending,
                        "external-join must never run while a Welcome is pending"
                    );
                }
            }
        }
    }
}
