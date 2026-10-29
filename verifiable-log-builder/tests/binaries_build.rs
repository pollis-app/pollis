//! Deterministic gate suite for the **binaries** builder tenant (binary
//! transparency, Phase 1 of #453).

use std::path::Path;
use std::process::Command;

use ml_dsa::Keypair;
use verifiable_log::monitor::{verify_bundle, VerifyOptions};
use verifiable_log::SigningKey;
use verifiable_log::{TenantInvariant, STH_CONTEXT_COMMIT_LOG};
use verifiable_log_builder::account_key::STH_CONTEXT as ACCOUNT_STH_CONTEXT;
use verifiable_log_builder::binaries::{
    BinaryInvariant, BinaryRecord, Layer, Toolchain, STH_CONTEXT, TENANT,
};
use verifiable_log_builder::build_binaries_bundle;
use verifiable_log_builder::builder::Bundle;

const TS: u64 = 1_700_000_000_000;
const KEY_HEX: &str = "0909090909090909090909090909090909090909090909090909090909090909";

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

/// One release on three platforms: payload+signed on macOS/Windows, payload-only Linux.
fn fixture_records() -> Vec<BinaryRecord> {
    vec![
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x11, 0x11),
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Signed, 0x11, 0x1a),
        record("v1.3.0", "linux", "x86_64", "appimage", Layer::Payload, 0x22, 0x22),
        record("v1.3.0", "windows", "x86_64", "nsis", Layer::Payload, 0x33, 0x33),
        record("v1.3.0", "windows", "x86_64", "nsis", Layer::Signed, 0x33, 0x3a),
    ]
}

/// Run the **real** monitor pass over a bundle under `context`, with the
/// binaries tenant invariant enforced on replay (#875).
fn verify_binaries(bundle: &Bundle, context: &[u8]) -> bool {
    let invariant: Box<dyn TenantInvariant> = Box::new(BinaryInvariant);
    verify_bundle(
        bundle,
        &VerifyOptions {
            context,
            now_ms: TS,
        },
        vec![(TENANT.to_string(), invariant)],
        &mut |_, _| {},
    )
}

/// The binaries tree's own context — how an auditor checks this bundle.
fn monitor_verify_binaries(bundle: &Bundle) -> bool {
    verify_binaries(bundle, STH_CONTEXT)
}

/// Drive the real `builder build-binaries` over a fixture `records.json` in `dir`.
fn run_build_binaries(dir: &Path, records: &[BinaryRecord], ts: u64) -> Bundle {
    let records_in = dir.join("records.json");
    let bundle_out = dir.join("binaries-bundle.json");
    std::fs::write(&records_in, serde_json::to_string_pretty(records).unwrap()).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_builder"))
        .args([
            "build-binaries",
            "--binaries-in",
            records_in.to_str().unwrap(),
            "--out",
            bundle_out.to_str().unwrap(),
            "--timestamp",
            &ts.to_string(),
        ])
        .env("VLOG_SIGNING_KEY", KEY_HEX)
        .status()
        .expect("failed to spawn builder");
    assert!(status.success(), "build-binaries must succeed on a valid fixture");

    serde_json::from_str(&std::fs::read_to_string(&bundle_out).unwrap()).unwrap()
}

#[test]
fn build_binaries_mode_emits_wellformed_signed_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let records = fixture_records();
    let bundle = run_build_binaries(dir.path(), &records, TS);

    assert_eq!(bundle.entries.len(), records.len());
    assert_eq!(bundle.inclusion.len(), records.len());
    assert!(bundle.entries.iter().all(|e| e.tenant == TENANT));
    assert_eq!(bundle.sths.len(), 2);
    assert_eq!(bundle.consistency.len(), 1);
    assert_eq!(bundle.enforce_unique, vec![TENANT.to_string()]);
    assert_eq!(
        bundle.public_key,
        hex::encode(signing_key().verifying_key().encode())
    );

    assert!(
        monitor_verify_binaries(&bundle),
        "binaries bundle must verify under the binaries domain context"
    );

    // Domain separation against both sibling trees.
    assert!(
        !verify_binaries(&bundle, STH_CONTEXT_COMMIT_LOG),
        "a binaries head must NOT verify as a commit-log head"
    );
    assert!(
        !verify_binaries(&bundle, ACCOUNT_STH_CONTEXT),
        "a binaries head must NOT verify as an account-key head"
    );

    let json = serde_json::to_string_pretty(&bundle).unwrap();
    let reparsed: Bundle = serde_json::from_str(&json).unwrap();
    assert!(monitor_verify_binaries(&reparsed));
}

/// The signed STH must be byte-stable across rebuilds for a reused timestamp, so
/// an unchanged binaries tree republishes without diverging from its published head.
#[test]
fn rebuild_is_byte_identical_for_a_reused_timestamp() {
    let records = fixture_records();

    let first =
        serde_json::to_string(&build_binaries_bundle(&records, &signing_key(), TS).unwrap())
            .unwrap();
    let again =
        serde_json::to_string(&build_binaries_bundle(&records, &signing_key(), TS).unwrap())
            .unwrap();
    assert_eq!(
        first, again,
        "same records + same timestamp must reproduce the bundle byte-for-byte"
    );

    let moved =
        serde_json::to_string(&build_binaries_bundle(&records, &signing_key(), TS + 1).unwrap())
            .unwrap();
    assert_ne!(
        first, moved,
        "a different timestamp must change the signed STH bytes"
    );
}

#[test]
fn fork_same_tuple_different_hash_aborts_the_build() {
    // Same released unit, different bytes: a silent re-issue the log must refuse.
    let records = vec![
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x11, 0x11),
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x99, 0x99),
    ];
    let err = build_binaries_bundle(&records, &signing_key(), TS).unwrap_err();
    assert!(err.to_string().contains("fork"), "expected a fork violation, got: {err}");
}

#[test]
fn signed_without_payload_aborts_the_build() {
    let records = vec![record(
        "v1.3.0", "windows", "x86_64", "nsis", Layer::Signed, 0x44, 0x55,
    )];
    let err = build_binaries_bundle(&records, &signing_key(), TS).unwrap_err();
    assert!(
        err.to_string().contains("no matching payload"),
        "expected a payload-pairing violation, got: {err}"
    );
}

#[test]
fn tag_out_of_publish_order_aborts_the_build() {
    let records = vec![
        record("v1.0.0", "linux", "x86_64", "appimage", Layer::Payload, 0x11, 0x11),
        record("v1.1.0", "linux", "x86_64", "appimage", Layer::Payload, 0x22, 0x22),
        record("v1.0.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x33, 0x33),
    ];
    let err = build_binaries_bundle(&records, &signing_key(), TS).unwrap_err();
    assert!(
        err.to_string().contains("out of publish order"),
        "expected a tag-order violation, got: {err}"
    );
}

#[test]
fn tampered_binary_entry_fails_verification() {
    let records = fixture_records();
    let mut bundle = build_binaries_bundle(&records, &signing_key(), TS).unwrap();
    assert!(monitor_verify_binaries(&bundle));

    // The flipped leaf no longer matches the STH root the proof reconstructs.
    bundle.entries[0].data[0] ^= 0xff;
    assert!(
        !monitor_verify_binaries(&bundle),
        "a tampered binary entry must fail the monitor"
    );
}
