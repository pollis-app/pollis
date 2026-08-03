//! Gate for per-group verification (slice 4): the shared `verify_group`, the
//! `GET /verify/group/<id>` HTTP endpoint, and the proof that the two agree.
//!
//! Covers:
//! (a) a healthy group over HTTP returns `chain_valid = true` with the expected
//!     epochs/commits;
//! (b) a forked group and an epoch-regressed group return `chain_valid = false`
//!     with the matching violation, and a tampered STH signature also fails;
//! (c) the endpoint sends `Access-Control-Allow-Origin: *` (and answers an
//!     `OPTIONS` preflight);
//! (d) the CLI path (`verify_group` called directly) and the HTTP endpoint
//!     return the SAME `GroupReport` for the same input.
//!
//! The fixture tree is constructed directly from slice-1 primitives with
//! slice-2 `CommitLeaf` payloads. Crucially it does NOT register the
//! `CommitLogInvariant` at build time, so forks/regressions can be planted into
//! the published tree exactly as a buggy/malicious server would have — and it is
//! `verify_group` (read-time) that must catch them.

use std::path::Path;

use ml_dsa::Keypair;
use verifiable_log::SigningKey;
use verifiable_log::{Entry, Sth, VerifiableLog};
use verifiable_log_builder::{
    derive_conversation_pseudonym, derive_sender_pseudonym, window_for_seq, CommitLeaf,
    PSEUDONYM_WINDOW_SIZE,
};
use verifiable_log_serve::bundle::{Bundle, ConsistencyCheck, InclusionCheck};
use verifiable_log_serve::group::{verify_group, GroupReport};
use verifiable_log_serve::{layout, DevServer};

const TS: u64 = 1_700_000_000_000;

/// One pseudonym window, so tests can straddle a real boundary with small seqs.
const W: i64 = PSEUDONYM_WINDOW_SIZE as i64;

/// A **published** leaf as the builder emits it since #701: the identity fields
/// are the windowed pseudonyms derived from the real `conv` and the leaf's own
/// `seq`. The fixtures keep passing the real `conv`, exactly as a member does.
fn leaf(conv: &str, epoch: u64, seq: i64, commit: &str) -> CommitLeaf {
    leaf_at(conv, 0, epoch, seq, commit)
}

fn leaf_at(conv: &str, generation: u64, epoch: u64, seq: i64, commit: &str) -> CommitLeaf {
    let window = window_for_seq(seq);
    CommitLeaf {
        conversation_pseudonym: derive_conversation_pseudonym(conv, window),
        generation,
        epoch,
        sender_pseudonym: derive_sender_pseudonym(conv, &format!("u-{conv}"), window),
        seq,
        commit_sha256: hex::encode(blake_ish(commit)),
    }
}

/// A cheap, distinct 32-byte "hash" for fixture commits — only distinctness and
/// hex-encoding matter here (the leaf encoding, not this value, is what the
/// Merkle tree commits to).
fn blake_ish(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in s.bytes().enumerate() {
        out[i % 32] ^= b.wrapping_add(i as u8);
    }
    out
}

/// Build a static `/v1` tree under `root` from the given leaves, in the order
/// supplied. Returns the bundle that was generated.
fn build_tree(root: &Path, leaves: &[CommitLeaf]) -> Bundle {
    let signing_key = SigningKey::from_seed(&[7u8; 32].into());
    let mut log = VerifiableLog::new();
    // Intentionally NO CommitLogInvariant: we want to plant forks/regressions.
    let entries: Vec<Entry> = leaves.iter().map(|l| l.to_entry().unwrap()).collect();
    for e in &entries {
        log.append(e.clone()).unwrap();
    }

    let n = log.size();
    let mut sths: Vec<Sth> = Vec::new();
    let mut midpoint = None;
    if n >= 2 {
        let m = n / 2;
        midpoint = Some(m);
        sths.push(Sth::create(&signing_key, m as u64, log.root_at(m).unwrap(), TS));
    }
    sths.push(log.signed_tree_head(&signing_key, TS));
    let final_index = sths.len() - 1;

    let inclusion = (0..n)
        .map(|i| InclusionCheck {
            entry: entries[i].clone(),
            proof: log.inclusion_proof(i).unwrap(),
            sth_index: final_index,
        })
        .collect();

    let consistency = midpoint
        .map(|m| {
            vec![ConsistencyCheck {
                old_index: 0,
                new_index: final_index,
                proof: log.consistency_proof(m, n).unwrap(),
            }]
        })
        .unwrap_or_default();

    let bundle = Bundle {
        public_key: hex::encode(signing_key.verifying_key().encode()),
        retired_keys: Vec::new(),
        sths,
        entries,
        enforce_unique: vec!["mls-commit-log".to_string()],
        inclusion,
        consistency,
    };
    layout::generate(&bundle, root).unwrap();
    bundle
}

/// A tree with one healthy group (conv-a), a second healthy group (conv-b), a
/// forked group (conv-c) and an epoch-regressed group (conv-d), all interleaved
/// in seq order like a real commit log.
fn mixed_leaves() -> Vec<CommitLeaf> {
    vec![
        leaf("conv-a", 0, 1, "a0"),
        leaf("conv-b", 0, 2, "b0"),
        leaf("conv-a", 1, 3, "a1"),
        leaf("conv-b", 1, 4, "b1"),
        leaf("conv-a", 2, 5, "a2"),
        // conv-c: two commits at the SAME epoch 0 — a fork.
        leaf("conv-c", 0, 6, "c0"),
        leaf("conv-c", 0, 7, "c0-EVIL"),
        // conv-d: 0 -> 5 -> 3, an epoch regression.
        leaf("conv-d", 0, 8, "d0"),
        leaf("conv-d", 5, 9, "d5"),
        leaf("conv-d", 3, 10, "d3"),
    ]
}

/// GET a group report over HTTP, returning (status, CORS header, parsed report).
fn http_group(base: &str, id: &str) -> (u16, Option<String>, GroupReport) {
    let resp = ureq::get(&format!("{base}/verify/group/{id}")).call().unwrap();
    let status = resp.status();
    let cors = resp.header("Access-Control-Allow-Origin").map(str::to_string);
    let body = resp.into_string().unwrap();
    let report: GroupReport = serde_json::from_str(&body).unwrap();
    (status, cors, report)
}

#[test]
fn healthy_group_verifies_over_http() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &mixed_leaves());
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    let (status, cors, report) = http_group(&server.base_url(), "conv-a");
    assert_eq!(status, 200);
    // (c) CORS header is present so the static site can read it cross-origin.
    assert_eq!(cors.as_deref(), Some("*"));

    assert!(report.found, "conv-a should be found");
    assert!(report.chain_valid, "healthy group must verify: {:?}", report.violations);
    assert!(report.violations.is_empty());
    let epochs: Vec<u64> = report.commits.iter().map(|c| c.epoch).collect();
    assert_eq!(epochs, vec![0, 1, 2], "expected conv-a's three epochs in order");
    assert!(report.commits.iter().all(|c| c.included), "every commit must be included");
    assert!(report.sth_tree_size >= 10);

    server.shutdown();
}

#[test]
fn forked_group_fails() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &mixed_leaves());
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    let (_, _, report) = http_group(&server.base_url(), "conv-c");
    assert!(report.found);
    assert!(!report.chain_valid, "a forked group must fail");
    assert!(
        report.violations.iter().any(|v| v.contains("fork")),
        "expected a fork violation, got: {:?}",
        report.violations
    );
    // The commits themselves are still genuinely in the log (included) — it is
    // the per-group invariant that fails, not inclusion.
    assert_eq!(report.commits.len(), 2);
    assert!(report.commits.iter().all(|c| c.included));

    server.shutdown();
}

#[test]
fn epoch_regressed_group_fails() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &mixed_leaves());
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    let (_, _, report) = http_group(&server.base_url(), "conv-d");
    assert!(!report.chain_valid, "an epoch-regressed group must fail");
    assert!(
        report.violations.iter().any(|v| v.contains("regression")),
        "expected an epoch regression violation, got: {:?}",
        report.violations
    );

    server.shutdown();
}

#[test]
fn unknown_group_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &mixed_leaves());
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    let (status, _, report) = http_group(&server.base_url(), "conv-does-not-exist");
    assert_eq!(status, 200);
    assert!(!report.found, "a missing group must report found = false");
    assert!(report.commits.is_empty());

    server.shutdown();
}

#[test]
fn tampered_sth_signature_fails() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &mixed_leaves());

    // Flip a nibble in the latest STH's signature; the trust anchor no longer
    // verifies, so every group's chain is doomed.
    let latest = dir.path().join("v1").join("sth").join("latest.json");
    let mut sth: Sth = serde_json::from_str(&std::fs::read_to_string(&latest).unwrap()).unwrap();
    let mut sig: Vec<char> = sth.signature.chars().collect();
    sig[0] = if sig[0] == 'a' { 'b' } else { 'a' };
    sth.signature = sig.into_iter().collect();
    std::fs::write(&latest, serde_json::to_string_pretty(&sth).unwrap()).unwrap();

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let (_, _, report) = http_group(&server.base_url(), "conv-a");
    assert!(!report.chain_valid, "a tampered STH signature must fail");
    assert!(
        report.violations.iter().any(|v| v.contains("STH signature")),
        "expected an STH signature violation, got: {:?}",
        report.violations
    );

    server.shutdown();
}

#[test]
fn cli_and_http_return_the_same_report() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &mixed_leaves());
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let base = server.base_url();

    for id in ["conv-a", "conv-c", "conv-d", "conv-missing"] {
        // The CLI path IS this direct call (see bin/serve.rs run_verify_group).
        let cli = verify_group(&base, id).unwrap();
        let (_, _, http) = http_group(&base, id);
        assert_eq!(cli, http, "CLI and HTTP must agree for group `{id}`");
    }

    server.shutdown();
}

/// DoD #4: the static commit-log tree emits **no** per-group artifact and the
/// manifest no longer enumerates conversations — the two things that would leak
/// the group set next to pseudonymous leaves. Per-group verification is dynamic
/// and member-gated instead.
#[test]
fn generate_emits_no_precomputed_group_reports_or_conversation_list() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &mixed_leaves());

    // No `verify/group/` directory at all.
    assert!(
        !dir.path().join("verify").join("group").exists(),
        "the static commit-log tree must not precompute per-group reports (#701)"
    );

    // The manifest carries no `conversations` key and no raw conversation id.
    let index = std::fs::read_to_string(dir.path().join("v1").join("index.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&index).unwrap();
    assert!(
        value.get("conversations").is_none(),
        "index.json must not enumerate conversations: {index}"
    );
    for id in ["conv-a", "conv-b", "conv-c", "conv-d"] {
        assert!(!index.contains(id), "index.json leaked a raw conversation id: {id}");
    }

    // And the published entries themselves carry no raw ids — only pseudonyms.
    let entries = std::fs::read_to_string(dir.path().join("v1").join("entries.json")).unwrap();
    for raw in ["conv-a", "conv-b", "conv-c", "conv-d", "u-conv-a"] {
        assert!(!entries.contains(raw), "entries.json leaked a raw id: {raw}");
    }
}

// ---------------------------------------------------------------------------
// #701: windowing. A member (who knows the real conversation_id) verifies the
// whole conversation across windows; an outsider with only the dump cannot link
// the windows.
// ---------------------------------------------------------------------------

/// A conversation whose commits fall on both sides of a window boundary. `conv-w`
/// is healthy across the boundary; `conv-r` regresses across it (epoch 5 then 3).
fn windowed_leaves() -> Vec<CommitLeaf> {
    vec![
        leaf("conv-w", 0, 1, "w0"),
        leaf("conv-w", 1, 2, "w1"),
        // …crosses the boundary, still monotone.
        leaf("conv-w", 2, W + 1, "w2"),
        leaf("conv-w", 3, W + 2, "w3"),
        // conv-r: a regression that only shows up if you span the boundary.
        leaf("conv-r", 5, 3, "r5"),
        leaf("conv-r", 3, W + 3, "r3"),
    ]
}

/// DoD #2: someone who knows the real `conversation_id` verifies the whole chain
/// across ≥2 windows.
#[test]
fn a_member_verifies_a_conversation_across_windows() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &windowed_leaves());
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    let (status, _, report) = http_group(&server.base_url(), "conv-w");
    assert_eq!(status, 200);
    assert!(report.found, "conv-w must be found across windows");
    assert!(report.chain_valid, "a healthy cross-window chain must verify: {:?}", report.violations);
    // All four commits recovered, in seq order, spanning the boundary.
    let epochs: Vec<u64> = report.commits.iter().map(|c| c.epoch).collect();
    assert_eq!(epochs, vec![0, 1, 2, 3], "every window's commits must be recovered");
    assert!(report.commits.iter().all(|c| c.included));
    // The two windows really are distinct pseudonyms in the log.
    assert_ne!(
        derive_conversation_pseudonym("conv-w", window_for_seq(1)),
        derive_conversation_pseudonym("conv-w", window_for_seq(W + 1))
    );

    server.shutdown();
}

/// The member path is full-strength: a regression that straddles the window
/// boundary — invisible to a public per-pseudonym grouping — is caught because the
/// member regroups every window under the real id.
#[test]
fn a_member_catches_a_cross_window_regression() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &windowed_leaves());
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    let (_, _, report) = http_group(&server.base_url(), "conv-r");
    assert!(report.found);
    assert!(!report.chain_valid, "a cross-window regression must fail for a member");
    assert!(
        report.violations.iter().any(|v| v.contains("regression")),
        "expected a regression violation, got: {:?}",
        report.violations
    );

    server.shutdown();
}

/// DoD #3, at the serve boundary: from the entries dump alone you cannot link the
/// same conversation across windows, nor the same sender across conversations.
#[test]
fn the_dump_alone_does_not_link_across_windows_or_conversations() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = build_tree(dir.path(), &windowed_leaves());

    // Pull the published pseudonyms straight out of the leaves.
    let leaves: Vec<CommitLeaf> = bundle
        .entries
        .iter()
        .map(|e| CommitLeaf::decode(&e.data).unwrap())
        .collect();
    // conv-w appears under two *different* conversation pseudonyms (one per
    // window); nothing in the dump ties them together.
    let convw_pseudos: std::collections::BTreeSet<&str> = leaves
        .iter()
        .filter(|l| l.seq <= W + 2)
        .filter(|l| {
            l.conversation_pseudonym == derive_conversation_pseudonym("conv-w", window_for_seq(l.seq))
        })
        .map(|l| l.conversation_pseudonym.as_str())
        .collect();
    assert_eq!(convw_pseudos.len(), 2, "one conversation must show two unlinkable window pseudonyms");
}

#[test]
fn options_preflight_has_cors_headers() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &mixed_leaves());
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    // ureq treats a 204 as success; an OPTIONS preflight must advertise CORS.
    let resp = ureq::request("OPTIONS", &format!("{}/verify/group/conv-a", server.base_url()))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert_eq!(resp.header("Access-Control-Allow-Origin"), Some("*"));
    assert!(resp.header("Access-Control-Allow-Methods").is_some());

    server.shutdown();
}

// ---------------------------------------------------------------------------
// Suite generations (#454 P4). Kept in their own leaf set rather than folded
// into `mixed_leaves` so the manifest/report assertions above keep pinning the
// exact pre-P4 conversation list.
// ---------------------------------------------------------------------------

/// `conv-m` migrates to the hybrid suite (a successor lineage restarting at
/// epoch 0); `conv-x` fakes one by bumping the generation mid-lineage.
fn migrated_leaves() -> Vec<CommitLeaf> {
    vec![
        leaf_at("conv-m", 0, 0, 1, "m-g0-e0"),
        leaf_at("conv-m", 0, 1, 2, "m-g0-e1"),
        // The migration: epoch restarts under the successor generation.
        leaf_at("conv-m", 1, 0, 3, "m-g1-e0"),
        leaf_at("conv-m", 1, 1, 4, "m-g1-e1"),
        // conv-x opens generation 1 at epoch 9 — a fork wearing a migration's
        // clothes.
        leaf_at("conv-x", 0, 0, 5, "x-g0-e0"),
        leaf_at("conv-x", 1, 9, 6, "x-g1-e9"),
    ]
}

/// The user-visible half of the #454 P4 acceptance criterion: after a real suite
/// migration, "verify this conversation" must still say **valid**. A verifier
/// that reported every migrated group as tampered would be worse than no
/// verifier — it trains people to ignore it.
#[test]
fn a_migrated_group_still_verifies() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &migrated_leaves());
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    let (status, _, report) = http_group(&server.base_url(), "conv-m");
    assert_eq!(status, 200);
    assert!(report.found);
    assert!(
        report.chain_valid,
        "a migrated group must verify: {:?}",
        report.violations
    );
    // The report exposes the lineage, so a reader can see *why* the epoch
    // counter restarts instead of concluding the log lost commits.
    let lineage: Vec<(u64, u64)> =
        report.commits.iter().map(|c| (c.generation, c.epoch)).collect();
    assert_eq!(lineage, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
    assert!(report.commits.iter().all(|c| c.included));

    server.shutdown();
}

/// …and the widened key still has teeth: a generation bump is not a licence to
/// pick an arbitrary epoch.
#[test]
fn a_forged_migration_fails() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &migrated_leaves());
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    let (_, _, report) = http_group(&server.base_url(), "conv-x");
    assert!(report.found);
    assert!(!report.chain_valid, "a forged migration must fail");
    assert!(
        report.violations.iter().any(|v| v.contains("must start at epoch 0")),
        "expected a lineage-opening violation, got: {:?}",
        report.violations
    );

    server.shutdown();
}
