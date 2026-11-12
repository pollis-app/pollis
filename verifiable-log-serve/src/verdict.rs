//! The steps the single-subject verifiers ([`crate::account`], [`crate::group`],
//! [`crate::release`]) share: fetching a one-head [`Bundle`] for one subject, anchoring the
//! verdict to the newest signed head, and printing the verdict's tail.

use std::collections::BTreeMap;

use verifiable_log::{proof, Entry, InclusionProof, Sth};

use crate::bundle::{now_ms, Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::pinned::KeySetTrust;
use crate::remote::fetch_json;

/// Fetch the tree served under `prefix` into a one-head [`Bundle`] for the `*_in_bundle`
/// core: the pinned public key, the latest head, every entry, and the inclusion proofs of
/// the entries `selects` picks. A failed proof fetch omits the proof, so the core marks
/// that entry not-included.
pub(crate) fn fetch_subject_bundle(
    agent: &ureq::Agent,
    prefix: &str,
    trust: &KeySetTrust,
    tenant: &str,
    selects: impl Fn(&Entry) -> bool,
) -> Result<Bundle> {
    let pk_doc: PublicKeyDoc = fetch_json(agent, &format!("{prefix}/public_key.json"))?;
    // H-1: pin at the fetch boundary; the `*_in_bundle` core is not gated. Only pinned keys pass.
    let pk_doc = crate::pinned::require_pinned_with(&pk_doc, now_ms(), trust)
        .map_err(crate::error::ServeError::BadBundle)?;
    let sth: Sth = fetch_json(agent, &format!("{prefix}/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(agent, &format!("{prefix}/entries.json"))?;

    let mut inclusion: Vec<InclusionCheck> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if !selects(entry) {
            continue;
        }
        let url = format!("{prefix}/proof/inclusion/{}/{}.json", sth.tree_size, i);
        if let Ok(proof) = fetch_json::<InclusionProof>(agent, &url) {
            inclusion.push(InclusionCheck {
                entry: entry.clone(),
                proof,
                sth_index: 0,
            });
        }
    }

    Ok(Bundle {
        // Carry the overlap set so a head from a retiring key still verifies (#875).
        retired_keys: pk_doc.overlap_keys(),
        public_key: pk_doc.public_key,
        sths: vec![sth],
        entries,
        enforce_unique: vec![tenant.to_string()],
        inclusion,
        consistency: Vec::new(),
    })
}

/// An unattributable served key set is an alarm even when the pin verifies: fail the
/// verdict and put the alarm first.
pub(crate) fn raise_key_set_alarm(
    trust: &KeySetTrust,
    chain_valid: &mut bool,
    violations: &mut Vec<String>,
) {
    if let Some(v) = trust.alarm_violation() {
        *chain_valid = false;
        violations.insert(0, v);
    }
}

/// A verdict's trust anchor: the newest head in a bundle, whether its signature verifies
/// under `context`, and the inclusion proofs checked against it.
pub(crate) struct Anchor<'a> {
    latest: Option<&'a Sth>,
    pub(crate) sth_tree_size: u64,
    pub(crate) root_hex: String,
    pub(crate) sth_sig_ok: bool,
    inclusion_by_index: BTreeMap<usize, &'a InclusionProof>,
}

impl<'a> Anchor<'a> {
    /// Anchor to the newest head in `bundle`, verified under any key acceptable at `now_ms`.
    pub(crate) fn new(bundle: &'a Bundle, now_ms: u64, context: &[u8]) -> Self {
        let candidates = bundle.key_candidates(now_ms);
        let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
        let (sth_tree_size, root_hex, sth_sig_ok) = match latest {
            Some(sth) => (
                sth.tree_size,
                sth.root_hash.clone(),
                sth.verify_any_with_context(&candidates, context).is_some(),
            ),
            None => (0, String::new(), false),
        };
        // Only proofs checked against the newest head, which the report is anchored to.
        let inclusion_by_index = bundle
            .inclusion
            .iter()
            .filter(|c| latest.is_some_and(|s| c.proof.tree_size == s.tree_size))
            .map(|c| (c.proof.leaf_index as usize, &c.proof))
            .collect();
        Self {
            latest,
            sth_tree_size,
            root_hex,
            sth_sig_ok,
            inclusion_by_index,
        }
    }

    /// Whether the entry at `leaf_index` is provably included under the anchored head.
    pub(crate) fn included(&self, bundle: &Bundle, leaf_index: usize) -> bool {
        match (
            self.inclusion_by_index.get(&leaf_index),
            self.latest,
            bundle.entries.get(leaf_index),
        ) {
            (Some(proof), Some(sth), Some(entry)) => {
                proof::verify_inclusion_proof(entry, proof, sth)
            }
            _ => false,
        }
    }
}

/// Print a verdict's tail for the CLI's text mode: its violations, then `pass` or `fail`.
pub(crate) fn print_verdict(violations: &[String], chain_valid: bool, pass: &str, fail: &str) {
    if !violations.is_empty() {
        println!("Violations:");
        for v in violations {
            println!("  - {v}");
        }
    }
    println!("\n{}", if chain_valid { pass } else { fail });
}
