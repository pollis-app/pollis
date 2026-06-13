//! Deterministic gate suite for the account-key builder tenant.
//!
//! Mirrors `build.rs` for the second tenant: seeds a LOCAL libSQL/SQLite fixture
//! file (no network, no prod DB) with several users/versions, builds the
//! account-key bundle, and verifies it through the slice-1 monitor path — but
//! checking STH signatures under the **account-keys** domain context, since the
//! account tree is domain-separated from the commit log. Also exercises
//! duplicate/regression rejection and tamper detection.

use ed25519_dalek::SigningKey;
use verifiable_log::{
    verify_consistency_proof, verify_inclusion_proof, verifying_key_from_hex, VerifiableLog,
};
use verifiable_log_builder::account_key::{AccountKeyInvariant, STH_CONTEXT, TENANT};
use verifiable_log_builder::builder::Bundle;
use verifiable_log_builder::{build_account_bundle, source};

const TS: u64 = 1_700_000_000_000;
const KEY: [u8; 32] = [9u8; 32];

/// A synthetic account_key_log row.
struct Row {
    seq: i64,
    user: &'static str,
    version: i64,
    pubkey: Vec<u8>,
}

fn row(seq: i64, user: &'static str, version: i64, pubkey: &[u8]) -> Row {
    Row {
        seq,
        user,
        version,
        pubkey: pubkey.to_vec(),
    }
}

/// Create a fresh local libSQL file with the real `account_key_log` shape and the
/// given rows. No UNIQUE index, so duplicate/regression rows can be injected
/// exactly as a buggy/malicious server might have written them.
async fn seed_db(path: &std::path::Path, rows: &[Row]) {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "CREATE TABLE account_key_log (\
            seq INTEGER PRIMARY KEY AUTOINCREMENT, \
            user_id TEXT NOT NULL, \
            account_id_pub BLOB NOT NULL, \
            identity_version INTEGER NOT NULL, \
            created_at TEXT NOT NULL DEFAULT (datetime('now')))",
        (),
    )
    .await
    .unwrap();
    for r in rows {
        conn.execute(
            "INSERT INTO account_key_log \
                (seq, user_id, account_id_pub, identity_version) \
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![r.seq, r.user.to_string(), r.pubkey.clone(), r.version],
        )
        .await
        .unwrap();
    }
}

/// Faithful in-process re-implementation of `monitor verify` for the account
/// tree: built ONLY on the public slice-1 verifiers, but with STH signatures
/// checked under [`STH_CONTEXT`] (the account tree's domain separation) and the
/// account invariant enforced on replay.
fn monitor_verify_account(bundle: &Bundle) -> bool {
    let vk = match verifying_key_from_hex(&bundle.public_key) {
        Ok(k) => k,
        Err(_) => return false,
    };

    // STHs must verify under the ACCOUNT context — and must NOT verify under the
    // default commit-log context (proving the domain separation holds).
    for sth in &bundle.sths {
        if !sth.verify_with_context(&vk, STH_CONTEXT) {
            return false;
        }
        if sth.verify(&vk) {
            return false;
        }
    }

    if !bundle.entries.is_empty() {
        let mut log = VerifiableLog::new();
        log.register_invariant(TENANT, Box::new(AccountKeyInvariant));
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

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&KEY)
}

#[tokio::test]
async fn valid_account_bundle_verifies_under_account_context() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("accounts.db");
    // Two users, each rotating keys, interleaved in seq order.
    let rows = vec![
        row(1, "u-alice", 1, &[0xa1; 32]),
        row(2, "u-bob", 1, &[0xb1; 32]),
        row(3, "u-alice", 2, &[0xa2; 32]),
        row(4, "u-bob", 2, &[0xb2; 32]),
        row(5, "u-alice", 3, &[0xa3; 32]),
    ];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_account_key_log(&conn).await.unwrap();
    assert_eq!(read.len(), 5);
    // The public key is read out verbatim (hex), never hashed.
    assert_eq!(read[0].account_id_pub, hex::encode([0xa1u8; 32]));

    let bundle = build_account_bundle(&read, &signing_key(), TS).unwrap();

    assert_eq!(bundle.entries.len(), 5);
    assert_eq!(bundle.inclusion.len(), 5);
    assert_eq!(bundle.sths.len(), 2);
    assert_eq!(bundle.consistency.len(), 1);
    assert_eq!(bundle.enforce_unique, vec!["account-key".to_string()]);

    assert!(monitor_verify_account(&bundle), "account bundle must verify");

    // Round-trips through the on-disk JSON shape and still verifies.
    let json = serde_json::to_string_pretty(&bundle).unwrap();
    let reparsed: Bundle = serde_json::from_str(&json).unwrap();
    assert!(monitor_verify_account(&reparsed));
}

#[tokio::test]
async fn duplicate_version_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("dup.db");
    // Same (user_id, identity_version) twice with different key bytes.
    let rows = vec![
        row(1, "u-alice", 1, &[0xa1; 32]),
        row(2, "u-alice", 1, &[0xfe; 32]),
    ];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_account_key_log(&conn).await.unwrap();

    let err = build_account_bundle(&read, &signing_key(), TS).unwrap_err();
    assert!(
        err.to_string().contains("duplicate"),
        "expected a duplicate-version violation, got: {err}"
    );
}

#[tokio::test]
async fn version_regression_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("regress.db");
    // u-alice goes 1 -> 5 -> 3 (backwards) in seq order.
    let rows = vec![
        row(1, "u-alice", 1, &[0xa1; 32]),
        row(2, "u-alice", 5, &[0xa5; 32]),
        row(3, "u-alice", 3, &[0xa3; 32]),
    ];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_account_key_log(&conn).await.unwrap();

    let err = build_account_bundle(&read, &signing_key(), TS).unwrap_err();
    assert!(
        err.to_string().contains("regression"),
        "expected a version regression violation, got: {err}"
    );
}

#[tokio::test]
async fn tampered_account_entry_fails_verification() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("tamper.db");
    let rows = vec![
        row(1, "u-alice", 1, &[0xa1; 32]),
        row(2, "u-alice", 2, &[0xa2; 32]),
        row(3, "u-alice", 3, &[0xa3; 32]),
    ];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_account_key_log(&conn).await.unwrap();
    let mut bundle = build_account_bundle(&read, &signing_key(), TS).unwrap();
    assert!(monitor_verify_account(&bundle));

    // Flip a byte in one committed entry's leaf data: its leaf hash no longer
    // matches the STH root the inclusion proof reconstructs.
    bundle.entries[0].data[0] ^= 0xff;
    assert!(
        !monitor_verify_account(&bundle),
        "a tampered account entry must fail the monitor"
    );
}
