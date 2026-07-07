//! The delivery-watermark computation — extracted as a pure, generic free
//! function so it can be (1) exercised by the real ingest path and (2) proved
//! by Kani over symbolic inputs.
//!
//! ## What this decides (the safety property)
//!
//! During interleaved catch-up ([`super::ingest`]) each conversation gets a new
//! watermark: an EXCLUSIVE `sent_at` cursor that the next fetch uses as
//! `sent_at > watermark`. Advancing this cursor past an envelope means "never
//! fetch it again". So the cursor MUST NOT advance to or past any envelope we
//! still have to retry (an MLS message whose epoch this pass never reached) —
//! otherwise a current member permanently loses a decryptable message (failure
//! class F3; the exact property #442 was a false alarm about).
//!
//! The rule, preserved verbatim from the original inline logic:
//! * `stop_at` = the `sent_at` of the FIRST un-handled envelope (in the given,
//!   `sent_at`-ordered, slice order).
//! * the candidate loop walks the slice and adopts each `sent_at` as the running
//!   watermark, but BREAKS as soon as it reaches an envelope with
//!   `sent_at >= stop_at`. The `>=` (not `>`) is deliberate: on a `sent_at`
//!   tie between a handled and an un-handled envelope the cursor must stop
//!   STRICTLY BELOW the shared timestamp, or the next `sent_at > watermark`
//!   fetch would skip the un-handled one. The watermark is therefore always
//!   strictly less than the first un-handled `sent_at`, even on a tie.
//!
//! The Kani harnesses at the bottom of this file prove exactly that (P1), plus
//! monotonicity (P2) and handled-liveness (P3). Each proof is paired with a
//! deliberately-broken mutant harness (`p{1,2,3}_mutant_refuted`, all
//! `#[kani::should_panic]`) demonstrating that proof still has teeth.

/// The only distinction the watermark cares about: whether an envelope's
/// deliverability is gated on reaching its MLS epoch this pass.
///
/// `Message` / `Edit` carry an MLS epoch and are epoch-gated (handled only once
/// the shared group's replay reached — or provably can never reach — their
/// epoch). `Delete` tombstones and any `Other` (unknown) type are
/// epoch-independent and always handled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvKind {
    Message,
    Edit,
    Delete,
    Other,
}

impl EnvKind {
    /// Map a `message_envelope.type` string to the watermark's kind. Mirrors the
    /// original `is_handled` match arms exactly: only `"message"` / `"edit"` are
    /// epoch-gated; everything else (`"delete"`, unknown) is always handled.
    pub fn from_type(env_type: &str) -> Self {
        match env_type {
            "message" => EnvKind::Message,
            "edit" => EnvKind::Edit,
            "delete" => EnvKind::Delete,
            _ => EnvKind::Other,
        }
    }
}

/// Is this envelope definitively handled (so the watermark may advance over it),
/// or must a later pass retry it? `max_fired_epoch` is the highest MLS epoch the
/// shared group's replay reached this pass (`None` = no local group, nothing
/// could be decrypted). Kept private and byte-for-byte identical to the arms of
/// the original inline `is_handled` closure.
fn is_handled(kind: EnvKind, epoch: Option<u64>, max_fired_epoch: Option<u64>) -> bool {
    match kind {
        EnvKind::Message | EnvKind::Edit => match (epoch, max_fired_epoch) {
            // Epoch within this pass's reach: decrypted now, or an unreachable
            // pre-join epoch we will never decrypt. Either way permanently
            // handled — advancing past it can't drop a message.
            (Some(e), Some(max)) => e <= max,
            // Unparseable bytes are never MLS-decryptable → permanently handled
            // (advancing past avoids wedging on a corrupt row).
            (None, _) => true,
            // The replay reached no epoch (no local group): nothing could be
            // decrypted, so these must be retried once a group exists.
            (Some(_), None) => false,
        },
        // delete tombstones / unknown types are epoch-independent.
        EnvKind::Delete | EnvKind::Other => true,
    }
}

/// Compute the `sent_at` a conversation's watermark may advance to, given its
/// `sent_at`-ordered envelopes and the highest MLS epoch this pass reached.
///
/// Returns the greatest `sent_at` in the contiguous prefix that is definitively
/// handled and strictly below the first un-handled envelope's `sent_at`, or
/// `None` if nothing may advance (empty slice, or the very first envelope must
/// be retried). Generic over the `sent_at` key `S` so the real callers pass
/// `&str`/`String` while the proofs pass bounded integers (Kani cannot make a
/// `String` symbolic).
pub fn next_watermark<S: Ord + Clone>(
    envs: &[(S, EnvKind, Option<u64>)],
    max_fired_epoch: Option<u64>,
) -> Option<S> {
    // The `sent_at` of the first envelope we must retry is an EXCLUSIVE ceiling
    // on the watermark: advancing to (or, via a `sent_at` tie, past) it would
    // drop it from the next `sent_at > watermark` fetch.
    let stop_at: Option<&S> = envs
        .iter()
        .find(|(_, kind, epoch)| !is_handled(*kind, *epoch, max_fired_epoch))
        .map(|(sent_at, _, _)| sent_at);

    let mut candidate: Option<S> = None;
    for (sent_at, _, _) in envs {
        if let Some(stop) = stop_at {
            if sent_at >= stop {
                break;
            }
        }
        candidate = Some(sent_at.clone());
    }
    candidate
}

// ─── Kani proof harnesses ────────────────────────────────────────────────────
//
// Behind `#[cfg(kani)]` only — never compiled into the runtime crate. Bounded to
// `envs.len() <= 4` with `#[kani::unwind(5)]`; keys/epochs/kinds are symbolic
// over a small integer domain. The slices are built sorted-ascending by key to
// match the real caller's `ORDER BY sent_at ASC, id ASC`.
#[cfg(kani)]
mod proofs {
    use super::*;

    // Small symbolic domains: keys and epochs live in `0..=3` so CBMC's state
    // space stays exhaustive-but-bounded while still exercising ties, gaps, and
    // ordering. Bounded at 4: the no-skip / tie counterexamples this proves are
    // 2-3-element phenomena, so len-4 finds them. NOTE: CBMC is memory-hungry
    // here (Vec heap modelling) and OOMs a 7 GB box even at len-3, so these
    // proofs run in CI (the `kani.yml` job, on a 16 GB runner), not in-box.
    const MAX_LEN: usize = 4;

    impl kani::Arbitrary for EnvKind {
        fn any() -> Self {
            match kani::any::<u8>() % 4 {
                0 => EnvKind::Message,
                1 => EnvKind::Edit,
                2 => EnvKind::Delete,
                _ => EnvKind::Other,
            }
        }
    }

    /// Build a symbolic, `sent_at`-ascending (ties allowed) envelope table into a
    /// FIXED-SIZE stack array plus a symbolic valid length `0..=MAX_LEN`. The
    /// harnesses use `&arr[..len]`. Using an array rather than a `Vec` is
    /// load-bearing: CBMC models `Vec`'s heap allocation at ruinous memory/time
    /// cost (it OOMs/​times-out even at small bounds), while a stack array is
    /// cheap. Every slot is constrained, so the used prefix is a valid ascending
    /// sequence. `distinct_keys` forces strictly-increasing keys for the
    /// harnesses (P2/P3) whose statement is only clean without `sent_at` ties;
    /// with keys in `0..=3` and `MAX_LEN == 4` a distinct fill is exactly
    /// `[0,1,2,3]` (satisfiable — do not raise MAX_LEN past the key domain).
    fn symbolic_envs(distinct_keys: bool) -> ([(u8, EnvKind, Option<u64>); MAX_LEN], usize) {
        let len: usize = kani::any();
        kani::assume(len <= MAX_LEN);

        let mut arr = [(0u8, EnvKind::Message, None); MAX_LEN];
        let mut prev: Option<u8> = None;
        for slot in arr.iter_mut() {
            let key: u8 = kani::any();
            kani::assume(key <= 3);
            if let Some(p) = prev {
                if distinct_keys {
                    kani::assume(key > p);
                } else {
                    kani::assume(key >= p);
                }
            }
            prev = Some(key);

            let kind: EnvKind = kani::any();
            // Epoch only meaningful for message/edit; keep it bounded and present
            // only where the real parser would produce one.
            let epoch: Option<u64> = if kani::any() {
                let e: u64 = kani::any();
                kani::assume(e <= 3);
                Some(e)
            } else {
                None
            };
            *slot = (key, kind, epoch);
        }
        (arr, len)
    }

    fn symbolic_max_fired() -> Option<u64> {
        if kani::any() {
            let m: u64 = kani::any();
            kani::assume(m <= 3);
            Some(m)
        } else {
            None
        }
    }

    /// P1 (no-skip / anti-F3): the returned watermark is STRICTLY LESS than the
    /// `sent_at` of the first un-handled envelope. ⇒ the next
    /// `sent_at > watermark` fetch cannot drop an un-decrypted message. The
    /// headline proof.
    #[kani::proof]
    #[kani::unwind(5)]
    fn p1_no_skip() {
        let (arr, len) = symbolic_envs(false);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let first_unhandled = envs
            .iter()
            .find(|(_, kind, epoch)| !is_handled(*kind, *epoch, max_fired))
            .map(|(k, _, _)| *k);

        let wm = next_watermark(envs, max_fired);

        if let Some(stop) = first_unhandled {
            // Whether or not the watermark advanced, it must sit strictly below
            // the first envelope we still owe a retry.
            if let Some(w) = wm {
                assert!(w < stop);
            }
        }
    }

    /// P2 (monotone): `next_watermark` over a prefix `<=` over the full slice — a
    /// superset never regresses the cursor. Stated over strictly-increasing keys
    /// (distinct `sent_at`); under a handled/un-handled `sent_at` TIE the cursor
    /// is *correctly* pulled back below the shared timestamp, so monotonicity is
    /// only a clean property when keys are distinct.
    #[kani::proof]
    #[kani::unwind(5)]
    fn p2_monotone() {
        let (arr, len) = symbolic_envs(true);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let cut: usize = kani::any();
        kani::assume(cut <= envs.len());
        let prefix = &envs[..cut];

        let wm_prefix = next_watermark(prefix, max_fired);
        let wm_full = next_watermark(envs, max_fired);

        // Option ordering: None < Some(_), so a prefix that produced no cursor
        // never exceeds the full slice's cursor.
        assert!(wm_prefix <= wm_full);
    }

    /// P3 (handled-liveness): if EVERY envelope is handled, the watermark equals
    /// the max `sent_at` — nothing decryptable is retried forever. Stated over
    /// strictly-increasing keys so "max" is the last element.
    #[kani::proof]
    #[kani::unwind(5)]
    fn p3_handled_liveness() {
        let (arr, len) = symbolic_envs(true);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let all_handled = envs
            .iter()
            .all(|(_, kind, epoch)| is_handled(*kind, *epoch, max_fired));
        kani::assume(all_handled);

        let wm = next_watermark(envs, max_fired);

        match envs.last() {
            // Strictly-increasing keys ⇒ the last element carries the max sent_at.
            Some((max_key, _, _)) => assert!(wm == Some(*max_key)),
            None => assert!(wm.is_none()),
        }
    }

    // ─── Negative test: the harness has teeth ────────────────────────────────
    //
    // A deliberately-broken variant of `next_watermark` that breaks on
    // `sent_at > stop` instead of `sent_at >= stop`. On a `sent_at` tie between a
    // handled and an un-handled envelope it lets the cursor advance ONTO the
    // shared timestamp, so the next `sent_at > watermark` fetch skips the
    // un-handled envelope — exactly the F3 message-loss bug. `p1_mutant_refuted`
    // asserts P1 on it; Kani must find a counterexample (see the report). This is
    // test-only and unreachable from any runtime code.
    fn next_watermark_mutant<S: Ord + Clone>(
        envs: &[(S, EnvKind, Option<u64>)],
        max_fired_epoch: Option<u64>,
    ) -> Option<S> {
        let stop_at: Option<&S> = envs
            .iter()
            .find(|(_, kind, epoch)| !is_handled(*kind, *epoch, max_fired_epoch))
            .map(|(sent_at, _, _)| sent_at);

        let mut candidate: Option<S> = None;
        for (sent_at, _, _) in envs {
            if let Some(stop) = stop_at {
                // BUG: `>` lets a tie through where the real code uses `>=`.
                if sent_at > stop {
                    break;
                }
            }
            candidate = Some(sent_at.clone());
        }
        candidate
    }

    /// Asserts P1 on the mutant. `#[kani::should_panic]`: the harness PASSES
    /// exactly when Kani finds the P1 assertion can fail — i.e. it produces the
    /// counterexample (a `sent_at` tie between a handled and an un-handled
    /// envelope) that the real code's `>=` avoids. Without `should_panic` this
    /// (correctly) reports FAILED and would redden CI; with it, a green run
    /// certifies the harness still has teeth. If the mutant ever stopped
    /// violating P1, this harness would FAIL (nothing panicked) — catching a
    /// toothless proof.
    #[kani::proof]
    #[kani::should_panic]
    #[kani::unwind(5)]
    fn p1_mutant_refuted() {
        let (arr, len) = symbolic_envs(false);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let first_unhandled = envs
            .iter()
            .find(|(_, kind, epoch)| !is_handled(*kind, *epoch, max_fired))
            .map(|(k, _, _)| *k);

        let wm = next_watermark_mutant(envs, max_fired);

        if let Some(stop) = first_unhandled {
            if let Some(w) = wm {
                assert!(w < stop);
            }
        }
    }

    // ─── Negative test for P2 (monotone) ─────────────────────────────────────
    //
    // A deliberately-broken variant that, on reaching the first un-handled
    // envelope, BAILS OUT to `None` — discarding the fully-handled run below it —
    // instead of returning that run's greatest `sent_at`. This is a plausible
    // "over-conservative" bug: "if anything is un-handled, don't advance at all".
    // It preserves P1 (returning `None` never skips past an un-handled envelope)
    // and P3 (never triggers when all envelopes are handled), so ONLY P2 catches
    // it: a prefix that stops short of the un-handled envelope keeps its cursor,
    // while the full slice — which sees the un-handled envelope — collapses to
    // `None`, making `wm_prefix > wm_full`. Test-only, unreachable from runtime.
    fn next_watermark_p2_mutant<S: Ord + Clone>(
        envs: &[(S, EnvKind, Option<u64>)],
        max_fired_epoch: Option<u64>,
    ) -> Option<S> {
        let mut candidate: Option<S> = None;
        for (sent_at, kind, epoch) in envs {
            // BUG: abandon the good handled prefix instead of returning it.
            if !is_handled(*kind, *epoch, max_fired_epoch) {
                return None;
            }
            candidate = Some(sent_at.clone());
        }
        candidate
    }

    /// Asserts P2 on the P2-mutant. `#[kani::should_panic]`: PASSES exactly when
    /// Kani finds a state where the mutant regresses the cursor across a superset
    /// (a handled prefix followed by an un-handled envelope the prefix excluded),
    /// certifying P2 has teeth. If the mutant ever stopped regressing, this would
    /// FAIL (nothing panicked) — catching a vacuous monotonicity proof.
    #[kani::proof]
    #[kani::should_panic]
    #[kani::unwind(5)]
    fn p2_mutant_refuted() {
        let (arr, len) = symbolic_envs(true);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let cut: usize = kani::any();
        kani::assume(cut <= envs.len());
        let prefix = &envs[..cut];

        let wm_prefix = next_watermark_p2_mutant(prefix, max_fired);
        let wm_full = next_watermark_p2_mutant(envs, max_fired);

        assert!(wm_prefix <= wm_full);
    }

    // ─── Negative test for P3 (handled-liveness) ─────────────────────────────
    //
    // A deliberately-broken variant with an off-by-one that refuses to advance
    // ONTO the final envelope even when it is handled — the cursor lags one short
    // of a fully-handled prefix. This preserves P1 (stopping short never skips an
    // un-handled envelope), so ONLY P3 catches it: when every envelope is handled
    // the watermark must equal the max `sent_at`, but the mutant returns the
    // second-to-last (or `None` for a single element). Test-only.
    fn next_watermark_p3_mutant<S: Ord + Clone>(
        envs: &[(S, EnvKind, Option<u64>)],
        max_fired_epoch: Option<u64>,
    ) -> Option<S> {
        let stop_at: Option<&S> = envs
            .iter()
            .find(|(_, kind, epoch)| !is_handled(*kind, *epoch, max_fired_epoch))
            .map(|(sent_at, _, _)| sent_at);

        let n = envs.len();
        let mut candidate: Option<S> = None;
        for (i, (sent_at, _, _)) in envs.iter().enumerate() {
            if let Some(stop) = stop_at {
                if sent_at >= stop {
                    break;
                }
            }
            // BUG: never adopt the last envelope, even when it is handled — the
            // cursor gets stuck one below a fully-handled prefix.
            if i + 1 == n {
                break;
            }
            candidate = Some(sent_at.clone());
        }
        candidate
    }

    /// Asserts P3 on the P3-mutant. `#[kani::should_panic]`: PASSES exactly when
    /// Kani finds an all-handled slice whose watermark falls short of the max
    /// `sent_at`, certifying P3 has teeth. If the mutant ever stopped lagging,
    /// this would FAIL (nothing panicked) — catching a vacuous liveness proof.
    #[kani::proof]
    #[kani::should_panic]
    #[kani::unwind(5)]
    fn p3_mutant_refuted() {
        let (arr, len) = symbolic_envs(true);
        let envs = &arr[..len];
        let max_fired = symbolic_max_fired();

        let all_handled = envs
            .iter()
            .all(|(_, kind, epoch)| is_handled(*kind, *epoch, max_fired));
        kani::assume(all_handled);

        let wm = next_watermark_p3_mutant(envs, max_fired);

        match envs.last() {
            Some((max_key, _, _)) => assert!(wm == Some(*max_key)),
            None => assert!(wm.is_none()),
        }
    }
}
