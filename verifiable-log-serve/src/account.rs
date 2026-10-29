//! Per-account key-history verification: the one function the static report
//! generator and the auditor CLI share, so their verdicts can't diverge. The
//! account-key analogue of [`crate::group`].
//!
//! The account STH is verified under the domain-separated
//! [`account_key::STH_CONTEXT`], so a commit-log head can never stand in for an
//! account head even though the same key signs both. Transport/parse failures
//! are `Err`; a tampered chain is an [`AccountReport`] with `chain_valid == false`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use verifiable_log::{proof, Entry, InclusionProof, Sth, VerifiableLog};
use verifiable_log_builder::account_key::{
    self, AccountKeyInvariant, AccountKeyLeaf,
};

use crate::bundle::{now_ms, short, Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::remote::{build_agent, fetch_json};

/// Tenant id of account-key entries in the shared log.
pub const ACCOUNT_TENANT: &str = account_key::TENANT;

/// One identity-key version in a user's chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyVersion {
    /// Monotonic identity version (1 at signup, +1 per rotation).
    pub identity_version: u64,
    /// Global insertion order (`account_key_log.seq`).
    pub seq: i64,
    /// ML-DSA-44 account identity public key at this version, lowercase hex.
    pub account_id_pub: String,
    /// Inclusion proof verified against the latest account STH.
    pub included: bool,
}

/// Result of verifying one user's key history; the shape both the CLI and the
/// static `/verify/account/<user_id>` report carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountReport {
    pub user_id: String,
    pub found: bool,
    /// Tree size of the account STH everything was checked against.
    pub sth_tree_size: u64,
    pub root_hex: String,
    pub keys: Vec<AccountKeyVersion>,
    /// Account STH signature valid AND every entry included AND the invariant holds.
    pub chain_valid: bool,
    /// Why `chain_valid` is false (empty when true).
    pub violations: Vec<String>,
}

/// Verify a user's key-history chain against the account-key tree at
/// `base_url`, trusting only the pinned key. `Err` only for transport/parse
/// failures; verification failures are `chain_valid = false`.
pub fn verify_account(base_url: &str, user_id: &str) -> Result<AccountReport> {
    verify_account_via(base_url, user_id, None)
}

/// [`verify_account`] with an optional SOCKS5 `proxy`; see
/// [`crate::group::verify_group_via`].
pub fn verify_account_via(
    base_url: &str,
    user_id: &str,
    proxy: Option<&str>,
) -> Result<AccountReport> {
    let agent = build_agent(proxy)?;
    // Root-authorised signers are trusted alongside the compiled pin (#754, #1454).
    let trust = crate::pinned::key_set_trust(&agent, base_url, account_key::STH_CONTEXT);
    verify_account_with(&agent, base_url, user_id, &trust)
}

/// [`verify_account_via`] with a caller-supplied key-set decision; see
/// [`crate::group::verify_group_via_trusting`].
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
    // H-1: pin at the fetch boundary; see `group::verify_group_with`.
    let pk_doc = crate::pinned::require_pinned_with(&pk_doc, now_ms(), trust)
        .map_err(crate::error::ServeError::BadBundle)?;
    let sth: Sth = fetch_json(agent, &format!("{base}/v1/account-keys/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(agent, &format!("{base}/v1/account-keys/entries.json"))?;

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
        // Keep the rotation-overlap set, or mid-rotation alarms (#875).
        retired_keys: pk_doc.overlap_keys(),
        public_key: pk_doc.public_key,
        sths: vec![sth],
        entries,
        enforce_unique: vec![ACCOUNT_TENANT.to_string()],
        inclusion,
        consistency: Vec::new(),
    };

    let mut report = verify_account_in_bundle(&bundle, user_id);
    // An unattributable served key set is an alarm even under the compiled pin.
    if let Some(v) = trust.alarm_violation() {
        report.chain_valid = false;
        report.violations.insert(0, v);
    }
    Ok(report)
}

/// Verify a user's chain against an already-loaded account-key [`Bundle`], no
/// IO: the shared verdict core behind [`verify_account`] and the static report
/// generator. Never panics.
pub fn verify_account_in_bundle(bundle: &Bundle, user_id: &str) -> AccountReport {
    verify_account_in_bundle_at(bundle, user_id, now_ms())
}

/// [`verify_account_in_bundle`] against an explicit `now_ms`, which decides
/// which rotation-overlap keys have expired (testable, deterministic).
pub fn verify_account_in_bundle_at(
    bundle: &Bundle,
    user_id: &str,
    now_ms: u64,
) -> AccountReport {
    let mut violations: Vec<String> = Vec::new();

    // Expiry is applied here, not trusted from the server.
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

    let mut selected: Vec<(usize, AccountKeyLeaf)> = bundle
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == ACCOUNT_TENANT)
        .filter_map(|(i, e)| AccountKeyLeaf::decode(&e.data).ok().map(|leaf| (i, leaf)))
        .filter(|(_, leaf)| leaf.user_id == user_id)
        .collect();
    // Sort defensively so the reported and replayed chain is source-order independent.
    selected.sort_by_key(|(_, leaf)| leaf.seq);

    let found = !selected.is_empty();

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

    // A rejected append is exactly a detected invariant violation.
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
