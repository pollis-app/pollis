//! Track-B fuzz target for `may_rejoin` (I5, the recovery gate —
//! `pollis_core::commands::mls::invariants`). Asserts the SAME property its Kani
//! harness `i5_gate_never_leaks` proves: the result holds IFF both gates hold —
//! `may_rejoin(r, m) <=> (r && m)`. Equivalently, a revoked (`!registered`) OR
//! removed (`!is_member`) device can never rejoin the tree (fuzzer-finding #2).
//!
//! The input space is a 2-bit truth table, so a coverage-guided fuzzer saturates
//! it almost instantly — the value here is that the property is checked against
//! the SAME production function Kani proves, closing the Track-B loop.
//!
//! NEGATIVE CHECK (teeth): build with `--cfg fuzz_mutant` and the target calls a
//! fail-open `registered || is_member` gate; the fuzzer trips the biconditional
//! on `(true, false)` immediately.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use pollis_core::commands::mls::invariants::may_rejoin;

#[derive(Debug, Arbitrary)]
struct Input {
    registered: bool,
    is_member: bool,
}

/// The gate under test. Clean → real `may_rejoin` (AND); `--cfg fuzz_mutant` → a
/// fail-open OR gate that lets a removed-but-registered device rejoin.
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
