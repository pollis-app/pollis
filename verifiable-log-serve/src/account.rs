//! Per-account key-history verification — the one shared function the static
//! report generator, the auditor CLI, and a future live endpoint all call, so
//! their verdicts can never diverge. This is the account-key tenant's analogue
//! of [`crate::group`].
//!
//! Given the published account-key tree (served under `/v1/account-keys/...`)
//! and a user id, it isolates that user's identity-key history and decides
//! whether the chain is trustworthy:
//!
//! 1. **Trust anchor.** Verify the latest account STH's signature *first* —
//!    crucially under the account tree's domain-separated
//!    [`account_key::STH_CONTEXT`], so a commit-log head can never stand in for
//!    an account head even though the same key signs both.
//! 2. **Membership.** Select the entries whose [`AccountKeyLeaf`] decodes and
//!    whose `user_id` matches, in `seq` order.
//! 3. **Inclusion.** Each selected entry's inclusion proof must verify against
//!    that account STH (reusing slice 1's
//!    [`verifiable_log::proof::verify_inclusion_proof`]).
//! 4. **Invariant.** Replay the user's versions through slice 2's
//!    [`AccountKeyInvariant`] — `identity_version` strictly increasing, no
//!    duplicate version. A rejected append is exactly a detected violation.
//!
//! Every cryptographic check is reused from slices 1–2; nothing here
//! reimplements Merkle, proof, signature, or invariant logic. Transport/parse
//! failures for the prerequisites return `Err`; a tampered/forked/regressed
//! chain is **not** an error — it yields an [`AccountReport`] with
//! `chain_valid == false` and populated `violations`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use verifiable_log::{
    proof, verifying_key_from_hex, Entry, InclusionProof, Sth, VerifiableLog,
};
use verifiable_log_builder::account_key::{
    self, AccountKeyInvariant, AccountKeyLeaf,
};

use crate::bundle::{Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::remote::{build_agent, fetch_json};

/// Tenant id the account-key entries carry in the shared log. Re-exported from
/// the builder so layout/remote can filter on it without reaching across crates.
pub const ACCOUNT_TENANT: &str = account_key::TENANT;

/// One identity-key version in a user's chain, as reported to a caller. Mirrors
/// the structural fields of an [`AccountKeyLeaf`] plus whether its inclusion
/// proof checked out against the signed head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyVersion {
    /// Monotonic identity version (1 at signup, +1 per rotation).
    pub identity_version: u64,
    /// Global insertion order (`account_key_log.seq`).
    pub seq: i64,
    /// The Ed25519 account identity public key authoritative at this version,
    /// lowercase hex.
    pub account_id_pub: String,
    /// Did this entry's inclusion proof verify against the latest account STH?
    pub included: bool,
}

/// The structured result of verifying a single user's key history. This is the
/// exact shape the CLI prints and the static `/verify/account/<user_id>` report
/// carries — same function, same output, so they can never disagree.
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
/// A thin transport wrapper around [`verify_account_in_bundle`]: it fetches the
/// account tree's prerequisites (`account-keys/public_key.json`,
/// `account-keys/sth/latest.json`, `account-keys/entries.json`) and this user's
/// inclusion proofs into an in-memory [`Bundle`], then hands them to the shared
/// core — the one place the verdict is computed.
///
/// Returns `Err` only for transport/parse failures of the prerequisites; any
/// *verification* failure is folded into the report as `chain_valid = false`.
pub fn verify_account(base_url: &str, user_id: &str) -> Result<AccountReport> {
    verify_account_via(base_url, user_id, None)
}

/// [`verify_account`] with an optional SOCKS5 `proxy` (e.g.
/// `socks5h://127.0.0.1:9050`) for every fetch. When the closed overlay is on,
/// pollis-core passes the loopback shim here so the blocking `ureq` verify path
/// routes through the relay and does not leak the client's IP to the first-party
/// transparency host (design §14.4). `None` is exactly [`verify_account`] — a
/// direct fetch, byte-for-byte the pre-overlay behaviour. A malformed proxy URL
/// is returned as `Err`, never silently downgraded to a direct fetch.
pub fn verify_account_via(
    base_url: &str,
    user_id: &str,
    proxy: Option<&str>,
) -> Result<AccountReport> {
    let base = base_url.trim_end_matches('/');
    let agent = build_agent(proxy)?;

    // Prerequisites, all under the account-keys subtree.
    let pk_doc: PublicKeyDoc =
        fetch_json(&agent, &format!("{base}/v1/account-keys/public_key.json"))?;
    let sth: Sth = fetch_json(&agent, &format!("{base}/v1/account-keys/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(&agent, &format!("{base}/v1/account-keys/entries.json"))?;

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
        if let Ok(proof) = fetch_json::<InclusionProof>(&agent, &url) {
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
        public_key: pk_doc.public_key,
        sths: vec![sth],
        entries,
        enforce_unique: vec![ACCOUNT_TENANT.to_string()],
        inclusion,
        consistency: Vec::new(),
    };

    Ok(verify_account_in_bundle(&bundle, user_id))
}

/// Verify a single user's key-history chain against an **already-loaded**
/// account-key [`Bundle`] — no IO. This is the shared verdict core: both the
/// URL-based [`verify_account`] and the static report generator call it, so a
/// user's verdict is identical no matter how the bundle was obtained. A future
/// live `/verify/account/<id>` endpoint reuses it the same way the live group
/// endpoint reuses [`crate::group::verify_group_in_bundle`].
///
/// Never panics; a tampered/duplicated/regressed chain yields
/// `chain_valid == false` with populated `violations` rather than an error.
pub fn verify_account_in_bundle(bundle: &Bundle, user_id: &str) -> AccountReport {
    let mut violations: Vec<String> = Vec::new();

    // 1. Trust anchor: the newest account head, verified under the log key AND
    //    the account-keys domain context. An STH minted for the commit-log tree
    //    fails here even though the same key signed it.
    let verifying_key = verifying_key_from_hex(&bundle.public_key).ok();
    let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
    let (sth_tree_size, root_hex, sth_sig_ok) = match (latest, &verifying_key) {
        (Some(sth), Some(vk)) => (
            sth.tree_size,
            sth.root_hash.clone(),
            sth.verify_with_context(vk, account_key::STH_CONTEXT),
        ),
        (Some(sth), None) => (sth.tree_size, sth.root_hash.clone(), false),
        (None, _) => (0, String::new(), false),
    };
    if !sth_sig_ok {
        violations
            .push("account STH signature is invalid — published head is not trustworthy".to_string());
    }

    // 2. Membership: every entry that decodes as an account-key leaf for this
    //    user, paired with its global leaf index (used to locate its proof).
    let mut selected: Vec<(usize, AccountKeyLeaf)> = bundle
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == ACCOUNT_TENANT)
        .filter_map(|(i, e)| AccountKeyLeaf::decode(&e.data).ok().map(|leaf| (i, leaf)))
        .filter(|(_, leaf)| leaf.user_id == user_id)
        .collect();
    // Already in seq order, but sort defensively so the chain we report and
    // replay is unambiguous regardless of source ordering.
    selected.sort_by_key(|(_, leaf)| leaf.seq);

    let found = !selected.is_empty();

    // Index the bundle's inclusion proofs by leaf index, keeping only those
    // checked against the newest head.
    let inclusion_by_index: BTreeMap<usize, &InclusionProof> = bundle
        .inclusion
        .iter()
        .filter(|c| latest.is_some_and(|s| c.proof.tree_size == s.tree_size))
        .map(|c| (c.proof.leaf_index as usize, &c.proof))
        .collect();

    // 3. Inclusion: each selected entry must be committed by the latest STH.
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

    // 4. Invariant: replay this user's versions through the account-key rules
    //    (strictly increasing identity_version, no duplicate). Reuse slice 2's
    //    invariant verbatim — a rejected append is exactly a detected violation.
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

/// Abbreviate a long opaque id/hash for human-readable output.
fn short(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}\u{2026}{}", &s[..6], &s[s.len() - 4..])
    }
}
