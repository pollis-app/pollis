//! Per-group verification — the one shared function the CLI and the backend
//! HTTP endpoint both call, so they can never diverge.
//!
//! Given a published static read API (slice 3) and a conversation id, this
//! fetches the log over HTTP, isolates that conversation's commits, and decides
//! whether the group's commit chain is trustworthy:
//!
//! 1. **Trust anchor.** Fetch `public_key.json` + `sth/latest.json` and verify
//!    the STH signature *first*. Everything downstream is checked against that
//!    signed root — an unsigned/forged head is worth nothing.
//! 2. **Membership.** Select the entries whose [`CommitLeaf`] decodes and whose
//!    `conversation_id` matches, in `seq` order.
//! 3. **Inclusion.** For each selected entry, fetch its inclusion proof and
//!    verify it against the latest STH (reusing slice 1's
//!    [`verifiable_log::proof::verify_inclusion_proof`]).
//! 4. **Invariant.** Replay the group's commits through slice 2's
//!    [`CommitLogInvariant`] (epoch strictly increasing, no fork).
//!
//! Every cryptographic check is reused from slices 1–2; nothing here
//! reimplements Merkle, proof, signature, or invariant logic. Transport/parse
//! failures for the prerequisites return `Err`; a tampered/forked/regressed
//! group is **not** an error — it yields a [`GroupReport`] with
//! `chain_valid == false` and populated `violations`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use verifiable_log::{
    proof, verifying_key_from_hex, Entry, InclusionProof, Sth, VerifiableLog,
};
use verifiable_log_builder::{CommitLeaf, CommitLogInvariant, TENANT};

use crate::bundle::{Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::remote::{build_agent, fetch_json};

/// One commit in a group's chain, as reported to a caller. Mirrors the
/// structural fields of a [`CommitLeaf`] plus whether its inclusion proof
/// checked out against the signed head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCommit {
    /// MLS epoch after this commit.
    pub epoch: u64,
    /// Global insertion order (`mls_commit_log.seq`).
    pub seq: i64,
    /// Committer's user id (recorded, not authorized — see [`CommitLeaf`]).
    pub sender_id: String,
    /// `sha256(commit_data)`, lowercase hex.
    pub commit_sha256: String,
    /// Did this entry's inclusion proof verify against the latest STH?
    pub included: bool,
}

/// The structured result of verifying a single group. This is the exact shape
/// the CLI prints and the HTTP endpoint returns as JSON — same function, same
/// output, so the two can never report different things for the same input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupReport {
    /// The conversation id that was verified (echoed back).
    pub group_id: String,
    /// Were any commits found for this conversation?
    pub found: bool,
    /// Tree size of the STH everything was checked against.
    pub sth_tree_size: u64,
    /// Root hash of that STH, lowercase hex.
    pub root_hex: String,
    /// The group's commits, in `seq` order.
    pub commits: Vec<GroupCommit>,
    /// Overall verdict: STH signature valid AND every selected entry included
    /// AND the commit-log invariant holds.
    pub chain_valid: bool,
    /// Human-readable reasons `chain_valid` is false (empty when it is true).
    pub violations: Vec<String>,
}

/// Verify a single conversation's commit chain against the static log served at
/// `base_url` (e.g. `http://127.0.0.1:8787`), trusting only the published key.
///
/// This is a thin transport wrapper: it fetches the prerequisites
/// (`public_key.json`, `sth/latest.json`, `entries.json`) and the inclusion
/// proofs for this group's entries into an in-memory [`Bundle`], then hands them
/// to [`verify_group_in_bundle`] — the one place the actual verdict is computed.
/// The live server ([`crate::live`]) calls that same core against its in-memory
/// bundle directly, so the URL-fetched and in-memory paths **cannot diverge**.
///
/// Returns `Err` only for transport/parse failures of the prerequisites —
/// without them there is nothing to verify. Any *verification* failure (bad
/// signature, a missing or forged inclusion proof, a fork, an epoch regression)
/// is folded into the returned report as `chain_valid = false` with a
/// `violations` entry; it never panics and never returns `Err` for those.
pub fn verify_group(base_url: &str, conversation_id: &str) -> Result<GroupReport> {
    verify_group_via(base_url, conversation_id, None)
}

/// [`verify_group`] with an optional SOCKS5 `proxy` (e.g.
/// `socks5h://127.0.0.1:9050`) for every fetch. When the closed overlay is on,
/// pollis-core passes the loopback shim here so the blocking `ureq` verify path
/// routes through the relay and does not leak the client's IP to the first-party
/// transparency host (design §14.4). `None` is exactly [`verify_group`] — a
/// direct fetch, byte-for-byte the pre-overlay behaviour. A malformed proxy URL
/// is returned as `Err`, never silently downgraded to a direct fetch.
pub fn verify_group_via(
    base_url: &str,
    conversation_id: &str,
    proxy: Option<&str>,
) -> Result<GroupReport> {
    let base = base_url.trim_end_matches('/');
    let agent = build_agent(proxy)?;

    // Prerequisites: the published key, the latest signed head, and the full
    // ordered entry list. Without these there is nothing to verify.
    let pk_doc: PublicKeyDoc = fetch_json(&agent, &format!("{base}/v1/public_key.json"))?;
    let sth: Sth = fetch_json(&agent, &format!("{base}/v1/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(&agent, &format!("{base}/v1/entries.json"))?;

    // Plan which proofs to fetch: only this group's entries (decode + match).
    // This selection is a fetch optimisation; the verdict-bearing selection is
    // re-derived inside the shared core, so the two can't disagree.
    let group_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == TENANT)
        .filter_map(|(i, e)| CommitLeaf::decode(&e.data).ok().map(|leaf| (i, leaf)))
        .filter(|(_, leaf)| leaf.conversation_id == conversation_id)
        .map(|(i, _)| i)
        .collect();

    // Fetch each selected entry's inclusion proof against the latest STH. A
    // failed fetch simply omits the proof — the core then marks that entry
    // not-included, exactly as the old fetch-per-entry path did.
    let mut inclusion: Vec<InclusionCheck> = Vec::with_capacity(group_indices.len());
    for i in &group_indices {
        let url = format!("{base}/v1/proof/inclusion/{}/{}.json", sth.tree_size, i);
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
        enforce_unique: vec![TENANT.to_string()],
        inclusion,
        consistency: Vec::new(),
    };

    Ok(verify_group_in_bundle(&bundle, conversation_id))
}

/// Verify a single conversation's commit chain against an **already-loaded**
/// [`Bundle`] — no IO. This is the shared verdict core: both the URL-based
/// [`verify_group`] and the live server's `/verify/group/<id>` endpoint call it,
/// so a group's verdict is identical no matter how the bundle was obtained.
///
/// The checks mirror the static read API exactly:
///
/// 1. **Trust anchor.** The newest STH's signature must verify under the
///    bundle's published key. If not, the report still lists the group's commits
///    but the verdict is doomed.
/// 2. **Membership.** Select entries whose [`CommitLeaf`] decodes and whose
///    `conversation_id` matches, in `seq` order.
/// 3. **Inclusion.** Each selected entry must have an inclusion proof (against
///    the newest STH) in the bundle that verifies.
/// 4. **Invariant.** Replay the group's commits through slice 2's
///    [`CommitLogInvariant`] (epoch strictly increasing, no fork).
///
/// Never panics; a tampered/forked/regressed group yields `chain_valid == false`
/// with populated `violations` rather than an error.
pub fn verify_group_in_bundle(bundle: &Bundle, conversation_id: &str) -> GroupReport {
    let mut violations: Vec<String> = Vec::new();

    // 1. Trust anchor: the newest published head, verified under the log key.
    let verifying_key = verifying_key_from_hex(&bundle.public_key).ok();
    let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
    let (sth_tree_size, root_hex, sth_sig_ok) = match (latest, &verifying_key) {
        (Some(sth), Some(vk)) => (sth.tree_size, sth.root_hash.clone(), sth.verify(vk)),
        (Some(sth), None) => (sth.tree_size, sth.root_hash.clone(), false),
        (None, _) => (0, String::new(), false),
    };
    if !sth_sig_ok {
        violations.push("STH signature is invalid — published head is not trustworthy".to_string());
    }

    // 2. Membership: every entry that decodes as a commit leaf for this group,
    //    paired with its global leaf index (used to locate its proof).
    let mut selected: Vec<(usize, CommitLeaf)> = bundle
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == TENANT)
        .filter_map(|(i, e)| CommitLeaf::decode(&e.data).ok().map(|leaf| (i, leaf)))
        .filter(|(_, leaf)| leaf.conversation_id == conversation_id)
        .collect();
    // The log is already in seq order, but sort defensively so the chain we
    // report and replay is unambiguous regardless of source ordering.
    selected.sort_by_key(|(_, leaf)| leaf.seq);

    let found = !selected.is_empty();

    // Index the bundle's inclusion proofs by leaf index, keeping only those
    // checked against the newest head — the one this report is anchored to.
    let inclusion_by_index: BTreeMap<usize, &InclusionProof> = bundle
        .inclusion
        .iter()
        .filter(|c| latest.is_some_and(|s| c.proof.tree_size == s.tree_size))
        .map(|c| (c.proof.leaf_index as usize, &c.proof))
        .collect();

    // 3. Inclusion: each selected entry must be committed by the latest STH.
    let mut commits: Vec<GroupCommit> = Vec::with_capacity(selected.len());
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
                "commit seq {} (epoch {}) is not provably included in the signed log",
                leaf.seq, leaf.epoch
            ));
        }
        commits.push(GroupCommit {
            epoch: leaf.epoch,
            seq: leaf.seq,
            sender_id: leaf.sender_id.clone(),
            commit_sha256: leaf.commit_sha256.clone(),
            included,
        });
    }

    // 4. Invariant: replay this group's commits through the commit-log rules
    //    (no fork, no epoch regression). Reuse slice 2's invariant verbatim — a
    //    rejected append is exactly a detected violation.
    let mut log = VerifiableLog::new();
    log.register_invariant(TENANT, Box::new(CommitLogInvariant));
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
                violations.push(format!("commit seq {} failed to re-encode: {e}", leaf.seq));
            }
        }
    }

    let chain_valid = sth_sig_ok && all_included && invariant_ok;

    GroupReport {
        group_id: conversation_id.to_string(),
        found,
        sth_tree_size,
        root_hex,
        commits,
        chain_valid,
        violations,
    }
}

impl GroupReport {
    /// Print a human-readable report to stdout (used by the CLI's text mode).
    pub fn print(&self) {
        println!("Group:   {}", self.group_id);
        println!("Found:   {}", if self.found { "yes" } else { "no" });
        println!("STH:     tree_size {}  root {}", self.sth_tree_size, self.root_hex);
        if self.commits.is_empty() {
            println!("Commits: (none)");
        } else {
            println!("Commits (seq order):");
            for c in &self.commits {
                println!(
                    "  epoch {:<4} seq {:<6} sender {:<12} commit {}  {}",
                    c.epoch,
                    c.seq,
                    short(&c.sender_id),
                    short(&c.commit_sha256),
                    if c.included { "[included \u{2713}]" } else { "[MISSING \u{2717}]" },
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
                "PASS: group chain is valid"
            } else {
                "FAIL: group chain is NOT valid"
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
