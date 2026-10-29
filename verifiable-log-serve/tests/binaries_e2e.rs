//! End-to-end gate for the **binaries** tree (binary transparency, Phase 1 of
//! #453) — the P1 acceptance criterion: `pollis-verify release <tag>` verifies a
//! fixture binaries tree against a synthetic STH.
//!
//! Fixtures come from the real builder and go through the shared verdict core.

use std::path::Path;

use ml_dsa::Keypair;
use verifiable_log::SigningKey;
use verifiable_log::{Entry, Sth, VerifiableLog};
use verifiable_log_builder::account_key::STH_CONTEXT as ACCOUNT_STH_CONTEXT;
use verifiable_log_builder::binaries::{self, BinaryRecord, Layer, Toolchain};
use verifiable_log_builder::{build_binaries_bundle, build_bundle, CommitRow};
use verifiable_log_serve::bundle::{Bundle, InclusionCheck};
use verifiable_log_serve::{layout, release, remote, DevServer};

const TS: u64 = 1_700_000_000_000;

fn signing_key() -> SigningKey {
    SigningKey::from_seed(&[9u8; 32].into())
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
/// and a payload-only Linux AppImage.
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
/// same trick `account_e2e` uses).
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

/// #944: the recipe recorded in each leaf must survive into the report, verbatim.
///
/// It must reach the report through the same verified path as the hashes.
#[test]
fn the_recorded_build_recipe_reaches_the_report() {
    let builder_bundle = build_binaries_bundle(&fixture_records(), &signing_key(), TS).unwrap();
    let bundle = to_serve_bundle(&builder_bundle);

    let report = release::verify_release_in_bundle(&bundle, "v1.3.0");

    assert!(report.found);
    assert!(!report.artifacts.is_empty());
    for artifact in &report.artifacts {
        assert_eq!(
            artifact.toolchain,
            toolchain(),
            "artifact `{}` must carry its leaf's recipe unchanged",
            artifact.artifact_name
        );
    }

    let linux = report
        .artifacts
        .iter()
        .find(|a| a.platform == "linux" && a.bundle == "appimage" && a.layer == Layer::Payload)
        .expect("the Linux AppImage payload leaf is the reproducible unit");
    assert_eq!(linux.toolchain.runner_image, "ubuntu-24.04@sha256:abc");

    // It must be in the serialized report, which is what `--json` hands the workflow.
    let json = serde_json::to_value(&report).unwrap();
    let runner_image = json["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["platform"] == "linux" && a["bundle"] == "appimage" && a["layer"] == "payload")
        .and_then(|a| a["toolchain"]["runner_image"].as_str());
    assert_eq!(runner_image, Some("ubuntu-24.04@sha256:abc"));
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

    assert_eq!(report.sth_tree_size, 5);
    assert_eq!(report.root_hex, expected_root);

    assert_eq!(report.artifacts.len(), 5);
    assert!(report.artifacts.iter().all(|a| a.included));

    // The macOS signed wrapper shares the payload hash but ships different bytes.
    let mac_signed = report
        .artifacts
        .iter()
        .find(|a| a.platform == "darwin" && a.layer == Layer::Signed)
        .expect("macOS signed artifact must be present");
    assert_eq!(mac_signed.payload_sha256, hex::encode([0x11u8; 32]));
    assert_eq!(mac_signed.artifact_sha256, hex::encode([0x1au8; 32]));
    assert_eq!(mac_signed.bundle, "dmg");

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
    // Forge the head under the account-key context: a binaries verifier must refuse it.
    resign_sths_under(&mut bundle, &signing_key(), ACCOUNT_STH_CONTEXT);

    let report = release::verify_release_in_bundle(&bundle, "v1.3.0");

    // The leaves are present, but the trust anchor is invalid.
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
    // A corrupted leaf no longer matches the STH root.
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
    // A forked tree the builder would refuse; the verifier must catch it on replay.
    let key = signing_key();
    let forked = vec![
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x11, 0x11),
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x99, 0x99),
    ];

    let mut log = VerifiableLog::new();
    let mut entries: Vec<Entry> = Vec::new();
    for r in &forked {
        let entry = r.to_entry().unwrap();
        log.append(entry.clone()).unwrap();
        entries.push(entry);
    }
    let size = log.size();
    let sth = Sth::create_with_context(&key, size as u64, log.root(), TS, binaries::STH_CONTEXT);
    let inclusion = (0..size)
        .map(|i| InclusionCheck {
            entry: entries[i].clone(),
            proof: log.inclusion_proof(i).unwrap(),
            sth_index: 0,
        })
        .collect();

    let bundle = Bundle {
        public_key: hex::encode(key.verifying_key().encode()),
        retired_keys: Vec::new(),
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

/// `verify_remote`'s prerequisites are the commit-log tree's `public_key.json` +
/// `index.json`, so a minimal commit-log tree must exist.
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
    assert!(bin.join("public_key.json").is_file());
    assert!(bin.join("index.json").is_file());
    assert!(bin.join("sth").join("latest.json").is_file());
    assert!(bin.join("entries.json").is_file());
    assert!(bin.join("sth").join("5.json").is_file());

    // The per-release report has no extension (the file is the URL).
    let report_path = dir.path().join("verify").join("release").join("v1.3.0");
    assert!(report_path.is_file());

    let manifest: verifiable_log_serve::BinaryManifest =
        serde_json::from_str(&std::fs::read_to_string(bin.join("index.json")).unwrap()).unwrap();
    assert_eq!(manifest.entry_count, fixture_records().len() as u64);
    assert_eq!(manifest.tags, vec!["v1.3.0".to_string()]);
    assert_eq!(manifest.enforce_unique, vec![binaries::TENANT.to_string()]);
    assert_eq!(manifest.latest_tree_size, Some(5));

    let served: release::ReleaseReport =
        serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    assert!(served.chain_valid && served.found);
    assert_eq!(served.artifacts.len(), 5);
}

#[test]
fn remote_verify_covers_the_binaries_tree() {
    let dir = tempfile::tempdir().unwrap();
    generate_commit_tree(dir.path());
    generate_binaries_tree(dir.path());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    let report = remote::verify_remote(&server.base_url()).unwrap();
    assert!(report.ok, "all trees should verify; checks: {:?}", report.checks);
    let binaries_checks = report
        .checks
        .iter()
        .filter(|(_, label)| label.starts_with("binaries:"))
        .count();
    assert!(binaries_checks >= 4, "expected binaries checks, got {binaries_checks}");
    assert!(
        !report.notes.iter().any(|n| n.contains("binaries")),
        "unexpected binaries note: {:?}",
        report.notes
    );

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
    generate_commit_tree(dir.path());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();

    assert!(report.ok, "absent binaries tree must not fail the audit");
    assert!(
        report.notes.iter().any(|n| n.contains("binaries")),
        "expected an absent-binaries note; got: {:?}",
        report.notes
    );
}

/// One leaf of a mobile over-the-air JS update (#1250), in the exact shape
/// `mobile/scripts/ota/lib.ts` `attestedFiles` writes.
fn ota_record(group: &str, platform: &str, runtime_version: &str, bundle: &str, digest: u8) -> BinaryRecord {
    let tag = format!("mobile-ota-{group}");
    let artifact_name = format!("pollis-ota-{group}-{platform}.{bundle}");
    BinaryRecord {
        release_tag: tag.clone(),
        commit: "e".repeat(40),
        platform: platform.to_string(),
        arch: runtime_version.to_string(),
        bundle: bundle.to_string(),
        artifact_name: artifact_name.clone(),
        layer: Layer::Payload,
        payload_sha256: hex::encode([digest; 32]),
        artifact_sha256: hex::encode([digest; 32]),
        toolchain: toolchain(),
        provenance_uri: format!("cdn.pollis.com/releases/mobile-ota/{tag}/{artifact_name}.intoto.jsonl"),
    }
}

/// OTA updates share the binaries tree with desktop releases without any change
/// to the frozen leaf contract: publishes, rollbacks and republishes all pass.
#[test]
fn ota_update_groups_verify_alongside_desktop_releases() {
    let rv_ios = "376466c81d9428da79f80525da44567b98aa4e4a";
    let rv_ios_old = "0000000000000000000000000000000000000001";
    let rv_android = "650428a8330d3d6e164e93a21f4fa73aa7088395";
    let publish = "11111111-1111-4111-8111-111111111111";
    let rollback = "22222222-2222-4222-8222-222222222222";
    let republish = "33333333-3333-4333-8333-333333333333";

    let mut records = fixture_records();
    for (platform, rv, base) in [("ios", rv_ios, 0x40u8), ("android", rv_android, 0x50u8)] {
        records.push(ota_record(publish, platform, rv, "ota-manifest", base));
        records.push(ota_record(publish, platform, rv, "ota-bundle", base + 1));
        records.push(ota_record(publish, platform, rv, "ota-assets", base + 2));
    }
    records.push(ota_record(rollback, "ios", rv_ios, "ota-directive", 0x60));
    records.push(ota_record(rollback, "ios", rv_ios_old, "ota-directive", 0x61));
    for (platform, rv, base) in [("ios", rv_ios, 0x40u8), ("android", rv_android, 0x50u8)] {
        records.push(ota_record(republish, platform, rv, "ota-manifest", base + 8));
        records.push(ota_record(republish, platform, rv, "ota-bundle", base + 1));
        records.push(ota_record(republish, platform, rv, "ota-assets", base + 2));
    }

    let bundle = to_serve_bundle(&build_binaries_bundle(&records, &signing_key(), TS).unwrap());

    let report = release::verify_release_in_bundle(&bundle, &format!("mobile-ota-{publish}"));
    assert!(report.chain_valid, "violations: {:?}", report.violations);
    assert_eq!(report.artifacts.len(), 6);
    assert!(report.artifacts.iter().all(|a| a.included && a.layer == Layer::Payload));
    let ios_bundle = report
        .artifacts
        .iter()
        .find(|a| a.platform == "ios" && a.bundle == "ota-bundle")
        .expect("the iOS launch bundle leaf");
    assert_eq!(ios_bundle.arch, rv_ios);

    let rolled = release::verify_release_in_bundle(&bundle, &format!("mobile-ota-{rollback}"));
    assert!(rolled.chain_valid, "violations: {:?}", rolled.violations);
    assert_eq!(rolled.artifacts.len(), 2);

    assert!(release::verify_release_in_bundle(&bundle, "v1.3.0").chain_valid);

    // A group appended twice is out of publish order, so `log-append` skips present tags.
    let mut twice = records.clone();
    twice.push(ota_record(publish, "ios", rv_ios, "ota-manifest", 0x40));
    let err = build_binaries_bundle(&twice, &signing_key(), TS).unwrap_err();
    assert!(err.to_string().contains("out of publish order"), "got: {err}");
}
