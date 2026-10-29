//! Per-group verification: the one function the CLI and the HTTP endpoint
//! share, so they can never diverge. Transport/parse failures are `Err`; a
//! tampered, forked or regressed group is a [`GroupReport`] with
//! `chain_valid == false`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use verifiable_log::{proof, Entry, InclusionProof, Sth, VerifiableLog};
use verifiable_log_builder::{
    derive_conversation_pseudonym, window_for_seq, CommitLeaf, CommitLogInvariant, TENANT,
};

use crate::bundle::{now_ms, short, Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::remote::{build_agent, fetch_json, fetch_text, fetch_text_opt, gate_leaf_format_version};

/// One commit in a group's chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCommit {
    /// Suite generation of this commit's lineage (#454); 0 if never migrated.
    pub generation: u64,
    /// MLS epoch after this commit, within `generation`'s lineage.
    pub epoch: u64,
    pub seq: i64,
    /// Windowed, conversation-bound committer pseudonym (#701). Recorded, not
    /// authorized (see [`CommitLeaf`]).
    pub sender_pseudonym: String,
    pub commit_sha256: String,
    /// Inclusion proof verified against the latest STH.
    pub included: bool,
}

/// Result of verifying one group; the shape the CLI prints and the HTTP
/// endpoint returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupReport {
    pub group_id: String,
    pub found: bool,
    /// Tree size of the STH everything was checked against.
    pub sth_tree_size: u64,
    pub root_hex: String,
    pub commits: Vec<GroupCommit>,
    /// STH signature valid AND every entry included AND the invariant holds.
    pub chain_valid: bool,
    /// Why `chain_valid` is false (empty when true).
    pub violations: Vec<String>,
}

/// Verify a conversation's commit chain against the static log at `base_url`,
/// trusting only the pinned key. Fetches into a [`Bundle`] and defers to
/// [`verify_group_in_bundle_with_context`]. `Err` only for transport/parse
/// failures; verification failures are `chain_valid = false`.
pub fn verify_group(base_url: &str, conversation_id: &str) -> Result<GroupReport> {
    verify_group_via(base_url, conversation_id, None)
}

/// [`verify_group`] with an optional SOCKS5 `proxy` for every fetch, so the
/// overlay can hide the client's IP from the transparency host. A malformed
/// proxy URL is `Err`, never a silent downgrade to a direct fetch.
pub fn verify_group_via(
    base_url: &str,
    conversation_id: &str,
    proxy: Option<&str>,
) -> Result<GroupReport> {
    let agent = build_agent(proxy)?;
    // A signer the offline root authorises for this tree is trusted alongside
    // the compiled pin, without a release (#754, #1454).
    let trust = crate::pinned::key_set_trust(&agent, base_url, verifiable_log::STH_CONTEXT_COMMIT_LOG);
    verify_group_with(&agent, base_url, conversation_id, &trust)
}

/// [`verify_group_via`] with a caller-supplied key-set decision, so a caller
/// that also pin-checks the served key (pollis-core) uses the same decision.
/// `trust.alarm` fails the report.
pub fn verify_group_via_trusting(
    base_url: &str,
    conversation_id: &str,
    proxy: Option<&str>,
    trust: &crate::pinned::KeySetTrust,
) -> Result<GroupReport> {
    let agent = build_agent(proxy)?;
    verify_group_with(&agent, base_url, conversation_id, trust)
}

fn verify_group_with(
    agent: &ureq::Agent,
    base_url: &str,
    conversation_id: &str,
    trust: &crate::pinned::KeySetTrust,
) -> Result<GroupReport> {
    let base = base_url.trim_end_matches('/');

    // Gate on `format_version` first: a verifier older than the leaf encoding
    // would select nothing and misreport "Found: no" (#701).
    //
    // A log serving `commit-log-2` has retired the first tree, so verify against
    // the successor under its own context (#1579). Hiding the successor gains
    // nothing: the first tree is frozen under the retired context, and `remote`
    // fails a log that names a successor it does not serve.
    let successor = format!("{base}/{}", crate::layout::COMMIT_LOG_2_API_PREFIX);
    let (prefix, context, index_body) =
        match fetch_text_opt(agent, &format!("{successor}/index.json"))? {
            Some(body) => (successor, verifiable_log::STH_CONTEXT_COMMIT_LOG_2, body),
            None => (
                format!("{base}/v1"),
                verifiable_log::STH_CONTEXT_COMMIT_LOG,
                fetch_text(agent, &format!("{base}/v1/index.json"))?,
            ),
        };
    gate_leaf_format_version(&index_body)?;

    let pk_doc: PublicKeyDoc = fetch_json(agent, &format!("{prefix}/public_key.json"))?;
    // H-1: pin at the fetch boundary, since this document came off an untrusted
    // server. The `*_in_bundle` core is deliberately ungated: a publisher's own
    // bundle is anchored by its own key. Rebinding keeps only pinned keys.
    let pk_doc = crate::pinned::require_pinned_with(&pk_doc, now_ms(), trust)
        .map_err(crate::error::ServeError::BadBundle)?;
    let sth: Sth = fetch_json(agent, &format!("{prefix}/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(agent, &format!("{prefix}/entries.json"))?;

    // Match by re-deriving the conversation's pseudonym in each leaf's own
    // window (#701). This is only a fetch optimisation; the core re-selects.
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

    // A failed proof fetch omits the proof; the core marks that entry not included.
    let mut inclusion: Vec<InclusionCheck> = Vec::with_capacity(group_indices.len());
    for i in &group_indices {
        let url = format!("{prefix}/proof/inclusion/{}/{}.json", sth.tree_size, i);
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
        // Carry the served rotation-overlap set: `public_key.json` and
        // `sth/latest.json` move at different moments, so an honest log
        // mid-rotation must not fail (#875).
        retired_keys: pk_doc.overlap_keys(),
        public_key: pk_doc.public_key,
        sths: vec![sth],
        entries,
        enforce_unique: vec![TENANT.to_string()],
        inclusion,
        consistency: Vec::new(),
    };

    let mut report = verify_group_in_bundle_with_context(&bundle, conversation_id, now_ms(), context);
    // A served key set the root doesn't vouch for is an alarm even when the tree
    // verifies under the compiled pin.
    if let Some(v) = trust.alarm_violation() {
        report.chain_valid = false;
        report.violations.insert(0, v);
    }
    Ok(report)
}

/// Verify a conversation's commit chain against an already-loaded [`Bundle`],
/// no IO: the shared verdict core behind [`verify_group`] and the live
/// `/verify/group/<id>` endpoint. Never panics.
///
/// This is the member/auditor path (#701): `conversation_id` is the real id, so
/// every window's pseudonym can be re-derived and the invariant checked across
/// window boundaries, which an outside-only replay cannot (see
/// [`CommitLogInvariant`]).
pub fn verify_group_in_bundle(bundle: &Bundle, conversation_id: &str) -> GroupReport {
    verify_group_in_bundle_at(bundle, conversation_id, now_ms())
}

/// [`verify_group_in_bundle`] against an explicit `now_ms`, which decides
/// which rotation-overlap keys have expired (testable, deterministic).
pub fn verify_group_in_bundle_at(
    bundle: &Bundle,
    conversation_id: &str,
    now_ms: u64,
) -> GroupReport {
    verify_group_in_bundle_with_context(
        bundle,
        conversation_id,
        now_ms,
        verifiable_log::STH_CONTEXT_COMMIT_LOG,
    )
}

/// [`verify_group_in_bundle_at`] for a tree whose heads are signed under
/// `context` (first tree or commit-log-2, #1579). A head of one tree never
/// anchors a report about the other.
pub fn verify_group_in_bundle_with_context(
    bundle: &Bundle,
    conversation_id: &str,
    now_ms: u64,
    context: &[u8],
) -> GroupReport {
    let mut violations: Vec<String> = Vec::new();

    // Accept any key the log publishes as currently acceptable; expiry is
    // applied here, not trusted from the server.
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
    if !sth_sig_ok {
        violations.push("STH signature is invalid — published head is not trustworthy".to_string());
    }

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
    // Sort defensively so the reported and replayed chain is source-order independent.
    selected.sort_by_key(|(_, leaf)| leaf.seq);

    let found = !selected.is_empty();

    let inclusion_by_index: BTreeMap<usize, &InclusionProof> = bundle
        .inclusion
        .iter()
        .filter(|c| latest.is_some_and(|s| c.proof.tree_size == s.tree_size))
        .map(|c| (c.proof.leaf_index as usize, &c.proof))
        .collect();

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

    // Leaves carry a different pseudonym per window, so regroup them under the
    // real id: otherwise [`CommitLogInvariant`] would group per window and miss
    // a fork or regression straddling a window boundary (#701).
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
