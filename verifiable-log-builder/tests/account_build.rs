//! Requires the `db-source` feature (on by default): these fixtures seed a local
//! libsql database and drive `source`'s readers, which are the part of this crate
//! #987 gated so a verification-only consumer does not link `libsql`.
#![cfg(feature = "db-source")]

//! Deterministic gate suite for the account-key builder tenant.
//!
//! Mirrors `build.rs` for the second tenant: seeds a LOCAL libSQL/SQLite fixture
//! file (no network, no prod DB) with several users/versions, builds the
//! account-key bundle, and verifies it through the slice-1 monitor path — but
//! checking STH signatures under the **account-keys** domain context, since the
//! account tree is domain-separated from the commit log. Also exercises
//! duplicate/regression rejection and tamper detection.

use verifiable_log::monitor::{verify_bundle, VerifyOptions};
use verifiable_log::SigningKey;
use verifiable_log::{TenantInvariant, STH_CONTEXT_BINARIES, STH_CONTEXT_COMMIT_LOG};
use verifiable_log_builder::account_key::{AccountKeyInvariant, STH_CONTEXT, TENANT};
use verifiable_log_builder::builder::Bundle;
use verifiable_log_builder::{build_account_bundle, build_bundle, source};

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

/// Run the **real** monitor pass (the one the `monitor` CLI runs) over a bundle,
/// under `context` and with the account tenant's own invariant enforced on
/// replay.
///
/// This used to be a hand-written transcription of the monitor's loop, because
/// the CLI could only ever check the commit-log context — so a test that wanted
/// to verify an account bundle had to rebuild the verifier to do it. The
/// re-implementation then froze that limitation as expected behaviour. There is
/// one verifier now, and the tree it checks is an argument (#875).
fn verify_account(bundle: &Bundle, context: &[u8]) -> bool {
    let invariant: Box<dyn TenantInvariant> = Box::new(AccountKeyInvariant);
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

/// The account tree's own context — how an auditor checks this bundle.
fn monitor_verify_account(bundle: &Bundle) -> bool {
    verify_account(bundle, STH_CONTEXT)
}

fn signing_key() -> SigningKey {
    SigningKey::from_seed(&KEY.into())
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

    // Domain separation, asserted where it can be read rather than buried inside
    // the verifier: the SAME bundle checked as another tree must fail. That is
    // what stops an account-key head being presented as a commit-log head, and
    // it is exactly why the monitor has to be told which tree it is checking.
    assert!(
        !verify_account(&bundle, STH_CONTEXT_COMMIT_LOG),
        "an account-key head must NOT verify as a commit-log head"
    );
    assert!(
        !verify_account(&bundle, STH_CONTEXT_BINARIES),
        "an account-key head must NOT verify as a binaries head"
    );

    // Round-trips through the on-disk JSON shape and still verifies.
    let json = serde_json::to_string_pretty(&bundle).unwrap();
    let reparsed: Bundle = serde_json::from_str(&json).unwrap();
    assert!(monitor_verify_account(&reparsed));
}

/// The STH for a given (size, root) must be byte-stable across rebuilds so an
/// unchanged tree can be republished to immutable storage without diverging from
/// the already-published per-size head. The timestamp is the only time-varying
/// input, so reusing the frozen timestamp must reproduce the bundle exactly,
/// and a different timestamp must change it (proving the timestamp is what the
/// publish workflow reuses to keep the head stable).
#[tokio::test]
async fn rebuild_is_byte_identical_for_a_reused_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("stable.db");
    let rows = vec![
        row(1, "u-alice", 1, &[0xa1; 32]),
        row(2, "u-bob", 1, &[0xb1; 32]),
        row(3, "u-alice", 2, &[0xa2; 32]),
    ];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_account_key_log(&conn).await.unwrap();

    let first = serde_json::to_string(&build_account_bundle(&read, &signing_key(), TS).unwrap())
        .unwrap();
    let again = serde_json::to_string(&build_account_bundle(&read, &signing_key(), TS).unwrap())
        .unwrap();
    assert_eq!(
        first, again,
        "same rows + same timestamp must reproduce the bundle byte-for-byte"
    );

    let moved = serde_json::to_string(
        &build_account_bundle(&read, &signing_key(), TS + 1).unwrap(),
    )
    .unwrap();
    assert_ne!(
        first, moved,
        "a different timestamp must change the signed STH bytes"
    );
}

/// The two tenants are independent: an empty commit log must not block the
/// account-key bundle (and vice versa). This mirrors the builder's decoupled
/// `run_build` — an empty commit source surfaces `EmptyCommitLog` and still
/// yields a valid (empty) commit bundle, while the non-empty account log builds
/// and verifies entirely on its own.
#[tokio::test]
async fn account_bundle_builds_when_commit_log_is_empty() {
    // Commit log empty (e.g. right after Goal A's cutover).
    let empty_commits: Vec<source::CommitRow> = Vec::new();
    assert!(
        matches!(
            source::ensure_non_empty(&empty_commits).unwrap_err(),
            verifiable_log_builder::BuilderError::EmptyCommitLog
        ),
        "an empty commit log must surface EmptyCommitLog"
    );
    // It still produces a valid bundle rather than aborting the whole run.
    build_bundle(&empty_commits, &signing_key(), TS)
        .expect("empty commit log must still build a valid bundle");

    // The account-key log is non-empty and its bundle builds + verifies
    // independently of the commit log's (empty) state.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("accounts.db");
    let rows = vec![
        row(1, "u-alice", 1, &[0xa1; 32]),
        row(2, "u-bob", 1, &[0xb1; 32]),
    ];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_account_key_log(&conn).await.unwrap();
    let bundle = build_account_bundle(&read, &signing_key(), TS).unwrap();
    assert_eq!(bundle.entries.len(), 2);
    assert!(
        monitor_verify_account(&bundle),
        "account bundle must verify even though the commit log is empty"
    );
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
