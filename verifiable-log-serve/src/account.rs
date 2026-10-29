//! Per-account key-history verification — the one shared function the static
//! report generator, the auditor CLI and a live endpoint all call.
//!
//! Given the account-key tree and a user id, decide whether that key history is trustworthy:
//!
//! 1. **Trust anchor.** Verify the latest account STH's signature *first* —
//!    under [`account_key::STH_CONTEXT`], against every currently acceptable key.
//! 2. **Membership.** Select the entries whose [`AccountKeyLeaf`] decodes and
//!    whose `user_id` matches, in `seq` order.
//! 3. **Inclusion.** Each selected entry's inclusion proof must verify against
//!    that account STH.
//! 4. **Invariant.** Replay the user's versions through slice 2's
//!    [`AccountKeyInvariant`] — `identity_version` strictly increasing, no
//!    duplicate version. A rejected append is exactly a detected violation.
//!
//! Prerequisite failures return `Err`; a tampered chain yields an [`AccountReport`] with
//! `chain_valid == false`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use verifiable_log::{proof, Entry, InclusionProof, Sth, VerifiableLog};
use verifiable_log_builder::account_key::{
    self, AccountKeyInvariant, AccountKeyLeaf,
};

use crate::bundle::{now_ms, short, Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::remote::{build_agent, fetch_json};

/// Tenant id the account-key entries carry in the shared log.
pub const ACCOUNT_TENANT: &str = account_key::TENANT;

/// One identity-key version in a user's chain, as reported to a caller. Mirrors
/// the structural fields of an [`AccountKeyLeaf`] plus whether its inclusion proof verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyVersion {
    /// Monotonic identity version (1 at signup, +1 per rotation).
    pub identity_version: u64,
    /// Global insertion order (`account_key_log.seq`).
    pub seq: i64,
    /// The ML-DSA-44 account identity public key authoritative at this version,
    /// lowercase hex.
    pub account_id_pub: String,
    /// Did this entry's inclusion proof verify against the latest account STH?
    pub included: bool,
}

/// The structured result of verifying a single user's key history. This is the
/// exact shape the CLI prints and the static `/verify/account/<user_id>` report carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountReport {
    /// The user id that was verified (echoed back).
    pub user_id: String,
    /// Were any key versions found for this user?
    pub found: bool,
    /// Tree size of the account STH everything was checked against.
    pub sth_tree_size: u64,
    /// Root hash of that STH, lowercase hex.
    pub root_hex: String,
    /// The user's key versions, in `seq` order.
    pub keys: Vec<AccountKeyVersion>,
    /// Overall verdict: account STH signature valid (under the account context)
    /// AND every selected entry included AND the account-key invariant holds.
    pub chain_valid: bool,
    /// Human-readable reasons `chain_valid` is false (empty when it is true).
    pub violations: Vec<String>,
}

/// Verify a single user's key-history chain against the account-key tree served
/// at `base_url` (e.g. `http://127.0.0.1:8787`), trusting only the published
/// key.
///
/// Fetches the prerequisites into an in-memory [`Bundle`] for [`verify_account_in_bundle`].
///
/// `Err` only for prerequisite failures; verification failures set `chain_valid = false`.
pub fn verify_account(base_url: &str, user_id: &str) -> Result<AccountReport> {
    verify_account_via(base_url, user_id, None)
}

/// [`verify_account`] with an optional SOCKS5 `proxy` (e.g.
/// `socks5h://127.0.0.1:9050`) so the overlay hides the client IP; `None` is a direct fetch.
pub fn verify_account_via(
    base_url: &str,
    user_id: &str,
    proxy: Option<&str>,
) -> Result<AccountReport> {
    let agent = build_agent(proxy)?;
    // Root-then-signers (#754, #1454): root-authorised signers are trusted alongside the pin.
    let trust = crate::pinned::key_set_trust(&agent, base_url, account_key::STH_CONTEXT);
    verify_account_with(&agent, base_url, user_id, &trust)
}

/// [`verify_account_via`] with the key-set trust decision supplied by the caller, so a
/// caller that also pin-checks the served key uses the same decision; `trust.alarm` fails it.
pub fn verify_account_via_trusting(
    base_url: &str,
    user_id: &str,
    proxy: Option<&str>,
    trust: &crate::pinned::KeySetTrust,
) -> Result<AccountReport> {
    let agent = build_agent(proxy)?;
    verify_account_with(&agent, base_url, user_id, trust)
}

fn verify_account_with(
    agent: &ureq::Agent,
    base_url: &str,
    user_id: &str,
    trust: &crate::pinned::KeySetTrust,
) -> Result<AccountReport> {
    let base = base_url.trim_end_matches('/');

    let pk_doc: PublicKeyDoc =
        fetch_json(agent, &format!("{base}/v1/account-keys/public_key.json"))?;
    // H-1: pin at the fetch boundary; the `*_in_bundle` core is not gated. Only pinned keys pass.
    let pk_doc = crate::pinned::require_pinned_with(&pk_doc, now_ms(), trust)
        .map_err(crate::error::ServeError::BadBundle)?;
    let sth: Sth = fetch_json(agent, &format!("{base}/v1/account-keys/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(agent, &format!("{base}/v1/account-keys/entries.json"))?;

    // Plan which proofs to fetch: only this user's entries (decode + match).
    let user_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == ACCOUNT_TENANT)
        .filter_map(|(i, e)| AccountKeyLeaf::decode(&e.data).ok().map(|leaf| (i, leaf)))
        .filter(|(_, leaf)| leaf.user_id == user_id)
        .map(|(i, _)| i)
        .collect();

    let mut inclusion: Vec<InclusionCheck> = Vec::with_capacity(user_indices.len());
    for i in &user_indices {
        let url = format!("{base}/v1/account-keys/proof/inclusion/{}/{}.json", sth.tree_size, i);
        if let Ok(proof) = fetch_json::<InclusionProof>(agent, &url) {
            if let Some(entry) = entries.get(*i) {
                inclusion.push(InclusionCheck {
                    entry: entry.clone(),
                    proof,
                    sth_index: 0,
                });
            }
        }
    }

    let bundle = Bundle {
        // Carry the overlap set so a head from a retiring key still verifies (#875).
        retired_keys: pk_doc.overlap_keys(),
        public_key: pk_doc.public_key,
        sths: vec![sth],
        entries,
        enforce_unique: vec![ACCOUNT_TENANT.to_string()],
        inclusion,
        consistency: Vec::new(),
    };

    let mut report = verify_account_in_bundle(&bundle, user_id);
    // An unattributable served key set is an alarm even when the pin verifies.
    if let Some(v) = trust.alarm_violation() {
        report.chain_valid = false;
        report.violations.insert(0, v);
    }
    Ok(report)
}

/// Verify a single user's key-history chain against an **already-loaded**
/// account-key [`Bundle`], no IO: the shared verdict core.
///
/// Never panics; failures yield `chain_valid == false` with `violations`.
pub fn verify_account_in_bundle(bundle: &Bundle, user_id: &str) -> AccountReport {
    verify_account_in_bundle_at(bundle, user_id, now_ms())
}

/// [`verify_account_in_bundle`] against an explicit `now_ms`, which decides which
/// rotation-overlap keys have expired.
///
/// The clock is a parameter so the overlap window is testable.
pub fn verify_account_in_bundle_at(
    bundle: &Bundle,
    user_id: &str,
    now_ms: u64,
) -> AccountReport {
    let mut violations: Vec<String> = Vec::new();

    // 1. Trust anchor: the newest head under the account context and any acceptable key.
    let candidates = bundle.key_candidates(now_ms);
    let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
    let (sth_tree_size, root_hex, sth_sig_ok) = match latest {
        Some(sth) => (
            sth.tree_size,
            sth.root_hash.clone(),
            sth.verify_any_with_context(&candidates, account_key::STH_CONTEXT)
                .is_some(),
        ),
        None => (0, String::new(), false),
    };
    if !sth_sig_ok {
        violations
            .push("account STH signature is invalid — published head is not trustworthy".to_string());
    }

    // 2. Membership: this user's account-key leaves with their leaf indices.
    let mut selected: Vec<(usize, AccountKeyLeaf)> = bundle
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == ACCOUNT_TENANT)
        .filter_map(|(i, e)| AccountKeyLeaf::decode(&e.data).ok().map(|leaf| (i, leaf)))
        .filter(|(_, leaf)| leaf.user_id == user_id)
        .collect();
    // Sorted defensively so the reported and replayed chain is unambiguous.
    selected.sort_by_key(|(_, leaf)| leaf.seq);

    let found = !selected.is_empty();

    // Only proofs checked against the newest head.
    let inclusion_by_index: BTreeMap<usize, &InclusionProof> = bundle
        .inclusion
        .iter()
        .filter(|c| latest.is_some_and(|s| c.proof.tree_size == s.tree_size))
        .map(|c| (c.proof.leaf_index as usize, &c.proof))
        .collect();

    let mut keys: Vec<AccountKeyVersion> = Vec::with_capacity(selected.len());
    let mut all_included = true;
    for (leaf_index, leaf) in &selected {
        let included = match (
            inclusion_by_index.get(leaf_index),
            latest,
            bundle.entries.get(*leaf_index),
        ) {
            (Some(proof), Some(sth), Some(entry)) => {
                proof::verify_inclusion_proof(entry, proof, sth)
            }
            _ => false,
        };
        if !included {
            all_included = false;
            violations.push(format!(
                "key version {} (seq {}) is not provably included in the signed account tree",
                leaf.identity_version, leaf.seq
            ));
        }
        keys.push(AccountKeyVersion {
            identity_version: leaf.identity_version,
            seq: leaf.seq,
            account_id_pub: leaf.account_id_pub.clone(),
            included,
        });
    }

    // 4. Invariant: replay through the account-key rules; a rejected append is a violation.
    let mut log = VerifiableLog::new();
    log.register_invariant(ACCOUNT_TENANT, Box::new(AccountKeyInvariant));
    let mut invariant_ok = true;
    for (_, leaf) in &selected {
        match leaf.to_entry() {
            Ok(entry) => {
                if let Err(violation) = log.append(entry) {
                    invariant_ok = false;
                    violations.push(violation.to_string());
                }
            }
            Err(e) => {
                invariant_ok = false;
                violations.push(format!(
                    "key version {} (seq {}) failed to re-encode: {e}",
                    leaf.identity_version, leaf.seq
                ));
            }
        }
    }

    let chain_valid = sth_sig_ok && all_included && invariant_ok;

    AccountReport {
        user_id: user_id.to_string(),
        found,
        sth_tree_size,
        root_hex,
        keys,
        chain_valid,
        violations,
    }
}

impl AccountReport {
    /// Print a human-readable report to stdout (used by the CLI's text mode).
    pub fn print(&self) {
        println!("User:    {}", self.user_id);
        println!("Found:   {}", if self.found { "yes" } else { "no" });
        println!("STH:     tree_size {}  root {}", self.sth_tree_size, self.root_hex);
        if self.keys.is_empty() {
            println!("Keys:    (none)");
        } else {
            println!("Key history (seq order):");
            for k in &self.keys {
                println!(
                    "  v{:<4} seq {:<6} key {}  {}",
                    k.identity_version,
                    k.seq,
                    short(&k.account_id_pub),
                    if k.included { "[included \u{2713}]" } else { "[MISSING \u{2717}]" },
                );
            }
        }
        if !self.violations.is_empty() {
            println!("Violations:");
            for v in &self.violations {
                println!("  - {v}");
            }
        }
        println!(
            "\n{}",
            if self.chain_valid {
                "PASS: account key chain is valid"
            } else {
                "FAIL: account key chain is NOT valid"
            }
        );
    }
}
