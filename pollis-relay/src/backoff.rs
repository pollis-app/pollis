//! The one jitter definition.
//!
//! Anything on a repeating schedule that a whole FLEET runs — a directory
//! refresh, a revocation-list poll, a re-dial after an outage — has to spread
//! itself out, or every device does the same thing at the same instant and the
//! host they all talk to sees a wall instead of a stream. `park.rs` and
//! `revocation_sync.rs` both say so in their own comments and both jittered;
//! `net::overlay`'s directory refresh and `net::peer::reachability` did not, and
//! #875 found that gap.
//!
//! The un-jittered directory refresh was the worse of the two, and not only on
//! the failure path: its sleep is derived from the directory's `expires_at`,
//! which is the SAME timestamp for every client in the fleet. So at steady state
//! every device woke to refetch within a second of every other device, forever —
//! a structural lockstep, not just an outage-recovery one.
//!
//! Deliberately NOT a full retry helper. `RemoteDb::with_retry` is the repo's
//! ONE retry seam, and it is narrow on purpose: it reconnects a dropped libsql
//! stream and retries a single idempotent statement once (its four call sites
//! are listed on the function). The schedules that don't use it don't use it for
//! real reasons (see the survey in #875): they retry different error types, on
//! different delays, some forever. What they all genuinely share is this —
//! spread the wake-up — so this is the piece that gets factored out, and nothing
//! else.

use std::time::Duration;

/// Deterministic ±12.5% jitter around `base`, derived from the process's own
/// randomness so two nodes started together do not stay in lockstep.
///
/// Never returns zero, and never more than 1.125× `base`, so a caller's clamps
/// stay meaningful. If the OS RNG fails, returns `base` unchanged — a
/// synchronized retry is better than no retry.
pub fn jittered(base: Duration) -> Duration {
    let mut byte = [0u8; 1];
    if getrandom::getrandom(&mut byte).is_err() {
        return base;
    }
    // byte/255 maps to [0, 1]; shift to [-0.125, +0.125] of the base.
    let spread = base.as_millis() as i64 / 4;
    let offset = (i64::from(byte[0]) * spread / 255) - (spread / 2);
    let millis = (base.as_millis() as i64 + offset).max(1) as u64;
    Duration::from_millis(millis)
}

/// Jitter that only ever shortens: uniformly in `[0.875 * base, base]`.
///
/// For a schedule whose CEILING is load-bearing rather than a sanity clamp —
/// `net::peer::reachability` must refresh before the revocation list it forwards
/// on goes stale, and fails closed if it oversleeps — symmetric jitter is not
/// available: a draw above the ceiling eats into a safety margin that was
/// calculated, not guessed. Shortening always is free in that direction, and
/// spreads the fleet just as well.
///
/// Callers still clamp afterwards if they have a floor: at a pathological
/// `base` near the floor there is no room to shorten, and a floor exists to stop
/// a tight loop.
pub fn jittered_down(base: Duration) -> Duration {
    let mut byte = [0u8; 1];
    if getrandom::getrandom(&mut byte).is_err() {
        return base;
    }
    let millis = base.as_millis() as u64;
    // byte/255 maps to [0, 1]; scale the eighth we are allowed to remove.
    let cut = (millis / 8).saturating_mul(u64::from(byte[0])) / 255;
    Duration::from_millis(millis.saturating_sub(cut).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bound the callers rely on: a jittered sleep stays inside ±12.5%, so
    /// it can never collapse to zero (a tight loop against the host) nor stretch
    /// past a caller's own ceiling by more than an eighth.
    #[test]
    fn jitter_stays_within_an_eighth_either_way() {
        let base = Duration::from_secs(60);
        for _ in 0..500 {
            let d = jittered(base);
            assert!(
                d >= Duration::from_millis(52_500) && d <= Duration::from_millis(67_500),
                "{d:?} escaped ±12.5% of {base:?}"
            );
        }
    }

    /// The property that makes it worth having at all: repeated calls do NOT
    /// return the same value, so a fleet that starts together does not stay
    /// together. (Vanishingly unlikely to flake — 500 draws from a 256-wide
    /// spread.)
    #[test]
    fn successive_draws_differ() {
        let base = Duration::from_secs(60);
        let first = jittered(base);
        assert!(
            (0..500).any(|_| jittered(base) != first),
            "jitter returned a constant — the fleet would stay in lockstep"
        );
    }

    /// A sub-millisecond base has no room to jitter; it must still be a valid,
    /// non-zero delay rather than an accidental busy-loop.
    #[test]
    fn a_tiny_base_never_becomes_zero() {
        assert!(jittered(Duration::from_millis(1)) >= Duration::from_millis(1));
        assert!(jittered(Duration::ZERO) >= Duration::from_millis(1));
        assert!(jittered_down(Duration::from_millis(1)) >= Duration::from_millis(1));
        assert!(jittered_down(Duration::ZERO) >= Duration::from_millis(1));
    }

    /// The property `jittered_down` exists for: it NEVER exceeds the base, so a
    /// caller whose ceiling is a safety margin (refresh before the evidence you
    /// forward on goes stale) can jitter without eating into it.
    #[test]
    fn downward_jitter_never_exceeds_the_base() {
        let base = Duration::from_secs(60);
        for _ in 0..500 {
            let d = jittered_down(base);
            assert!(d <= base, "{d:?} exceeded the ceiling {base:?}");
            assert!(d >= Duration::from_millis(52_500), "{d:?} cut more than an eighth");
        }
    }

    /// It must still actually spread — a constant would leave the fleet in
    /// lockstep, which is the whole reason either of these exists.
    #[test]
    fn downward_jitter_still_varies() {
        let base = Duration::from_secs(60);
        let first = jittered_down(base);
        assert!((0..500).any(|_| jittered_down(base) != first));
    }
}
