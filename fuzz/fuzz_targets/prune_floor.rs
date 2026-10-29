//! Fuzz target for `prune_floor` (I4), asserting the same properties as its Kani harnesses:
//!
//!   * P1 (floor_non_negative): `prune_floor(..) >= 0`.
//!   * P2 (NoLossForCurrentMember): with the whole roster reported and
//!     `min_since == Some(m)`, the ONLY way the floor exceeds `m` is Tier 2
//!     binding (`head - PRUNE_MAX_BEHIND_HEAD > m`) — Tier 1 never prunes past
//!     the slowest current member.
//!   * P3 (unreported_disables_tier1): with `all_reported == false` the floor is
//!     EXACTLY `(head - PRUNE_MAX_BEHIND_HEAD).max(0)` for ANY `min_since`.
//!
//! `DOMAIN` exceeds `PRUNE_MAX_BEHIND_HEAD` so Tier 2 can bind (else P2 is vacuous).
//!
//! Teeth: `--cfg fuzz_mutant` adds the slack instead of subtracting it, tripping P2.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_delivery::commit::{prune_floor, PRUNE_MAX_BEHIND_HEAD, PRUNE_SLACK_EPOCHS};

/// Reduce raw epochs into `0..=DOMAIN`.
const DOMAIN: i64 = 1024;

#[derive(Debug, Arbitrary)]
struct Input {
    has_min: bool,
    min_since: i64,
    all_reported: bool,
    head: i64,
}

/// The floor under test; `--cfg fuzz_mutant` swaps in the slack bug.
fn run(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
    #[cfg(not(fuzz_mutant))]
    {
        prune_floor(min_since, all_reported, head)
    }
    #[cfg(fuzz_mutant)]
    {
        // BUG: `+ SLACK` prunes SLACK epochs PAST the slowest member (violates P2).
        let tier1 = match (all_reported, min_since) {
            (true, Some(m)) => (m + PRUNE_SLACK_EPOCHS).max(0),
            _ => 0,
        };
        let tier2 = (head - PRUNE_MAX_BEHIND_HEAD).max(0);
        tier1.max(tier2)
    }
}

fuzz_target!(|input: Input| {
    let head = input.head.rem_euclid(DOMAIN + 1);
    let min_since = if input.has_min {
        Some(input.min_since.rem_euclid(DOMAIN + 1))
    } else {
        None
    };

    let floor = run(min_since, input.all_reported, head);

    assert!(floor >= 0, "P1 floor_non_negative violated: floor={floor}");

    // P3: an unreported roster disables Tier 1 — the floor is exactly Tier 2.
    if !input.all_reported {
        assert_eq!(
            floor,
            (head - PRUNE_MAX_BEHIND_HEAD).max(0),
            "P3 unreported_disables_tier1 violated: min_since={min_since:?}, head={head}"
        );
    }

    // P2: with the whole roster reported, the floor exceeds the slowest member
    // ONLY when Tier 2 binds — Tier 1 never prunes past a current member.
    if let (true, Some(m)) = (input.all_reported, min_since) {
        if floor > m {
            assert!(
                head - PRUNE_MAX_BEHIND_HEAD > m,
                "P2 NoLossForCurrentMember violated: floor={floor} > m={m} without Tier 2 (head={head})"
            );
        }
    }
});
