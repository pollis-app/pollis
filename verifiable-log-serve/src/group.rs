//! Per-group verification — the one shared function the CLI and the backend
//! HTTP endpoint both call, so they can never diverge.
//!
//! Given a published static read API (slice 3) and a conversation id, this
//! fetches the log over HTTP, isolates that conversation's commits, and decides
//! whether the group's commit chain is trustworthy:
//!
//! 1. **Trust anchor.** Fetch `public_key.json` + `sth/latest.json` and verify
//!    the STH signature *first*, against the *set* of keys that document
//!    publishes as currently acceptable (active signer + any key still inside its
//!    rotation-overlap window). Everything downstream is checked against that
//!    signed root — an unsigned/forged head is worth nothing.
//! 2. **Membership.** Select the entries whose [`CommitLeaf`] decodes and whose
//!    windowed pseudonym matches the one re-derived for the real
//!    `conversation_id` in that leaf's window (#701), in `seq` order — every
//!    window of the conversation.
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

use verifiable_log::{proof, Entry, InclusionProof, Sth, VerifiableLog};
use verifiable_log_builder::{
    derive_conversation_pseudonym, window_for_seq, CommitLeaf, CommitLogInvariant, TENANT,
};

use crate::bundle::{now_ms, short, Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::remote::{build_agent, fetch_json, fetch_text, gate_leaf_format_version};

/// One commit in a group's chain, as reported to a caller. Mirrors the
/// structural fields of a [`CommitLeaf`] plus whether its inclusion proof
/// checked out against the signed head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCommit {
    /// Suite generation of the lineage this commit belongs to (#454 P4). 0 for
    /// a conversation that has never migrated to the post-quantum hybrid suite.
    pub generation: u64,
    /// MLS epoch after this commit, within `generation`'s lineage.
    pub epoch: u64,
    /// Global insertion order (`mls_commit_log.seq`).
    pub seq: i64,
    /// Windowed, conversation-bound pseudonym for the committer (#701) — the
    /// opaque published value, not the real user id. Recorded, not authorized
    /// (see [`CommitLeaf`]).
    pub sender_pseudonym: String,
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

    // Version gate first: `group` decodes commit leaves, whose encoding changed at
    // the #701 republish, so a verifier older than the log would silently select
    // nothing and report "Found: no" for a conversation that is really there. Read
    // the manifest's `format_version` up front and refuse a log newer than we
    // understand with VersionSkew ("upgrade your verifier") rather than a
    // misleading empty result.
    let index_body = fetch_text(&agent, &format!("{base}/v1/index.json"))?;
    gate_leaf_format_version(&index_body)?;

    // Prerequisites: the published key, the latest signed head, and the full
    // ordered entry list. Without these there is nothing to verify.
    let pk_doc: PublicKeyDoc = fetch_json(&agent, &format!("{base}/v1/public_key.json"))?;
    let sth: Sth = fetch_json(&agent, &format!("{base}/v1/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(&agent, &format!("{base}/v1/entries.json"))?;

    // Plan which proofs to fetch: only this group's entries. Since #701 the leaf
    // carries a windowed pseudonym, not the raw id, so we match by re-deriving the
    // pseudonym for the conversation *in each leaf's own window* (window = seq /
    // WINDOW_SIZE) — the same thing a member does to recover their history across
    // windows. This selection is a fetch optimisation; the verdict-bearing
    // selection is re-derived inside the shared core, so the two can't disagree.
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
        // Verification-side reconstruction, never re-published. The served key
        // document's rotation-overlap set is carried through so the verdict core
        // accepts a head signed by a key that is retiring but still inside its
        // window — during a changeover `public_key.json` and `sth/latest.json` are
        // separate artifacts on separate cache policies and legitimately move at
        // different moments. Dropping the set here (#875) made an honest log
        // mid-rotation report `chain_valid = false`.
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

/// Verify a single conversation's commit chain against an **already-loaded**
/// [`Bundle`] — no IO. This is the shared verdict core: both the URL-based
/// [`verify_group`] and the live server's `/verify/group/<id>` endpoint call it,
/// so a group's verdict is identical no matter how the bundle was obtained.
///
/// **This is the member/auditor path (#701).** `conversation_id` is the *real*
/// id — something only a member (or an auditor a member has told) knows, since the
/// published log carries windowed pseudonyms, not raw ids. Knowing it lets this
/// function recover the conversation's *entire* history across every window and
/// check the **full-strength** invariant, closing the window-boundary residual an
/// outside-only replay cannot (see [`CommitLogInvariant`]).
///
/// The checks mirror the static read API exactly:
///
/// 1. **Trust anchor.** The newest STH's signature must verify under the
///    bundle's published key. If not, the report still lists the group's commits
///    but the verdict is doomed.
/// 2. **Membership.** Select entries whose [`CommitLeaf`] decodes and whose
///    `conversation_pseudonym` equals the pseudonym re-derived for
///    `conversation_id` in that leaf's own window (`seq / WINDOW_SIZE`), in `seq`
///    order — i.e. every window of this conversation.
/// 3. **Inclusion.** Each selected entry must have an inclusion proof (against
///    the newest STH) in the bundle that verifies.
/// 4. **Invariant.** Replay the group's commits through slice 2's
///    [`CommitLogInvariant`], **regrouped under a single key** so the check spans
///    windows (epoch strictly increasing, no fork — across the whole conversation,
///    not just within a window).
///
/// Never panics; a tampered/forked/regressed group yields `chain_valid == false`
/// with populated `violations` rather than an error.
pub fn verify_group_in_bundle(bundle: &Bundle, conversation_id: &str) -> GroupReport {
    verify_group_in_bundle_at(bundle, conversation_id, now_ms())
}

/// [`verify_group_in_bundle`] against an explicit `now_ms`, which decides which
/// rotation-overlap keys have expired.
///
/// The clock is a parameter so the overlap window is testable without waiting for
/// one, and so a caller that needs a deterministic verdict can supply its own
/// instant. [`verify_group_in_bundle`] is exactly this against the wall clock.
pub fn verify_group_in_bundle_at(
    bundle: &Bundle,
    conversation_id: &str,
    now_ms: u64,
) -> GroupReport {
    let mut violations: Vec<String> = Vec::new();

    // 1. Trust anchor: the newest published head, verified under ANY key the log
    //    publishes as acceptable right now — the active signer plus any key still
    //    inside its rotation-overlap window. A head is honest if it verifies under
    //    one of them; only verifying under none is evidence of a problem. Expiry is
    //    applied here (not trusted from the server), so a retired key stops being
    //    accepted on schedule.
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

    // 2. Membership: every entry whose published pseudonym matches the one this
    //    conversation would carry in that leaf's own window — i.e. all of the
    //    conversation's commits, across every window — paired with its global leaf
    //    index (used to locate its proof).
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

    // 4. Invariant: replay this group's commits through the commit-log rules
    //    (no fork, no epoch regression). Reuse slice 2's invariant verbatim — a
    //    rejected append is exactly a detected violation.
    //
    //    Since #701 the selected leaves carry a *different* pseudonym per window,
    //    so replaying them as-is would let [`CommitLogInvariant`] group only
    //    within a window and miss a boundary-straddling fork/regression. Because
    //    we know the real `conversation_id`, we regroup every selected leaf under
    //    one canonical key (the real id) so the invariant spans all windows — the
    //    full-strength check a member is entitled to, and the whole reason this
    //    path takes the real id rather than a pseudonym.
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
