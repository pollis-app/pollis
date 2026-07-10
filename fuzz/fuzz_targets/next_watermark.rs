//! Track-B fuzz target for `next_watermark` (I3, the delivery-watermark safety
//! function — `pollis_core::commands::messages::watermark`). Asserts the SAME
//! three properties its Kani harnesses prove (see `watermark.rs` P1/P2/P3):
//!
//!   * **P1 (no-skip / anti-F3):** the returned watermark is STRICTLY below the
//!     `sent_at` of the first un-handled envelope — the next `sent_at > watermark`
//!     fetch can never drop an un-decrypted message.
//!   * **P2 (monotone):** the watermark over a prefix `<=` over the full slice.
//!   * **P3 (handled-liveness):** if every envelope is handled, the watermark
//!     equals the max `sent_at`.
//!
//! Like the Kani harness this fuzzes SMALL FIXED-DOMAIN INTEGER envelopes (not
//! Strings), sorted ascending by key to match the real caller's
//! `ORDER BY sent_at ASC`. P2/P3 are only clean under distinct keys (a
//! handled/un-handled `sent_at` tie correctly pulls the cursor back), so they are
//! asserted only when the generated keys are strictly increasing; P1 is asserted
//! on every input, ties included — ties are exactly where the `>=`-vs-`>` mutant
//! trips.
//!
//! NEGATIVE CHECK (teeth): build with `--cfg fuzz_mutant` and the target computes
//! the watermark with a buggy `>` (instead of `>=`) break, which lets the cursor
//! advance onto a `sent_at` tie and violates P1 — the fuzzer finds it fast.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_core::commands::messages::watermark::{next_watermark, EnvKind};

/// Bound on the generated envelope count — keeps inputs tiny (the tie / no-skip
/// counterexamples are 2-3-element phenomena, matching the Kani bound of 4).
const MAX_LEN: usize = 6;
/// Small integer domain for keys and epochs (mirrors the Kani `0..=3` domain,
/// widened slightly). Kept `< MAX_LEN` so distinct-key fills stay satisfiable.
const DOMAIN: u8 = 5;

#[derive(Debug, Arbitrary)]
struct RawEnv {
    key: u8,
    kind: u8,
    has_epoch: bool,
    epoch: u8,
}

#[derive(Debug, Arbitrary)]
struct Input {
    envs: Vec<RawEnv>,
    has_max: bool,
    max: u8,
    cut: u8,
}

fn kind_of(raw: u8) -> EnvKind {
    match raw % 4 {
        0 => EnvKind::Message,
        1 => EnvKind::Edit,
        2 => EnvKind::Delete,
        _ => EnvKind::Other,
    }
}

/// Independent oracle for the private `is_handled` — replicated verbatim so the
/// property check does not depend on the function under test.
fn handled(kind: EnvKind, epoch: Option<u64>, max_fired: Option<u64>) -> bool {
    match kind {
        EnvKind::Message | EnvKind::Edit => match (epoch, max_fired) {
            (Some(e), Some(max)) => e <= max,
            (None, _) => true,
            (Some(_), None) => false,
        },
        EnvKind::Delete | EnvKind::Other => true,
    }
}

/// The watermark under test. Clean build → the REAL production `next_watermark`.
/// `--cfg fuzz_mutant` → a buggy copy that breaks on `>` instead of `>=`, letting
/// the cursor advance onto a handled/un-handled `sent_at` tie (the F3 bug).
fn compute(envs: &[(u8, EnvKind, Option<u64>)], max_fired: Option<u64>) -> Option<u8> {
    #[cfg(not(fuzz_mutant))]
    {
        next_watermark(envs, max_fired)
    }
    #[cfg(fuzz_mutant)]
    {
        let stop_at: Option<&u8> = envs
            .iter()
            .find(|(_, kind, epoch)| !handled(*kind, *epoch, max_fired))
            .map(|(sent_at, _, _)| sent_at);
        let mut candidate: Option<u8> = None;
        for (sent_at, _, _) in envs {
            if let Some(stop) = stop_at {
                // BUG: `>` lets a tie through where the real code uses `>=`.
                if sent_at > stop {
                    break;
                }
            }
            candidate = Some(*sent_at);
        }
        candidate
    }
}

fuzz_target!(|input: Input| {
    // Build a bounded, `sent_at`-ascending (ties allowed) envelope table.
    let mut envs: Vec<(u8, EnvKind, Option<u64>)> = input
        .envs
        .iter()
        .take(MAX_LEN)
        .map(|e| {
            let epoch = if e.has_epoch {
                Some((e.epoch % DOMAIN) as u64)
            } else {
                None
            };
            (e.key % DOMAIN, kind_of(e.kind), epoch)
        })
        .collect();
    envs.sort_by_key(|(k, _, _)| *k);

    let max_fired = if input.has_max {
        Some((input.max % DOMAIN) as u64)
    } else {
        None
    };

    // ── P1 (no-skip / anti-F3) — asserted on EVERY input, ties included. ──
    let first_unhandled = envs
        .iter()
        .find(|(_, kind, epoch)| !handled(*kind, *epoch, max_fired))
        .map(|(k, _, _)| *k);
    let wm = compute(&envs, max_fired);
    if let (Some(stop), Some(w)) = (first_unhandled, wm) {
        assert!(
            w < stop,
            "P1 violated: watermark {w} not strictly below first un-handled sent_at {stop} (envs={envs:?}, max_fired={max_fired:?})"
        );
    }

    // P2/P3 are clean only under strictly-increasing keys (no `sent_at` tie).
    let distinct = envs.windows(2).all(|w| w[0].0 < w[1].0);
    if !distinct {
        return;
    }

    // ── P2 (monotone): prefix cursor <= full-slice cursor. ──
    let cut = (input.cut as usize) % (envs.len() + 1);
    let wm_prefix = compute(&envs[..cut], max_fired);
    let wm_full = compute(&envs, max_fired);
    assert!(
        wm_prefix <= wm_full,
        "P2 violated: prefix watermark {wm_prefix:?} > full watermark {wm_full:?} (envs={envs:?}, cut={cut}, max_fired={max_fired:?})"
    );

    // ── P3 (handled-liveness): all handled ⇒ watermark == max sent_at. ──
    let all_handled = envs
        .iter()
        .all(|(_, kind, epoch)| handled(*kind, *epoch, max_fired));
    if all_handled {
        match envs.last() {
            Some((max_key, _, _)) => assert_eq!(
                wm_full,
                Some(*max_key),
                "P3 violated: all-handled watermark {wm_full:?} != max sent_at {max_key} (envs={envs:?}, max_fired={max_fired:?})"
            ),
            None => assert!(wm_full.is_none(), "P3 violated: empty slice must yield None"),
        }
    }
});
