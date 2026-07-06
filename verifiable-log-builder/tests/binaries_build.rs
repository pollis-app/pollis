//! Deterministic gate suite for the **binaries** builder tenant (binary
//! transparency, Phase 1 of #453).
//!
//! Mirrors `account_build.rs` for the third tenant: drives the builder's
//! `build-binaries` mode over a fixture `records.json` (a few `BinaryRecord`s
//! across platforms, with payload/signed pairs), then verifies the emitted
//! bundle through the slice-1 monitor path — checking STH signatures under the
//! **binaries** domain context, since the binaries tree is domain-separated from
//! both the commit log and the account-key tree. Also exercises the invariant's
//! build-time rejections (fork, monotonic tag order, payload/signed pairing) and
//! tamper detection.
//!
//! The `build-binaries` mode reads a JSON file (not a DB), so — unlike the
//! account tenant — the fixture is a `records.json` handed to the real CLI
//! binary, exactly what the release job produces.

use std::path::Path;
use std::process::Command;

use ed25519_dalek::SigningKey;
use verifiable_log::{
    verify_consistency_proof, verify_inclusion_proof, verifying_key_from_hex, VerifiableLog,
};
use verifiable_log_builder::account_key::STH_CONTEXT as ACCOUNT_STH_CONTEXT;
use verifiable_log_builder::binaries::{
    BinaryInvariant, BinaryRecord, Layer, Toolchain, STH_CONTEXT, TENANT,
};
use verifiable_log_builder::build_binaries_bundle;
use verifiable_log_builder::builder::Bundle;

const TS: u64 = 1_700_000_000_000;
/// 32-byte hex Ed25519 signing key handed to the CLI via `VLOG_SIGNING_KEY`.
const KEY_HEX: &str = "0909090909090909090909090909090909090909090909090909090909090909";

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

/// A well-formed fixture: one release across three platforms, with a
/// payload+signed pair on macOS and Windows and a payload-only Linux AppImage.
fn fixture_records() -> Vec<BinaryRecord> {
    vec![
        // macOS: reproducible payload, then the notarized signed wrapper.
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x11, 0x11),
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Signed, 0x11, 0x1a),
        // Linux AppImage: reproducible payload only (no separate signature layer).
        record("v1.3.0", "linux", "x86_64", "appimage", Layer::Payload, 0x22, 0x22),
        // Windows: reproducible payload, then the Authenticode-signed wrapper.
        record("v1.3.0", "windows", "x86_64", "nsis", Layer::Payload, 0x33, 0x33),
        record("v1.3.0", "windows", "x86_64", "nsis", Layer::Signed, 0x33, 0x3a),
    ]
}

/// Faithful in-process re-implementation of `monitor verify` for the binaries
/// tree: built ONLY on the public slice-1 verifiers, but with STH signatures
/// checked under [`STH_CONTEXT`] (the binaries tree's domain separation) and the
/// [`BinaryInvariant`] enforced on replay.
fn monitor_verify_binaries(bundle: &Bundle) -> bool {
    let vk = match verifying_key_from_hex(&bundle.public_key) {
        Ok(k) => k,
        Err(_) => return false,
    };

    // STHs must verify under the BINARIES context — and must NOT verify under the
    // default commit-log context or the account-key context (proving the domain
    // separation holds against BOTH sibling trees).
    for sth in &bundle.sths {
        if !sth.verify_with_context(&vk, STH_CONTEXT) {
            return false;
        }
        if sth.verify(&vk) {
            return false;
        }
        if sth.verify_with_context(&vk, ACCOUNT_STH_CONTEXT) {
            return false;
        }
    }

    if !bundle.entries.is_empty() {
        let mut log = VerifiableLog::new();
        log.register_invariant(TENANT, Box::new(BinaryInvariant));
        for entry in &bundle.entries {
            if log.append(entry.clone()).is_err() {
                return false;
            }
        }
        for sth in &bundle.sths {
            let size = sth.tree_size as usize;
            let root = match log.root_at(size) {
                Ok(r) => r,
                Err(_) => return false,
            };
            match sth.root_bytes() {
                Ok(r) if r == root => {}
                _ => return false,
            }
        }
    }
    for check in &bundle.inclusion {
        let sth = match bundle.sths.get(check.sth_index) {
            Some(s) => s,
            None => return false,
        };
        if !verify_inclusion_proof(&check.entry, &check.proof, sth) {
            return false;
        }
    }
    for check in &bundle.consistency {
        match (
            bundle.sths.get(check.old_index),
            bundle.sths.get(check.new_index),
        ) {
            (Some(o), Some(n)) => {
                if !verify_consistency_proof(o, n, &check.proof) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Drive the real `builder build-binaries` subcommand over a fixture
/// `records.json` written to `dir`, returning the parsed bundle it emits.
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

    // One entry per artifact leaf, one inclusion proof each.
    assert_eq!(bundle.entries.len(), records.len());
    assert_eq!(bundle.inclusion.len(), records.len());
    // Every leaf carries the binaries tenant tag.
    assert!(bundle.entries.iter().all(|e| e.tenant == TENANT));
    // A midpoint + final STH (5 leaves) plus the consistency proof between them.
    assert_eq!(bundle.sths.len(), 2);
    assert_eq!(bundle.consistency.len(), 1);
    // The binaries tenant is the one whose invariant a verifier re-checks.
    assert_eq!(bundle.enforce_unique, vec![TENANT.to_string()]);
    // The head is signed by the pinned key we handed the CLI.
    assert_eq!(
        bundle.public_key,
        hex::encode(signing_key().verifying_key().to_bytes())
    );

    assert!(
        monitor_verify_binaries(&bundle),
        "binaries bundle must verify under the binaries domain context"
    );

    // Round-trips through the on-disk JSON shape and still verifies.
    let json = serde_json::to_string_pretty(&bundle).unwrap();
    let reparsed: Bundle = serde_json::from_str(&json).unwrap();
    assert!(monitor_verify_binaries(&reparsed));
}

/// The signed STH must be byte-stable across rebuilds for a reused timestamp, so
/// an unchanged binaries tree republishes without diverging from the already
/// -published head. The timestamp is the only time-varying input.
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
    // Two leaves describing the SAME released unit (tag/platform/arch/bundle/layer)
    // but different artifact bytes — a silent re-issue the log must refuse.
    let records = vec![
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x11, 0x11),
        record("v1.3.0", "darwin", "aarch64", "dmg", Layer::Payload, 0x99, 0x99),
    ];
    let err = build_binaries_bundle(&records, &signing_key(), TS).unwrap_err();
    assert!(err.to_string().contains("fork"), "expected a fork violation, got: {err}");
}

#[test]
fn signed_without_payload_aborts_the_build() {
    // A signed leaf whose payload_sha256 was never logged as a payload leaf.
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
    // v1.0.0 reappears after v1.1.0 has begun — a jump backwards in publish order.
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

    // Flip a byte in one committed entry's leaf data: its leaf hash no longer
    // matches the STH root the inclusion proof reconstructs.
    bundle.entries[0].data[0] ^= 0xff;
    assert!(
        !monitor_verify_binaries(&bundle),
        "a tampered binary entry must fail the monitor"
    );
}
