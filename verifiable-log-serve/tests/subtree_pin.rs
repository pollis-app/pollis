//! The whole-log audit pins the subtree keys too (H-1, #1540): the account-keys
//! and binaries trees are signed by the same log key as the commit log, so a
//! server must not be able to anchor a subtree in a key of its own choosing.
//!
//! Its own test binary on purpose: `DevServer` trusts the keys of the log it
//! serves process-wide, and the attacker key here must never be among them.

use std::path::Path;

use verifiable_log::SigningKey;
use verifiable_log_builder::binaries::{BinaryRecord, Layer, Toolchain};
use verifiable_log_builder::source::{AccountKeyRow, CommitRow};
use verifiable_log_builder::{build_account_bundle, build_binaries_bundle, build_bundle};
use verifiable_log_serve::bundle::Bundle;
use verifiable_log_serve::{layout, remote, DevServer};

const TS: u64 = 1_700_000_000_000;

fn log_key() -> SigningKey {
    SigningKey::from_seed(&[21u8; 32].into())
}

fn attacker_key() -> SigningKey {
    SigningKey::from_seed(&[22u8; 32].into())
}

fn to_serve_bundle(b: &verifiable_log_builder::Bundle) -> Bundle {
    serde_json::from_slice(&serde_json::to_vec(b).unwrap()).unwrap()
}

fn commit_rows() -> Vec<CommitRow> {
    vec![CommitRow {
        seq: 1,
        conversation_id: "conv-1".to_string(),
        generation: 0,
        epoch: 0,
        sender_id: "u-alice".to_string(),
        commit_sha256: hex::encode([0x11u8; 32]),
    }]
}

fn account_rows() -> Vec<AccountKeyRow> {
    vec![AccountKeyRow {
        seq: 1,
        user_id: "u-alice".to_string(),
        identity_version: 1,
        account_id_pub: hex::encode([0xa1u8; 32]),
    }]
}

fn binary_records() -> Vec<BinaryRecord> {
    vec![BinaryRecord {
        release_tag: "v1.3.0".to_string(),
        commit: "f".repeat(40),
        platform: "linux".to_string(),
        arch: "x86_64".to_string(),
        bundle: "appimage".to_string(),
        artifact_name: "pollis-v1.3.0-linux.appimage".to_string(),
        layer: Layer::Payload,
        payload_sha256: hex::encode([0x22u8; 32]),
        artifact_sha256: hex::encode([0x22u8; 32]),
        toolchain: Toolchain {
            rustc: "1.83.0".to_string(),
            node: "20.11.1".to_string(),
            pnpm: "9.1.0".to_string(),
            runner_image: "ubuntu-24.04@sha256:abc".to_string(),
            source_date_epoch: 1_700_000_000,
        },
        provenance_uri: "cdn.pollis.com/releases/v1.3.0/linux.intoto.jsonl".to_string(),
    }]
}

/// Commit-log and account trees signed by the log key; the binaries tree signed
/// by `binaries_signer`, which its own `public_key.json` then names.
fn generate(root: &Path, binaries_signer: &SigningKey) {
    layout::generate(&to_serve_bundle(&build_bundle(&commit_rows(), &log_key(), TS).unwrap()), root)
        .unwrap();
    layout::generate_account(
        &to_serve_bundle(&build_account_bundle(&account_rows(), &log_key(), TS).unwrap()),
        root,
    )
    .unwrap();
    layout::generate_binaries(
        &to_serve_bundle(&build_binaries_bundle(&binary_records(), binaries_signer, TS).unwrap()),
        root,
    )
    .unwrap();
}

#[test]
fn a_subtree_anchored_in_an_untrusted_key_fails_the_whole_log_audit() {
    let honest = tempfile::tempdir().unwrap();
    generate(honest.path(), &log_key());
    let server = DevServer::spawn(honest.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();
    server.shutdown();
    assert!(report.ok, "control: an honest log verifies; checks: {:?}", report.checks);

    let forged = tempfile::tempdir().unwrap();
    generate(forged.path(), &attacker_key());
    let server = DevServer::spawn(forged.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();
    server.shutdown();
    assert!(
        !report.ok,
        "a binaries tree self-signed by a key the verifier does not trust must fail the audit"
    );
}
