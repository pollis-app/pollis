//! The shared jitter for fleet-wide repeating schedules, so devices don't wake
//! in lockstep (#875). Deliberately not a retry helper: the schedules retry
//! different errors on different delays; only the spread is shared.

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
/// For schedules whose ceiling is a safety margin (e.g. refreshing before a
/// forwarded revocation list goes stale). Callers with a floor still clamp.
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

    /// Vanishingly unlikely to flake: 500 draws from a 256-wide spread.
    #[test]
    fn successive_draws_differ() {
        let base = Duration::from_secs(60);
        let first = jittered(base);
        assert!(
            (0..500).any(|_| jittered(base) != first),
            "jitter returned a constant — the fleet would stay in lockstep"
        );
    }

    #[test]
    fn a_tiny_base_never_becomes_zero() {
        assert!(jittered(Duration::from_millis(1)) >= Duration::from_millis(1));
        assert!(jittered(Duration::ZERO) >= Duration::from_millis(1));
        assert!(jittered_down(Duration::from_millis(1)) >= Duration::from_millis(1));
        assert!(jittered_down(Duration::ZERO) >= Duration::from_millis(1));
    }

    #[test]
    fn downward_jitter_never_exceeds_the_base() {
        let base = Duration::from_secs(60);
        for _ in 0..500 {
            let d = jittered_down(base);
            assert!(d <= base, "{d:?} exceeded the ceiling {base:?}");
            assert!(d >= Duration::from_millis(52_500), "{d:?} cut more than an eighth");
        }
    }

    #[test]
    fn downward_jitter_still_varies() {
        let base = Duration::from_secs(60);
        let first = jittered_down(base);
        assert!((0..500).any(|_| jittered_down(base) != first));
    }
}
