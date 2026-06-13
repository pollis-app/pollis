//! End-to-end gate for the **account-key** tree alongside the commit-log tree:
//! (a) the builder produces BOTH bundles (commit log + account keys), the layout
//!     generator writes both static trees into one root, and the dev server
//!     serves them;
//! (b) `verify_remote` verifies BOTH trees over HTTP (the account STHs under the
//!     account domain context) and `verify_account` checks one user's chain;
//! (c) tampering with an account-keys artifact fails the account tree while the
//!     commit-log tree still passes — the two are independent;
//! (d) when the account tree is absent, `verify_remote` warns and still passes
//!     the commit log.
//!
//! The fixtures are built with the REAL builder (`build_bundle` /
//! `build_account_bundle`) so the entries are genuine `CommitLeaf` /
//! `AccountKeyLeaf` payloads and the account STHs are signed under the account
//! context — exactly what production emits. Nothing here reimplements any
//! verification.

use std::path::Path;

use ed25519_dalek::SigningKey;
use verifiable_log_builder::source::{AccountKeyRow, CommitRow};
use verifiable_log_builder::{build_account_bundle, build_bundle};
use verifiable_log_serve::bundle::Bundle;
use verifiable_log_serve::{account, group, layout, remote, DevServer};

const TS: u64 = 1_700_000_000_000;

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

/// Two conversations, a few commits each, in seq order.
fn commit_rows() -> Vec<CommitRow> {
    vec![
        CommitRow {
            seq: 1,
            conversation_id: "conv-1".to_string(),
            epoch: 0,
            sender_id: "u-alice".to_string(),
            commit_sha256: hex::encode([0x11u8; 32]),
        },
        CommitRow {
            seq: 2,
            conversation_id: "conv-1".to_string(),
            epoch: 1,
            sender_id: "u-bob".to_string(),
            commit_sha256: hex::encode([0x12u8; 32]),
        },
        CommitRow {
            seq: 3,
            conversation_id: "conv-2".to_string(),
            epoch: 0,
            sender_id: "u-bob".to_string(),
            commit_sha256: hex::encode([0x21u8; 32]),
        },
        CommitRow {
            seq: 4,
            conversation_id: "conv-1".to_string(),
            epoch: 2,
            sender_id: "u-alice".to_string(),
            commit_sha256: hex::encode([0x13u8; 32]),
        },
    ]
}

/// Two users, each rotating identity keys, interleaved in seq order.
fn account_rows() -> Vec<AccountKeyRow> {
    vec![
        AccountKeyRow {
            seq: 1,
            user_id: "u-alice".to_string(),
            identity_version: 1,
            account_id_pub: hex::encode([0xa1u8; 32]),
        },
        AccountKeyRow {
            seq: 2,
            user_id: "u-bob".to_string(),
            identity_version: 1,
            account_id_pub: hex::encode([0xb1u8; 32]),
        },
        AccountKeyRow {
            seq: 3,
            user_id: "u-alice".to_string(),
            identity_version: 2,
            account_id_pub: hex::encode([0xa2u8; 32]),
        },
        AccountKeyRow {
            seq: 4,
            user_id: "u-alice".to_string(),
            identity_version: 3,
            account_id_pub: hex::encode([0xa3u8; 32]),
        },
    ]
}

/// Round-trip a builder bundle into the serve crate's identical wire shape.
fn to_serve_bundle(b: &verifiable_log_builder::Bundle) -> Bundle {
    serde_json::from_slice(&serde_json::to_vec(b).unwrap()).unwrap()
}

/// Generate the commit-log tree into `root`. Returns the conversation id used.
fn generate_commit_tree(root: &Path) {
    let builder_bundle = build_bundle(&commit_rows(), &signing_key(), TS).unwrap();
    let bundle = to_serve_bundle(&builder_bundle);
    layout::generate(&bundle, root).unwrap();
}

/// Generate the account-key tree into `root` (alongside the commit-log tree).
fn generate_account_tree(root: &Path) {
    let builder_bundle = build_account_bundle(&account_rows(), &signing_key(), TS).unwrap();
    let bundle = to_serve_bundle(&builder_bundle);
    layout::generate_account(&bundle, root).unwrap();
}

#[test]
fn both_trees_generate_documented_files() {
    let dir = tempfile::tempdir().unwrap();
    generate_commit_tree(dir.path());
    generate_account_tree(dir.path());

    let v1 = dir.path().join("v1");
    let acct = v1.join("account-keys");

    // The commit-log tree is untouched and complete.
    assert!(v1.join("public_key.json").is_file());
    assert!(v1.join("index.json").is_file());
    assert!(v1.join("sth").join("latest.json").is_file());
    assert!(v1.join("entries.json").is_file());

    // The account-key subtree mirrors it one level down.
    assert!(acct.join("public_key.json").is_file());
    assert!(acct.join("index.json").is_file());
    assert!(acct.join("sth").join("latest.json").is_file());
    assert!(acct.join("entries.json").is_file());

    // Precomputed per-user reports exist with no extension (file IS the URL).
    assert!(dir.path().join("verify").join("account").join("u-alice").is_file());
    assert!(dir.path().join("verify").join("account").join("u-bob").is_file());

    // The account manifest advertises both users.
    let manifest: verifiable_log_serve::AccountManifest = serde_json::from_str(
        &std::fs::read_to_string(acct.join("index.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.entry_count, account_rows().len() as u64);
    assert_eq!(manifest.users, vec!["u-alice".to_string(), "u-bob".to_string()]);
    assert_eq!(manifest.enforce_unique, vec!["account-key".to_string()]);
}

#[test]
fn remote_verifies_both_trees_and_account_chain() {
    let dir = tempfile::tempdir().unwrap();
    generate_commit_tree(dir.path());
    generate_account_tree(dir.path());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    // Whole-log remote verification covers BOTH trees and passes.
    let report = remote::verify_remote(&server.base_url()).unwrap();
    assert!(report.ok, "both trees should verify; checks: {:?}", report.checks);
    // The account-keys tree actually contributed checks (not just the commit log).
    let account_checks = report
        .checks
        .iter()
        .filter(|(_, label)| label.starts_with("account-keys:"))
        .count();
    assert!(account_checks >= 4, "expected account-keys checks, got {account_checks}");
    // No "absent" note when the tree is present.
    assert!(report.notes.is_empty(), "unexpected notes: {:?}", report.notes);

    // One user's full key history verifies.
    let alice = account::verify_account(&server.base_url(), "u-alice").unwrap();
    assert!(alice.found);
    assert!(alice.chain_valid, "violations: {:?}", alice.violations);
    // Alice rotated to version 3 — three versions, strictly increasing.
    assert_eq!(alice.keys.len(), 3);
    assert_eq!(
        alice.keys.iter().map(|k| k.identity_version).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(alice.keys.iter().all(|k| k.included));

    server.shutdown();
}

#[test]
fn tampered_account_tree_fails_while_commit_log_still_passes() {
    let dir = tempfile::tempdir().unwrap();
    generate_commit_tree(dir.path());
    generate_account_tree(dir.path());

    // Corrupt the account tree's trust anchor: flip a hex nibble in the latest
    // account STH signature. Everything downstream is anchored to this head, so
    // the whole account tree is now untrustworthy — while the commit-log tree,
    // signed independently, is unaffected.
    let latest_path = dir
        .path()
        .join("v1")
        .join("account-keys")
        .join("sth")
        .join("latest.json");
    let mut sth: verifiable_log::Sth =
        serde_json::from_str(&std::fs::read_to_string(&latest_path).unwrap()).unwrap();
    let mut sig: Vec<char> = sth.signature.chars().collect();
    sig[0] = if sig[0] == 'a' { 'b' } else { 'a' };
    sth.signature = sig.into_iter().collect();
    std::fs::write(&latest_path, serde_json::to_string_pretty(&sth).unwrap()).unwrap();

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();

    // The overall verdict fails because the account tree is broken...
    let report = remote::verify_remote(&server.base_url()).unwrap();
    assert!(!report.ok, "tampered account tree must fail overall verification");

    // ...but every NON-account check (i.e. the whole commit-log tree) still
    // passes — the trees are independent.
    let commit_failures: Vec<&String> = report
        .checks
        .iter()
        .filter(|(passed, label)| !*passed && !label.starts_with("account-keys:"))
        .map(|(_, label)| label)
        .collect();
    assert!(
        commit_failures.is_empty(),
        "commit-log checks must still pass; failures: {commit_failures:?}"
    );

    // The account chain itself is now invalid...
    let alice = account::verify_account(&server.base_url(), "u-alice").unwrap();
    assert!(!alice.chain_valid, "tampered account chain must be invalid");

    // ...while the commit-log group chain is still valid.
    let conv = group::verify_group(&server.base_url(), "conv-1").unwrap();
    assert!(conv.chain_valid, "violations: {:?}", conv.violations);

    server.shutdown();
}

#[test]
fn absent_account_tree_warns_and_commit_log_still_passes() {
    let dir = tempfile::tempdir().unwrap();
    // Only the commit-log tree — no account tree at all.
    generate_commit_tree(dir.path());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();

    // The commit log verifies fine; the absent account tree is a warning, not a
    // failure.
    assert!(report.ok, "commit log should verify; checks: {:?}", report.checks);
    assert!(
        report.notes.iter().any(|n| n.contains("absent")),
        "expected an 'absent' note; notes: {:?}",
        report.notes
    );
    // No account-keys checks ran (only a note).
    assert!(report
        .checks
        .iter()
        .all(|(_, label)| !label.starts_with("account-keys:")));

    server.shutdown();
}
