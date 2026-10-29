//! Fuzz target for `resolve` (I2, #411), asserting both directions of Kani's `i2_resolve_sound`:
//!
//!   * **`Adopt` ⟹ `stored_at_epoch == Some(ours)`** — never adopt a foreign
//!     commit (no phantom epoch, no fork).
//!   * **`Rollback` ⟹ `stored_at_epoch != Some(ours)`** — never discard a landed
//!     own commit (no wedge).
//!
//! States where `Committed` disagrees with the stored bytes are unreachable and skipped.
//!
//! Teeth: `--cfg fuzz_mutant` adopts ambiguous `LostRace`/`Failed` without consulting the log.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_core::commands::mls::invariants::{resolve, Resolution, SubmitOutcome};

/// Bound on the symbolic commit-byte length (mirrors the Kani `N = 3`).
const MAX_BYTES: usize = 4;

#[derive(Debug, Arbitrary)]
struct Input {
    outcome: u8,
    ours: Vec<u8>,
    has_stored: bool,
    stored: Vec<u8>,
}

fn outcome_of(raw: u8) -> SubmitOutcome {
    match raw % 3 {
        0 => SubmitOutcome::Committed,
        1 => SubmitOutcome::LostRace,
        _ => SubmitOutcome::Failed,
    }
}

/// The resolver under test; `--cfg fuzz_mutant` adopts ambiguous outcomes.
fn run(outcome: SubmitOutcome, ours: &[u8], stored: Option<&[u8]>) -> Resolution {
    #[cfg(not(fuzz_mutant))]
    {
        resolve(outcome, ours, stored)
    }
    #[cfg(fuzz_mutant)]
    {
        let _ = (ours, stored);
        match outcome {
            SubmitOutcome::Committed => Resolution::Adopt,
            // BUG: adopts without checking the log — grafts a foreign commit.
            SubmitOutcome::LostRace | SubmitOutcome::Failed => Resolution::Adopt,
        }
    }
}

fuzz_target!(|input: Input| {
    // Tiny byte domain so equal and unequal are both reachable.
    let ours: Vec<u8> = input.ours.iter().take(MAX_BYTES).map(|b| b % 2).collect();
    let stored_vec: Vec<u8> = input.stored.iter().take(MAX_BYTES).map(|b| b % 2).collect();
    let stored: Option<&[u8]> = if input.has_stored {
        Some(&stored_vec[..])
    } else {
        None
    };
    let outcome = outcome_of(input.outcome);

    // `Committed` ⇒ the log holds our bytes; skip states violating that.
    if outcome == SubmitOutcome::Committed && stored != Some(&ours[..]) {
        return;
    }

    match run(outcome, &ours[..], stored) {
        // No foreign adopt: an adopted commit is exactly the one at this epoch.
        Resolution::Adopt => assert_eq!(
            stored,
            Some(&ours[..]),
            "no-foreign-adopt violated: Adopt but log holds {stored:?}, ours={ours:?}, outcome={outcome:?}"
        ),
        // No own rollback: we only roll back when the log does NOT hold ours.
        Resolution::Rollback => assert_ne!(
            stored,
            Some(&ours[..]),
            "own-rollback violated: Rollback but log holds our bytes {ours:?}, outcome={outcome:?}"
        ),
    }
});
