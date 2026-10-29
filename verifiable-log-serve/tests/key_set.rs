//! End-to-end gate for root-then-signers trust (#754, wired at runtime by #1454).
//!
//! An account tree signed by a key that is NOT compiled in is served over HTTP
//! next to a `/v1/key-set.json`. The verifier must accept that tree only when a
//! key set signed by a pinned root names its signer for the account tree, and
//! must refuse it (or alarm) otherwise.
//!
//! Trees are written after the server spawns, so only the key set can make the verifier trust them.

use std::path::Path;

use ml_dsa::Keypair as _;
use verifiable_log::{key_id_for, KeySetStatement, SignerEntry, SigningKey, STH_CONTEXT_ACCOUNT_KEYS};
use verifiable_log_builder::build_account_bundle;
use verifiable_log_builder::source::AccountKeyRow;
use verifiable_log_serve::bundle::Bundle;
use verifiable_log_serve::{account, layout, DevServer, KeySetTrust};

const TS: u64 = 1_700_000_000_000;

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32].into())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn rows() -> Vec<AccountKeyRow> {
    vec![
        AccountKeyRow {
            seq: 1,
            user_id: "u-alice".to_string(),
            identity_version: 1,
            account_id_pub: hex::encode([0xa1u8; 32]),
        },
        AccountKeyRow {
            seq: 2,
            user_id: "u-alice".to_string(),
            identity_version: 2,
            account_id_pub: hex::encode([0xa2u8; 32]),
        },
    ]
}

/// Write the account tree, signed by `signer`, into `root`.
fn write_account_tree(root: &Path, signer: &SigningKey) {
    let b = build_account_bundle(&rows(), signer, TS).unwrap();
    let bundle: Bundle = serde_json::from_slice(&serde_json::to_vec(&b).unwrap()).unwrap();
    layout::generate_account(&bundle, root).unwrap();
}

/// Write `/v1/key-set.json`: `root_key` authorising `signer` for `trees`.
fn write_key_set(root: &Path, root_key: &SigningKey, signer: &SigningKey, trees: &[&[u8]]) -> String {
    let vk = signer.verifying_key();
    let entry = SignerEntry {
        key_id: key_id_for(&vk),
        public_key: hex::encode(vk.encode()),
        trees: trees.iter().map(|t| String::from_utf8_lossy(t).to_string()).collect(),
        not_after_ms: None,
    };
    let st = KeySetStatement::create(root_key, now_ms(), None, vec![entry]).unwrap();
    let doc = serde_json::to_string(&st).unwrap();
    std::fs::create_dir_all(root.join("v1")).unwrap();
    std::fs::write(root.join("v1").join("key-set.json"), &doc).unwrap();
    doc
}

fn served_key_set(base: &str) -> Option<String> {
    ureq::get(&format!("{base}/v1/key-set.json")).call().ok()?.into_string().ok()
}

/// Resolve the served key set against a test root, as the client does against
/// the compiled one.
fn trust_under(base: &str, pinned_root: &SigningKey) -> KeySetTrust {
    let hex_root = hex::encode(pinned_root.verifying_key().encode());
    KeySetTrust::from_document(
        served_key_set(base).as_deref(),
        [(hex_root.as_str(), None)],
        STH_CONTEXT_ACCOUNT_KEYS,
        now_ms(),
    )
}

#[test]
fn a_log_signed_by_a_root_authorised_key_verifies_and_one_outside_the_set_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let base = server.base_url();
    let root = sk(1);
    let rotated = sk(40);
    write_account_tree(dir.path(), &rotated);
    write_key_set(dir.path(), &root, &rotated, &[STH_CONTEXT_ACCOUNT_KEYS]);

    let t = trust_under(&base, &root);
    assert_eq!(t.alarm, None);
    let report = account::verify_account_via_trusting(&base, "u-alice", None, &t).unwrap();
    assert!(report.chain_valid, "violations: {:?}", report.violations);
    assert_eq!(report.keys.len(), 2);

    assert!(account::verify_account_via_trusting(&base, "u-alice", None, &KeySetTrust::default()).is_err());

    // The production entry point uses the compiled root, which did not sign this set.
    assert!(account::verify_account_via(&base, "u-alice", None).is_err());

    server.shutdown();
}

#[test]
fn a_key_set_from_an_unpinned_root_does_not_authorise_its_signer() {
    let dir = tempfile::tempdir().unwrap();
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let base = server.base_url();
    let attacker = sk(41);
    write_account_tree(dir.path(), &attacker);
    write_key_set(dir.path(), &sk(66), &attacker, &[STH_CONTEXT_ACCOUNT_KEYS]);

    let t = trust_under(&base, &sk(1));
    assert!(t.signers.is_empty());
    assert!(t.alarm.is_some(), "a forged key set must alarm");
    assert!(account::verify_account_via_trusting(&base, "u-alice", None, &t).is_err());

    server.shutdown();
}

#[test]
fn a_signer_the_set_authorises_only_for_another_tree_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let base = server.base_url();
    let root = sk(1);
    let signer = sk(42);
    write_account_tree(dir.path(), &signer);
    write_key_set(dir.path(), &root, &signer, &[verifiable_log::STH_CONTEXT_BINARIES]);

    let t = trust_under(&base, &root);
    assert!(t.signers.is_empty());
    assert!(account::verify_account_via_trusting(&base, "u-alice", None, &t).is_err());

    server.shutdown();
}

/// A log the verifier already trusts keeps verifying when no key set is
/// published (404), and a served key set that does not verify under the
/// compiled root turns the verdict into a failure rather than being ignored.
#[test]
fn compiled_trust_survives_a_missing_key_set_and_an_unverifiable_one_fails_the_verdict() {
    let dir = tempfile::tempdir().unwrap();
    // Written before spawn, so the dev allowance trusts this key.
    write_account_tree(dir.path(), &sk(50));
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let base = server.base_url();

    let ok = account::verify_account_via(&base, "u-alice", None).unwrap();
    assert!(ok.chain_valid, "no key set must mean compiled-pin behaviour: {:?}", ok.violations);

    write_key_set(dir.path(), &sk(66), &sk(50), &[STH_CONTEXT_ACCOUNT_KEYS]);
    let alarmed = account::verify_account_via(&base, "u-alice", None).unwrap();
    assert!(!alarmed.chain_valid);
    assert!(
        alarmed.violations[0].contains("key set"),
        "the key-set alarm leads the violations: {:?}",
        alarmed.violations
    );

    server.shutdown();
}
