//! End-to-end gate for the **binaries** tree (binary transparency, Phase 1 of
//! #453) — the P1 acceptance criterion: `pollis-verify release <tag>` verifies a
//! fixture binaries tree against a synthetic STH.
//!
//! It mirrors `account_e2e.rs` for the third tenant, but drives the shared
//! verdict core directly: fixtures are built with the REAL builder
//! (`build_binaries_bundle`, STHs signed under the binaries domain context —
//! exactly what production emits), round-tripped into the serve crate's wire
//! shape, and handed to [`release::verify_release_in_bundle`] — the one function
//! the CLI (`pollis-verify release`) and the static `/verify/release/<tag>`
//! report both call, so their verdicts can never diverge.
//!
//! Coverage:
//! * POSITIVE — a well-formed tag verifies: `chain_valid`, `found`, the expected
//!   artifacts + hashes, and the STH size/root are all correct.
//! * NEGATIVE (the teeth):
//!   (a) an STH signed under the WRONG domain context (the account-key tree's)
//!       is rejected as an untrustworthy head;
//!   (b) a tampered leaf is rejected;
//!   (c) a tag with no artifacts reports `found == false`;
//!   (d) a forked tree (same released unit, different bytes) is caught on the
//!       verifier's independent invariant replay.

use std::path::Path;

use ed25519_dalek::SigningKey;
use verifiable_log::{Entry, Sth, VerifiableLog};
use verifiable_log_builder::account_key::STH_CONTEXT as ACCOUNT_STH_CONTEXT;
use verifiable_log_builder::binaries::{self, BinaryRecord, Layer, Toolchain};
use verifiable_log_builder::{build_binaries_bundle, build_bundle, CommitRow};
use verifiable_log_serve::bundle::{Bundle, InclusionCheck};
use verifiable_log_serve::{layout, release, remote, DevServer};

const TS: u64 = 1_700_000_000_000;

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

fn toolchain() -> Toolchain {
    Toolchain {
        rustc: "1.83.0".to_string(),
        node: "20.11.1".to_string(),
        pnpm: "9.1.0".to_string(),
        runner_image: "ubuntu-24.04@sha256:abc".to_string(),
        source_date_epoch: 1_700_000_000,
    }
}

fn record(
    tag: &str,
    platform: &str,
    arch: &str,
    bundle: &str,
    layer: Layer,
    payload: u8,
    artifact: u8,
) -> BinaryRecord {
    BinaryRecord {
        release_tag: tag.to_string(),
        commit: "f".repeat(40),
        platform: platform.to_string(),
        arch: arch.to_string(),
        bundle: bundle.to_string(),
        artifact_name: format!("pollis-{tag}-{platform}.{bundle}"),
        layer,
        payload_sha256: hex::encode([payload; 32]),
        artifact_sha256: hex::encode([artifact; 32]),
        toolchain: toolchain(),
        provenance_uri: format!("cdn.pollis.com/releases/{tag}/{platform}.intoto.jsonl"),
    }
}

/// One release, three platforms, with payload+signed pairs on macOS and Windows
/// and a payload-only Linux AppImage — five artifact leaves for `v1.3.0`.
fn fixture_records() -> Vec<BinaryRecord> {
    vec![
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x11, 0x11),
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Signed, 0x11, 0x1a),
        record("v1.3.0", "linux", "x86_64", "appimage", Layer::Payload, 0x22, 0x22),
        record("v1.3.0", "windows", "x86_64", "nsis", Layer::Payload, 0x33, 0x33),
        record("v1.3.0", "windows", "x86_64", "nsis", Layer::Signed, 0x33, 0x3a),
    ]
}

/// Round-trip a builder bundle into the serve crate's identical wire shape (the
/// same trick `account_e2e` uses), so the fixture is genuine production output.
fn to_serve_bundle(b: &verifiable_log_builder::Bundle) -> Bundle {
    serde_json::from_slice(&serde_json::to_vec(b).unwrap()).unwrap()
}

/// Build the fixture binaries tree with the real builder and hand it over as a
/// serve `Bundle` (STHs signed under the binaries domain context).
fn fixture_bundle() -> Bundle {
    let builder_bundle = build_binaries_bundle(&fixture_records(), &signing_key(), TS).unwrap();
    to_serve_bundle(&builder_bundle)
}

/// Re-sign every STH in `bundle` under an explicit domain `context`, keeping the
/// same (size, root, timestamp). Used to forge a head signed for the WRONG tree.
fn resign_sths_under(bundle: &mut Bundle, key: &SigningKey, context: &[u8]) {
    for sth in &mut bundle.sths {
        let root = sth.root_bytes().unwrap();
        *sth = Sth::create_with_context(key, sth.tree_size, root, sth.timestamp, context);
    }
}

#[test]
fn wellformed_release_verifies_with_expected_artifacts() {
    let builder_bundle = build_binaries_bundle(&fixture_records(), &signing_key(), TS).unwrap();
    let expected_root = builder_bundle.sths.last().unwrap().root_hash.clone();
    let bundle = to_serve_bundle(&builder_bundle);

    let report = release::verify_release_in_bundle(&bundle, "v1.3.0");

    assert!(report.found, "the tag's artifacts must be found");
    assert!(
        report.chain_valid,
        "a well-formed release must verify; violations: {:?}",
        report.violations
    );
    assert!(report.violations.is_empty());

    // The head everything was checked against is the size-5 final STH.
    assert_eq!(report.sth_tree_size, 5);
    assert_eq!(report.root_hex, expected_root);

    // All five artifacts are listed, in publish order, each provably included.
    assert_eq!(report.artifacts.len(), 5);
    assert!(report.artifacts.iter().all(|a| a.included));

    // Spot-check the macOS signed wrapper: it wraps the reproducible payload
    // (shared payload_sha256) but ships different signed bytes.
    let mac_signed = report
        .artifacts
        .iter()
        .find(|a| a.platform == "darwin" && a.layer == Layer::Signed)
        .expect("macOS signed artifact must be present");
    assert_eq!(mac_signed.payload_sha256, hex::encode([0x11u8; 32]));
    assert_eq!(mac_signed.artifact_sha256, hex::encode([0x1au8; 32]));
    assert_eq!(mac_signed.bundle, "dmg");

    // The Linux AppImage is a payload-only leaf (artifact == payload hash).
    let linux = report
        .artifacts
        .iter()
        .find(|a| a.platform == "linux")
        .expect("linux artifact must be present");
    assert_eq!(linux.layer, Layer::Payload);
    assert_eq!(linux.artifact_sha256, linux.payload_sha256);
}

#[test]
fn sth_signed_under_wrong_domain_context_is_rejected() {
    let mut bundle = fixture_bundle();
    // Forge the head under the ACCOUNT-KEY tree's context: same key, same tree,
    // but a binaries verifier must refuse a head minted for a sibling tree.
    resign_sths_under(&mut bundle, &signing_key(), ACCOUNT_STH_CONTEXT);

    let report = release::verify_release_in_bundle(&bundle, "v1.3.0");

    // The tag's leaves are still present, but the trust anchor is invalid, so the
    // whole tree is untrustworthy.
    assert!(report.found, "the entries are still there...");
    assert!(
        !report.chain_valid,
        "a head signed under the wrong domain context must NOT verify"
    );
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.contains("signature") && v.contains("trustworthy")),
        "expected a trust-anchor/signature violation; got: {:?}",
        report.violations
    );
}

#[test]
fn tampered_leaf_is_rejected() {
    let mut bundle = fixture_bundle();
    // Corrupt one committed leaf's bytes: its leaf hash no longer matches the STH
    // root the inclusion proof reconstructs (and its replay may also break the
    // invariant) — either way the tree is no longer trustworthy.
    bundle.entries[0].data[20] ^= 0xff;

    let report = release::verify_release_in_bundle(&bundle, "v1.3.0");
    assert!(
        !report.chain_valid,
        "a tampered leaf must be rejected; violations: {:?}",
        report.violations
    );
    assert!(!report.violations.is_empty());
}

#[test]
fn unknown_tag_reports_not_found() {
    let bundle = fixture_bundle();
    let report = release::verify_release_in_bundle(&bundle, "v9.9.9");

    assert!(!report.found, "a tag with no artifacts must report found == false");
    assert!(report.artifacts.is_empty());
}

#[test]
fn forked_tree_fails_the_verifiers_independent_replay() {
    // Two leaves describing the SAME released unit (tag/platform/arch/bundle/layer)
    // but different artifact bytes. The BUILDER would refuse to seal this (covered
    // in binaries_build.rs); here we assemble a signed tree that hides the fork —
    // no invariant registered — and prove the VERIFIER catches it on replay, the
    // independent re-check that keeps the CLI and the app honest.
    let key = signing_key();
    let forked = vec![
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x11, 0x11),
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x99, 0x99),
    ];

    let mut log = VerifiableLog::new();
    let mut entries: Vec<Entry> = Vec::new();
    for r in &forked {
        let entry = r.to_entry().unwrap();
        // No BinaryInvariant registered — the fork is admitted into the tree.
        log.append(entry.clone()).unwrap();
        entries.push(entry);
    }
    let size = log.size();
    // A genuine, correctly-signed binaries STH over the forked tree.
    let sth = Sth::create_with_context(&key, size as u64, log.root(), TS, binaries::STH_CONTEXT);
    let inclusion = (0..size)
        .map(|i| InclusionCheck {
            entry: entries[i].clone(),
            proof: log.inclusion_proof(i).unwrap(),
            sth_index: 0,
        })
        .collect();

    let bundle = Bundle {
        public_key: hex::encode(key.verifying_key().to_bytes()),
        sths: vec![sth],
        entries,
        enforce_unique: vec![binaries::TENANT.to_string()],
        inclusion,
        consistency: Vec::new(),
    };

    let report = release::verify_release_in_bundle(&bundle, "v1.3.0");
    assert!(
        !report.chain_valid,
        "the verifier's invariant replay must catch a fork the STH signature hides"
    );
    assert!(
        report.violations.iter().any(|v| v.contains("fork")),
        "expected a fork violation on replay; got: {:?}",
        report.violations
    );
}

// ── Static-tree generation + remote self-audit (the P2 publish/verify path) ──

/// `verify_remote`'s prerequisites are the commit-log tree's `public_key.json` +
/// `index.json`, so a minimal (empty) commit-log tree must exist for a
/// whole-log audit to run. Emit one alongside whatever else the test writes.
fn generate_commit_tree(root: &Path) {
    let empty: Vec<CommitRow> = Vec::new();
    let builder_bundle = build_bundle(&empty, &signing_key(), TS).unwrap();
    let bundle = to_serve_bundle(&builder_bundle);
    layout::generate(&bundle, root).unwrap();
}

/// Generate the binaries tree into `root` from the real builder's fixture bundle.
fn generate_binaries_tree(root: &Path) {
    let builder_bundle = build_binaries_bundle(&fixture_records(), &signing_key(), TS).unwrap();
    let bundle = to_serve_bundle(&builder_bundle);
    layout::generate_binaries(&bundle, root).unwrap();
}

#[test]
fn binaries_tree_generates_documented_files() {
    let dir = tempfile::tempdir().unwrap();
    generate_binaries_tree(dir.path());

    let bin = dir.path().join("v1").join("binaries");
    // The binaries subtree mirrors the other trees one level down.
    assert!(bin.join("public_key.json").is_file());
    assert!(bin.join("index.json").is_file());
    assert!(bin.join("sth").join("latest.json").is_file());
    assert!(bin.join("entries.json").is_file());
    // A five-leaf fixture → a midpoint + final STH and a consistency proof.
    assert!(bin.join("sth").join("5.json").is_file());

    // The precomputed per-release report exists with no extension (file IS URL).
    let report_path = dir.path().join("verify").join("release").join("v1.3.0");
    assert!(report_path.is_file());

    // The manifest advertises the release tag and enforces the binaries invariant.
    let manifest: verifiable_log_serve::BinaryManifest =
        serde_json::from_str(&std::fs::read_to_string(bin.join("index.json")).unwrap()).unwrap();
    assert_eq!(manifest.entry_count, fixture_records().len() as u64);
    assert_eq!(manifest.tags, vec!["v1.3.0".to_string()]);
    assert_eq!(manifest.enforce_unique, vec![binaries::TENANT.to_string()]);
    assert_eq!(manifest.latest_tree_size, Some(5));

    // The precomputed report is byte-identical to the shared verdict core.
    let served: release::ReleaseReport =
        serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert!(served.chain_valid && served.found);
    assert_eq!(served.artifacts.len(), 5);
}

#[test]
fn remote_verify_covers_the_binaries_tree() {
    let dir = tempfile::tempdir().unwrap();
    // The commit-log tree is a prerequisite for whole-log remote verification;
    // the binaries tree is the third tenant served alongside it.
    generate_commit_tree(dir.path());
    generate_binaries_tree(dir.path());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    // Whole-log remote verification now covers the binaries tree and passes.
    let report = remote::verify_remote(&server.base_url()).unwrap();
    assert!(report.ok, "all trees should verify; checks: {:?}", report.checks);
    let binaries_checks = report
        .checks
        .iter()
        .filter(|(_, label)| label.starts_with("binaries:"))
        .count();
    assert!(binaries_checks >= 4, "expected binaries checks, got {binaries_checks}");
    // Present tree → no "absent" note about binaries.
    assert!(
        !report.notes.iter().any(|n| n.contains("binaries")),
        "unexpected binaries note: {:?}",
        report.notes
    );

    // And the CLI's per-release verifier passes over HTTP against the same tree.
    let release_report = release::verify_release(&server.base_url(), "v1.3.0").unwrap();
    assert!(
        release_report.chain_valid,
        "release verify must pass; violations: {:?}",
        release_report.violations
    );
    assert_eq!(release_report.artifacts.len(), 5);
}

#[test]
fn remote_verify_notes_absent_binaries_tree() {
    let dir = tempfile::tempdir().unwrap();
    // Only the commit-log tree — no binaries subtree published yet.
    generate_commit_tree(dir.path());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();

    // An absent binaries tree is a note, not a failure.
    assert!(report.ok, "absent binaries tree must not fail the audit");
    assert!(
        report.notes.iter().any(|n| n.contains("binaries")),
        "expected an absent-binaries note; got: {:?}",
        report.notes
    );
}
