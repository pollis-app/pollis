//! Fuzz target for `may_rejoin` (I5): `may_rejoin(r, m) <=> (r && m)` (Kani: `i5_gate_never_leaks`).
//!
//! Teeth: `--cfg fuzz_mutant` uses a fail-open OR gate.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_core::commands::mls::invariants::may_rejoin;

#[derive(Debug, Arbitrary)]
struct Input {
    registered: bool,
    is_member: bool,
}

/// The gate under test; `--cfg fuzz_mutant` fails open.
fn run(registered: bool, is_member: bool) -> bool {
    #[cfg(not(fuzz_mutant))]
    {
        may_rejoin(registered, is_member)
    }
    #[cfg(fuzz_mutant)]
    {
        // BUG: OR fails open — a removed member still registered would rejoin.
        registered || is_member
    }
}

fuzz_target!(|input: Input| {
    let allowed = run(input.registered, input.is_member);

    // The full biconditional: rejoin is permitted IFF both gates pass.
    assert_eq!(
        allowed,
        input.registered && input.is_member,
        "recovery gate leaked: may_rejoin({}, {}) = {allowed}, expected {}",
        input.registered,
        input.is_member,
        input.registered && input.is_member
    );
});
