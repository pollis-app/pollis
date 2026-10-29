//! Per-group verification: the one shared function the CLI and the HTTP
//! endpoints call, so their verdicts cannot diverge. All crypto is reused from
//! `verifiable_log`; a tampered group yields `chain_valid == false`, not `Err`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use verifiable_log::{proof, Entry, InclusionProof, Sth, VerifiableLog};
use verifiable_log_builder::{
    derive_conversation_pseudonym, window_for_seq, CommitLeaf, CommitLogInvariant, TENANT,
};

use crate::bundle::{now_ms, short, Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::remote::{build_agent, fetch_json, fetch_text, gate_leaf_format_version};

/// One commit in a group's chain: [`CommitLeaf`]'s fields plus whether its
/// inclusion proof verified against the signed head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCommit {
    /// Suite generation of the commit's lineage (#454); 0 = never migrated to PQ.
    pub generation: u64,
    /// MLS epoch after this commit, within `generation`'s lineage.
    pub epoch: u64,
    /// Global insertion order (`mls_commit_log.seq`).
    pub seq: i64,
    /// Windowed committer pseudonym (#701); recorded, not authorized.
    pub sender_pseudonym: String,
    /// `sha256(commit_data)`, lowercase hex.
    pub commit_sha256: String,
    pub included: bool,
}

/// The result of verifying a single group, as the CLI prints it and the HTTP
/// endpoint returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupReport {
    pub group_id: String,
    pub found: bool,
    /// Tree size of the STH everything was checked against.
    pub sth_tree_size: u64,
    pub root_hex: String,
    /// The group's commits, in `seq` order.
    pub commits: Vec<GroupCommit>,
    /// STH signature valid AND every selected entry included AND the commit-log
    /// invariant holds.
    pub chain_valid: bool,
    /// Why `chain_valid` is false (empty when true).
    pub violations: Vec<String>,
}

/// Verify a conversation's commit chain against the static log at `base_url`,
/// trusting only the pinned key. Fetches into a [`Bundle`] and defers to
/// [`verify_group_in_bundle`].
///
/// `Err` only for transport/parse failures of the prerequisites; verification
/// failures land in the report as `chain_valid = false`.
pub fn verify_group(base_url: &str, conversation_id: &str) -> Result<GroupReport> {
    verify_group_via(base_url, conversation_id, None)
}

/// [`verify_group`] with an optional SOCKS5 `proxy`, so the overlay can hide the
/// client's IP from the transparency host (design §14.4). A malformed proxy URL
/// is `Err`, never silently downgraded to a direct fetch.
pub fn verify_group_via(
    base_url: &str,
    conversation_id: &str,
    proxy: Option<&str>,
) -> Result<GroupReport> {
    let base = base_url.trim_end_matches('/');
    let agent = build_agent(proxy)?;

    // Version gate first: this path decodes leaves, so skew would otherwise read
    // as a misleading "Found: no".
    let index_body = fetch_text(&agent, &format!("{base}/v1/index.json"))?;
    gate_leaf_format_version(&index_body)?;

    let pk_doc: PublicKeyDoc = fetch_json(&agent, &format!("{base}/v1/public_key.json"))?;
    // H-1: pin at the fetch boundary (see `require_pinned`).
    let pk_doc = crate::pinned::require_pinned(&pk_doc, now_ms()).map_err(crate::error::ServeError::BadBundle)?;
    let sth: Sth = fetch_json(&agent, &format!("{base}/v1/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(&agent, &format!("{base}/v1/entries.json"))?;

    // Fetch optimisation only: the verdict-bearing selection is re-derived in the
    // shared core.
    let group_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == TENANT)
        .filter_map(|(i, e)| CommitLeaf::decode(&e.data).ok().map(|leaf| (i, leaf)))
        .filter(|(_, leaf)| {
            leaf.conversation_pseudonym
                == derive_conversation_pseudonym(conversation_id, window_for_seq(leaf.seq))
        })
        .map(|(i, _)| i)
        .collect();

    // A failed fetch omits the proof; the core then marks the entry not-included.
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
        // Carry the overlap set through, or an honest log mid-rotation fails
        // (#875).
        retired_keys: pk_doc.overlap_keys(),
        public_key: pk_doc.public_key,
        sths: vec![sth],
        entries,
        enforce_unique: vec![TENANT.to_string()],
        inclusion,
        consistency: Vec::new(),
    };

    Ok(verify_group_in_bundle(&bundle, conversation_id))
}

/// The shared verdict core over an already-loaded [`Bundle`] (no IO).
///
/// The member/auditor path (#701): `conversation_id` is the real id, which lets
/// it recover every window's pseudonyms and check the invariant across window
/// boundaries, which an outside-only replay cannot (see [`CommitLogInvariant`]).
/// Checks: STH signature, membership, inclusion against the newest STH, and the
/// commit-log invariant.
pub fn verify_group_in_bundle(bundle: &Bundle, conversation_id: &str) -> GroupReport {
    verify_group_in_bundle_at(bundle, conversation_id, now_ms())
}

/// [`verify_group_in_bundle`] against an explicit `now_ms`, which decides which
/// rotation-overlap keys have expired.
pub fn verify_group_in_bundle_at(
    bundle: &Bundle,
    conversation_id: &str,
    now_ms: u64,
) -> GroupReport {
    let mut violations: Vec<String> = Vec::new();

    // 1. Trust anchor: the newest head must verify under any currently
    //    acceptable key; expiry is applied here, not trusted from the server.
    let candidates = bundle.key_candidates(now_ms);
    let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
    let (sth_tree_size, root_hex, sth_sig_ok) = match latest {
        Some(sth) => (
            sth.tree_size,
            sth.root_hash.clone(),
            sth.verify_any(&candidates).is_some(),
        ),
        None => (0, String::new(), false),
    };
    if !sth_sig_ok {
        violations.push("STH signature is invalid — published head is not trustworthy".to_string());
    }

    // 2. Membership: entries whose pseudonym matches this conversation's in the
    //    leaf's own window, with their global leaf index.
    let mut selected: Vec<(usize, CommitLeaf)> = bundle
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == TENANT)
        .filter_map(|(i, e)| CommitLeaf::decode(&e.data).ok().map(|leaf| (i, leaf)))
        .filter(|(_, leaf)| {
            leaf.conversation_pseudonym
                == derive_conversation_pseudonym(conversation_id, window_for_seq(leaf.seq))
        })
        .collect();
    // Defensive: the log is already in seq order.
    selected.sort_by_key(|(_, leaf)| leaf.seq);

    let found = !selected.is_empty();

    // Only proofs against the newest head, which this report is anchored to.
    let inclusion_by_index: BTreeMap<usize, &InclusionProof> = bundle
        .inclusion
        .iter()
        .filter(|c| latest.is_some_and(|s| c.proof.tree_size == s.tree_size))
        .map(|c| (c.proof.leaf_index as usize, &c.proof))
        .collect();

    // 3. Inclusion against the latest STH.
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
                "commit seq {} (generation {}, epoch {}) is not provably included in the signed log",
                leaf.seq, leaf.generation, leaf.epoch
            ));
        }
        commits.push(GroupCommit {
            generation: leaf.generation,
            epoch: leaf.epoch,
            seq: leaf.seq,
            sender_pseudonym: leaf.sender_pseudonym.clone(),
            commit_sha256: leaf.commit_sha256.clone(),
            included,
        });
    }

    // 4. Invariant: regroup every leaf under the real id, so a fork or
    //    regression straddling a pseudonym window boundary is still caught.
    let mut log = VerifiableLog::new();
    log.register_invariant(TENANT, Box::new(CommitLogInvariant));
    let mut invariant_ok = true;
    for (_, leaf) in &selected {
        let canonical = CommitLeaf {
            conversation_pseudonym: conversation_id.to_string(),
            generation: leaf.generation,
            epoch: leaf.epoch,
            sender_pseudonym: leaf.sender_pseudonym.clone(),
            seq: leaf.seq,
            commit_sha256: leaf.commit_sha256.clone(),
        };
        match canonical.to_entry() {
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
    /// Print a human-readable report to stdout.
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
                    short(&c.sender_pseudonym),
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
