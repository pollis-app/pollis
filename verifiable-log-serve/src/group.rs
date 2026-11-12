//! Per-group verification — the one shared function the CLI and the backend
//! HTTP endpoint both call, so they can never diverge.
//!
//! Given a published read API and a conversation id, decide whether its commit chain is
//! trustworthy:
//!
//! 1. **Trust anchor.** Fetch `public_key.json` + `sth/latest.json` and verify
//!    the STH signature first, against every currently acceptable published key.
//! 2. **Membership.** Select entries whose pseudonym matches the one re-derived for the real
//!    `conversation_id` in each leaf window (#701).
//! 3. **Inclusion.** For each selected entry, fetch its inclusion proof and
//!    verify it against the latest STH.
//! 4. **Invariant.** Replay the group's commits through slice 2's
//!    [`CommitLogInvariant`] (epoch strictly increasing, no fork).
//!
//! Prerequisite failures return `Err`; a tampered group yields a [`GroupReport`] with
//! `chain_valid == false`.

use serde::{Deserialize, Serialize};

use verifiable_log::VerifiableLog;
use verifiable_log_builder::{
    derive_conversation_pseudonym, window_for_seq, CommitLeaf, CommitLogInvariant, TENANT,
};

use crate::bundle::{now_ms, short, Bundle};
use crate::error::Result;
use crate::remote::{build_agent, fetch_text, fetch_text_opt, gate_leaf_format_version};
use crate::verdict::{fetch_subject_bundle, print_verdict, raise_key_set_alarm, Anchor};

/// One commit in a group's chain, as reported to a caller. Mirrors the
/// structural fields of a [`CommitLeaf`] plus whether its inclusion proof verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCommit {
    /// Suite generation of the lineage (#454 P4); 0 if never migrated.
    pub generation: u64,
    /// MLS epoch after this commit, within `generation`'s lineage.
    pub epoch: u64,
    /// Global insertion order (`mls_commit_log.seq`).
    pub seq: i64,
    /// Windowed, conversation-bound pseudonym for the committer (#701) — the
    /// opaque published value, not the real user id. Not authorized.
    pub sender_pseudonym: String,
    /// `sha256(commit_data)`, lowercase hex.
    pub commit_sha256: String,
    /// Did this entry's inclusion proof verify against the latest STH?
    pub included: bool,
}

/// The structured result of verifying a single group. This is the exact shape
/// the CLI prints and the HTTP endpoint returns, from one function.
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
/// Fetches the prerequisites into an in-memory [`Bundle`] for [`verify_group_in_bundle`], the
/// one verdict core.
///
/// `Err` only for prerequisite failures; verification failures set `chain_valid = false`.
pub fn verify_group(base_url: &str, conversation_id: &str) -> Result<GroupReport> {
    verify_group_via(base_url, conversation_id, None)
}

/// [`verify_group`] with an optional SOCKS5 `proxy` (e.g.
/// `socks5h://127.0.0.1:9050`) so the overlay hides the client IP; `None` is a direct fetch.
pub fn verify_group_via(
    base_url: &str,
    conversation_id: &str,
    proxy: Option<&str>,
) -> Result<GroupReport> {
    let agent = build_agent(proxy)?;
    // Root-then-signers (#754, #1454): root-authorised signers are trusted alongside the pin.
    let trust = crate::pinned::key_set_trust(&agent, base_url, verifiable_log::STH_CONTEXT_COMMIT_LOG);
    verify_group_with(&agent, base_url, conversation_id, &trust)
}

/// [`verify_group_via`] with the key-set trust decision supplied by the caller, so a
/// caller that also pin-checks the served key uses the same decision. `trust.alarm` fails the
/// report.
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

    // Version gate first: a verifier older than the leaf format would report "Found: no". With
    // `commit-log-2` served (#1579), verify against the successor.
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

    // Prerequisites: the published key, the latest signed head, and the ordered entries.
    let bundle = fetch_subject_bundle(
        agent,
        &prefix,
        trust,
        TENANT,
        // Only this group's proofs, matched by re-deriving the pseudonym per leaf window; the
        // core re-selects.
        |e| {
            e.tenant == TENANT
                && CommitLeaf::decode(&e.data).is_ok_and(|leaf| {
                    leaf.conversation_pseudonym
                        == derive_conversation_pseudonym(conversation_id, window_for_seq(leaf.seq))
                })
        },
    )?;

    let mut report =
        verify_group_in_bundle_with_context(&bundle, conversation_id, now_ms(), context);
    raise_key_set_alarm(trust, &mut report.chain_valid, &mut report.violations);
    Ok(report)
}

/// Verify a single conversation's commit chain against an **already-loaded**
/// [`Bundle`], no IO: the shared verdict core for every path.
///
/// **The member/auditor path (#701).** `conversation_id` is the real id, so the full-strength
/// invariant spans every window.
///
/// Same checks; the invariant spans windows.
///
/// Never panics; failures yield `chain_valid == false` with `violations`.
pub fn verify_group_in_bundle(bundle: &Bundle, conversation_id: &str) -> GroupReport {
    verify_group_in_bundle_at(bundle, conversation_id, now_ms())
}

/// [`verify_group_in_bundle`] against an explicit `now_ms`, which decides which
/// rotation-overlap keys have expired.
///
/// The clock is a parameter so the overlap window is testable.
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

/// [`verify_group_in_bundle_at`] for a commit-log tree whose heads are signed
/// under `context`: [`verifiable_log::STH_CONTEXT_COMMIT_LOG`] for the first
/// tree, [`verifiable_log::STH_CONTEXT_COMMIT_LOG_2`] for its successor (#1579).
pub fn verify_group_in_bundle_with_context(
    bundle: &Bundle,
    conversation_id: &str,
    now_ms: u64,
    context: &[u8],
) -> GroupReport {
    let mut violations: Vec<String> = Vec::new();

    // 1. Trust anchor: the newest head, verified under any currently acceptable key.
    let anchor = Anchor::new(bundle, now_ms, context);
    let sth_sig_ok = anchor.sth_sig_ok;
    if !sth_sig_ok {
        violations.push("STH signature is invalid — published head is not trustworthy".to_string());
    }

    // 2. Membership: every entry whose pseudonym matches this conversation in its window.
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
    // Sorted defensively so the reported and replayed chain is unambiguous.
    selected.sort_by_key(|(_, leaf)| leaf.seq);

    let found = !selected.is_empty();

    let mut commits: Vec<GroupCommit> = Vec::with_capacity(selected.len());
    let mut all_included = true;
    for (leaf_index, leaf) in &selected {
        let included = anchor.included(bundle, *leaf_index);
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

    // 4. Invariant: replay under one canonical key (the real id) so the check spans all windows.
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
        sth_tree_size: anchor.sth_tree_size,
        root_hex: anchor.root_hex,
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
                    short(&c.sender_pseudonym),
                    short(&c.commit_sha256),
                    if c.included { "[included \u{2713}]" } else { "[MISSING \u{2717}]" },
                );
            }
        }
        print_verdict(
            &self.violations,
            self.chain_valid,
            "PASS: group chain is valid",
            "FAIL: group chain is NOT valid",
        );
    }
}
