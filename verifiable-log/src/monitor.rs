//! The monitor's verification pass over a [`Bundle`], shared by the CLI and the
//! builder's gate suites so there is one verifier (#875).
//!
//! `context` has no default: a FAIL from asking under the wrong tree's context
//! is indistinguishable from a lying log. Tenant invariants live in the
//! builder, so callers supply them; [`unique_invariants`] is the fallback.

use crate::bundle::Bundle;
use crate::log::{TenantInvariant, UniqueDataInvariant, VerifiableLog};
use crate::proof;
use crate::sth::is_equivocation;

/// What a verification pass is checking against.
pub struct VerifyOptions<'a> {
    /// One of [`crate::sth::STH_CONTEXTS`].
    pub context: &'a [u8],
    /// Wall-clock milliseconds, used only to expire rotation-overlap keys. A
    /// skewed clock trusts a retired key slightly too long or retires it early
    /// ("cannot verify"), never a false tampering verdict.
    pub now_ms: u64,
}

/// [`UniqueDataInvariant`] for every named tenant. Deliberately weaker than the
/// builder's real tenant invariants: this crate holds no tenant knowledge.
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
/// `report` is called once per check with `(passed, label)`.
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

    // Using `public_key` alone would make an honest log mid-rotation look
    // tampered with.
    let candidates = bundle.key_candidates(opts.now_ms);
    check(
        !candidates.is_empty(),
        "bundle publishes at least one usable verifying key",
        &mut ok,
    );

    for (i, sth) in bundle.sths.iter().enumerate() {
        let hit = sth.verify_any_with_context(&candidates, opts.context);
        check(
            hit.is_some(),
            &format!("STH[{i}] signature (tree_size={})", sth.tree_size),
            &mut ok,
        );
    }

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
