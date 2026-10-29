//! Fuzz target for `classify` (I1): replay never `Apply`s across a gap (Kani: `i1_classify_no_gap_apply`).
//!
//! Teeth: `--cfg fuzz_mutant` `Apply`s any present row.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_core::commands::mls::invariants::{classify, ReplayStep};

/// Small epoch domain; the property is universal.
const DOMAIN: u64 = 5;

#[derive(Debug, Arbitrary)]
struct Input {
    current: u64,
    has_next: bool,
    next: u64,
}

/// The classifier under test; `--cfg fuzz_mutant` ignores the gap.
fn run(current_epoch: u64, next_row_epoch: Option<u64>) -> ReplayStep {
    #[cfg(not(fuzz_mutant))]
    {
        classify(current_epoch, next_row_epoch)
    }
    #[cfg(fuzz_mutant)]
    {
        match next_row_epoch {
            None => ReplayStep::Wait,
            // BUG: applies across a gap — a forward-gap row is applied.
            Some(_) => {
                let _ = current_epoch;
                ReplayStep::Apply
            }
        }
    }
}

fuzz_target!(|input: Input| {
    let current = input.current % DOMAIN;
    let next = if input.has_next {
        Some(input.next % DOMAIN)
    } else {
        None
    };

    match run(current, next) {
        // The headline: an Apply is only ever the exact bridging commit.
        ReplayStep::Apply => assert_eq!(
            next,
            Some(current),
            "no-gap-apply violated: Apply with next={next:?}, current={current}"
        ),
        // A gap is a present-but-non-bridging row — never silently skipped.
        ReplayStep::GapRecover => {
            assert!(next.is_some(), "GapRecover with no next row");
            assert_ne!(next, Some(current), "GapRecover on the bridging commit");
        }
        // Wait is exactly "no more rows".
        ReplayStep::Wait => assert!(next.is_none(), "Wait with a present next row {next:?}"),
    }
});
