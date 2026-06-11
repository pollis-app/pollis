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

use ed25519_dalek::SigningKey;
use verifiable_log::{Entry, Sth, VerifiableLog};
use verifiable_log_builder::CommitLeaf;
use verifiable_log_serve::bundle::{Bundle, ConsistencyCheck, InclusionCheck};
use verifiable_log_serve::group::{verify_group, verify_group_in_bundle, GroupReport};
use verifiable_log_serve::{layout, DevServer, Manifest};

const TS: u64 = 1_700_000_000_000;

fn leaf(conv: &str, epoch: u64, seq: i64, commit: &str) -> CommitLeaf {
    CommitLeaf {
        conversation_id: conv.to_string(),
        epoch,
        sender_id: format!("u-{conv}"),
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
    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
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
        public_key: hex::encode(signing_key.verifying_key().to_bytes()),
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

#[test]
fn generate_emits_precomputed_group_reports() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = build_tree(dir.path(), &mixed_leaves());

    // Every distinct conversation in the bundle gets a precomputed report at
    // verify/group/<id> — no `.json` suffix, the file path IS the endpoint URL.
    for id in ["conv-a", "conv-b", "conv-c", "conv-d"] {
        let path = dir.path().join("verify").join("group").join(id);
        assert!(path.is_file(), "expected a precomputed report file for {id}");

        let on_disk = std::fs::read(&path).unwrap();
        // Byte-identical to the shared verifier's compact JSON — which is exactly
        // what the live `GET /verify/group/<id>` endpoint serializes and returns.
        let expected = serde_json::to_vec(&verify_group_in_bundle(&bundle, id)).unwrap();
        assert_eq!(on_disk, expected, "report for {id} must be byte-identical to the shared verifier");

        // And it round-trips back into the same report the endpoint would return.
        let report: GroupReport = serde_json::from_slice(&on_disk).unwrap();
        assert_eq!(report.group_id, id);
    }

    // The precomputed verdicts carry the right answer per group: healthy passes,
    // forked/regressed fail — proving the report content, not just its presence.
    let read = |id: &str| -> GroupReport {
        serde_json::from_slice(&std::fs::read(dir.path().join("verify").join("group").join(id)).unwrap())
            .unwrap()
    };
    assert!(read("conv-a").chain_valid, "healthy group's report must pass");
    assert!(!read("conv-c").chain_valid, "forked group's report must fail");
    assert!(!read("conv-d").chain_valid, "regressed group's report must fail");

    // A conversation never present in the bundle gets no precomputed file (it is
    // only ever answered dynamically).
    assert!(!dir.path().join("verify").join("group").join("conv-missing").exists());
}

#[test]
fn precomputed_report_matches_live_endpoint_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &mixed_leaves());

    // The dynamic endpoint shadows the static file on the dev server, so fetch
    // the live response and compare it to the precomputed file on disk: the
    // static host (R2) serves this exact file, with no server on the path.
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    for id in ["conv-a", "conv-c", "conv-d"] {
        let live = ureq::get(&format!("{}/verify/group/{id}", server.base_url()))
            .call()
            .unwrap()
            .into_string()
            .unwrap();
        let on_disk =
            std::fs::read_to_string(dir.path().join("verify").join("group").join(id)).unwrap();
        assert_eq!(on_disk, live, "precomputed file for {id} must match the live endpoint body");
    }
    server.shutdown();
}

#[test]
fn index_advertises_the_conversation_list() {
    let dir = tempfile::tempdir().unwrap();
    build_tree(dir.path(), &mixed_leaves());

    let manifest: Manifest =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("v1").join("index.json")).unwrap())
            .unwrap();

    // Distinct, sorted, one entry per conversation that has a report file.
    assert_eq!(
        manifest.conversations,
        vec![
            "conv-a".to_string(),
            "conv-b".to_string(),
            "conv-c".to_string(),
            "conv-d".to_string(),
        ],
        "index.json must list every conversation with a precomputed report"
    );
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
