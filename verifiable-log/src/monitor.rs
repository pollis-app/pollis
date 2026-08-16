//! The monitor's verification pass, as a library.
//!
//! Everything the `monitor` CLI does to a [`Bundle`] lives here so it can be
//! *called* rather than re-implemented. Before #875 it existed only inside the
//! binary, so the builder's account-key and binaries gate suites each carried a
//! hand-written copy of the loop — and both copies asserted the CLI's own
//! limitation (it could only ever check the commit-log context) as though it
//! were the intended behaviour. A verifier whose logic exists in three places is
//! a verifier whose three answers can disagree.
//!
//! The pass is deliberately parameterised on the two things that differ between
//! trees and callers:
//!
//! * `context` — the STH domain separation. There is no default: verifying an
//!   account-key head under the commit-log context fails, and a FAIL that means
//!   "you asked the wrong question" is indistinguishable from one that means
//!   "this log is lying".
//! * `invariants` — the tenant rules replayed over `entries`. The concrete
//!   tenant invariants live in `verifiable-log-builder`, above this crate, so
//!   the caller supplies them; [`unique_invariants`] is the generic fallback the
//!   CLI uses when it only has the bundle's `enforce_unique` list.

use crate::bundle::Bundle;
use crate::log::{TenantInvariant, UniqueDataInvariant, VerifiableLog};
use crate::proof;
use crate::sth::is_equivocation;

/// What a verification pass is checking against.
pub struct VerifyOptions<'a> {
    /// STH domain-separation context — one of [`crate::sth::STH_CONTEXTS`].
    pub context: &'a [u8],
    /// Wall-clock milliseconds, used only to expire rotation-overlap keys.
    ///
    /// A verifier is allowed a clock (the builder is not — its output must be
    /// deterministic). A clock wrong in the past keeps a retired key trusted
    /// slightly too long; one wrong in the future retires it early and degrades
    /// to "cannot verify", never to a false accusation of tampering.
    pub now_ms: u64,
}

/// [`UniqueDataInvariant`] for every named tenant — the generic replay rule a
/// verifier can apply knowing nothing about a tenant beyond "leaves don't
/// repeat". Weaker than a tenant's real invariant (which lives in the builder),
/// and deliberately so: this crate must not grow tenant knowledge.
pub fn unique_invariants(tenants: &[String]) -> Vec<(String, Box<dyn TenantInvariant>)> {
    tenants
        .iter()
        .map(|t| {
            let boxed: Box<dyn TenantInvariant> = Box::new(UniqueDataInvariant);
            (t.clone(), boxed)
        })
        .collect()
}

/// Run every check the monitor performs and return the overall verdict.
///
/// `report` is called once per check with `(passed, label)` so a CLI can print a
/// running PASS/FAIL log while a test can ignore it entirely.
pub fn verify_bundle(
    bundle: &Bundle,
    opts: &VerifyOptions<'_>,
    invariants: Vec<(String, Box<dyn TenantInvariant>)>,
    report: &mut dyn FnMut(bool, &str),
) -> bool {
    let mut ok = true;
    let mut check = |passed: bool, label: &str, ok: &mut bool| {
        report(passed, label);
        if !passed {
            *ok = false;
        }
    };

    // The rotation-overlap key set: the active key plus any retired key still
    // inside its window. Using `public_key` alone makes an honest log mid-
    // rotation look tampered with.
    let candidates = bundle.key_candidates(opts.now_ms);
    check(
        !candidates.is_empty(),
        "bundle publishes at least one usable verifying key",
        &mut ok,
    );

    // 1. STH signatures, under this tree's context and any acceptable key.
    for (i, sth) in bundle.sths.iter().enumerate() {
        let hit = sth.verify_any_with_context(&candidates, opts.context);
        check(
            hit.is_some(),
            &format!("STH[{i}] signature (tree_size={})", sth.tree_size),
            &mut ok,
        );
    }

    // 2. Equivocation: any two STHs at the same size with different roots.
    for i in 0..bundle.sths.len() {
        for j in (i + 1)..bundle.sths.len() {
            let equivocates = is_equivocation(&bundle.sths[i], &bundle.sths[j]);
            check(
                !equivocates,
                &format!(
                    "no equivocation between STH[{i}] and STH[{j}] (tree_size={})",
                    bundle.sths[i].tree_size
                ),
                &mut ok,
            );
        }
    }

    // 3. Replay entries: run tenant invariants and confirm every STH root.
    if !bundle.entries.is_empty() {
        let mut log = VerifiableLog::new();
        for (tenant, invariant) in invariants {
            log.register_invariant(tenant, invariant);
        }
        let mut replay_ok = true;
        for (i, entry) in bundle.entries.iter().enumerate() {
            if let Err(e) = log.append(entry.clone()) {
                check(
                    false,
                    &format!("entry[{i}] rejected by tenant invariant: {e}"),
                    &mut ok,
                );
                replay_ok = false;
            }
        }
        check(replay_ok, "all entries satisfy tenant invariants", &mut ok);

        if replay_ok {
            for (i, sth) in bundle.sths.iter().enumerate() {
                let size = sth.tree_size as usize;
                let matches = match log.root_at(size) {
                    Ok(root) => sth.root_bytes().map(|r| r == root).unwrap_or(false),
                    Err(_) => false,
                };
                check(
                    matches,
                    &format!("STH[{i}] root matches replayed entries"),
                    &mut ok,
                );
            }
        }
    }

    // 4. Inclusion proofs.
    for (i, c) in bundle.inclusion.iter().enumerate() {
        let passed = bundle
            .sths
            .get(c.sth_index)
            .map(|s| proof::verify_inclusion_proof(&c.entry, &c.proof, s))
            .unwrap_or(false);
        check(
            passed,
            &format!(
                "inclusion[{i}] leaf {} in STH[{}]",
                c.proof.leaf_index, c.sth_index
            ),
            &mut ok,
        );
    }

    // 5. Consistency proofs.
    for (i, c) in bundle.consistency.iter().enumerate() {
        let passed = match (bundle.sths.get(c.old_index), bundle.sths.get(c.new_index)) {
            (Some(o), Some(n)) => proof::verify_consistency_proof(o, n, &c.proof),
            _ => false,
        };
        check(
            passed,
            &format!(
                "consistency[{i}] STH[{}] -> STH[{}]",
                c.old_index, c.new_index
            ),
            &mut ok,
        );
    }

    ok
}
