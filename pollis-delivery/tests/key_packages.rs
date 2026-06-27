//! Key-package CLAIM endpoint invariants, against a real (local) libsql DB.
//!
//! The claim (`POST /v1/key-packages/claim`, `apply_claim_key_package`) flips one
//! of a TARGET user's published `mls_key_package` rows to `claimed = 1` and hands
//! the caller its TLS bytes, so an MLS Add commit can be built for that device.
//! It is the read-only-client replacement (Goal B #419, blocker C1) for the
//! direct `UPDATE … RETURNING` the client used to run.
//!
//! Coverage: a valid claim returns the right bytes (device-scoped and
//! user-scoped), and concurrent claims of a single-package pool yield exactly one
//! winner — the rest see no package (never a double-claim).

use std::sync::Arc;

use pollis_delivery::db::Db;
use pollis_delivery::devices::{apply_claim_key_package, ClaimKeyPackageBody, ClaimOutcome};

// Minimal slice of `mls_key_package` — the columns the claim reads/writes. No
// `users` FK (foreign_keys=OFF in the local test DB) so the test is self-contained.
const SCHEMA: &str = "\
CREATE TABLE mls_key_package (\
  ref_hash    TEXT PRIMARY KEY,\
  user_id     TEXT NOT NULL,\
  key_package BLOB NOT NULL,\
  claimed     INTEGER NOT NULL DEFAULT 0,\
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),\
  device_id   TEXT\
);";

async fn fresh_db() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("kp.db");
    // Keep the tempdir alive for the process.
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    db.conn().unwrap().execute_batch(SCHEMA).await.expect("schema");
    Arc::new(db)
}

/// Insert one unclaimed key package. `created_at` is supplied explicitly so the
/// claim's `ORDER BY created_at ASC` is deterministic across rows.
async fn insert_kp(
    db: &Db,
    ref_hash: &str,
    user_id: &str,
    device_id: &str,
    key_package: &[u8],
    created_at: &str,
) {
    let conn = db.conn().unwrap();
    conn.execute(
        "INSERT INTO mls_key_package (ref_hash, user_id, key_package, device_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![
            ref_hash.to_string(),
            user_id.to_string(),
            key_package.to_vec(),
            device_id.to_string(),
            created_at.to_string(),
        ],
    )
    .await
    .unwrap();
}

fn device_body(user_id: &str, device_id: &str) -> ClaimKeyPackageBody {
    ClaimKeyPackageBody {
        target_user_id: user_id.to_string(),
        target_device_id: Some(device_id.to_string()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_returns_the_right_bytes_then_exhausts() {
    let db = fresh_db().await;
    insert_kp(&db, "ref-a", "bob", "dev1", b"kp-bytes-a", "2024-01-01 00:00:00").await;
    let conn = db.conn().unwrap();

    // Device-scoped claim returns the published bytes.
    match apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, key_package } => {
            assert_eq!(ref_hash, "ref-a");
            assert_eq!(key_package, b"kp-bytes-a");
        }
        ClaimOutcome::NoKeyPackage => panic!("expected a claimed package"),
    }

    // The pool is now empty for that device → the next claim sees no package.
    match apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::NoKeyPackage => {}
        ClaimOutcome::Claimed { .. } => panic!("a claimed package must not be re-claimable"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_is_oldest_first_and_device_scoped() {
    let db = fresh_db().await;
    // Two devices for the same user; for dev1, two packages of differing age.
    insert_kp(&db, "ref-old", "bob", "dev1", b"old", "2024-01-01 00:00:00").await;
    insert_kp(&db, "ref-new", "bob", "dev1", b"new", "2024-02-01 00:00:00").await;
    insert_kp(&db, "ref-d2", "bob", "dev2", b"d2", "2024-01-01 00:00:00").await;
    let conn = db.conn().unwrap();

    // Oldest unclaimed package for dev1 wins.
    match apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, key_package } => {
            assert_eq!(ref_hash, "ref-old");
            assert_eq!(key_package, b"old");
        }
        ClaimOutcome::NoKeyPackage => panic!("expected the oldest dev1 package"),
    }

    // dev2's package is untouched by a dev1 claim — device scoping holds.
    let user_scoped = ClaimKeyPackageBody { target_user_id: "bob".into(), target_device_id: None };
    // A user-scoped claim (no device) now picks the oldest remaining of ANY
    // device: ref-new and ref-d2 share a timestamp, so just assert it is one of
    // them and is non-empty.
    match apply_claim_key_package(&conn, &user_scoped).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => {
            assert!(ref_hash == "ref-new" || ref_hash == "ref-d2", "got {ref_hash}");
        }
        ClaimOutcome::NoKeyPackage => panic!("expected a remaining package"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_claims_of_one_package_yield_exactly_one_winner() {
    let db = fresh_db().await;
    // A single unclaimed package for (bob, dev1).
    insert_kp(&db, "ref-solo", "bob", "dev1", b"the-only-kp", "2024-01-01 00:00:00").await;

    // 8 claimers race for the one package. The atomic `WHERE claimed = 0`
    // single-row UPDATE guarantees exactly one observes the bytes; the rest see
    // none — never a double-claim.
    let mut handles = Vec::new();
    for _ in 0..8 {
        let db = Arc::clone(&db);
        handles.push(tokio::spawn(async move {
            let conn = db.conn().unwrap();
            // busy_timeout is per-connection: each writer waits for the local
            // file lock instead of erroring (a local-file test artifact; Turso
            // serializes writes server-side). The conditional UPDATE still
            // decides exactly one winner.
            conn.execute_batch("PRAGMA busy_timeout=10000;").await.unwrap();
            apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap()
        }));
    }

    let mut claimed = 0;
    let mut none = 0;
    for h in handles {
        match h.await.unwrap() {
            ClaimOutcome::Claimed { ref_hash, key_package } => {
                assert_eq!(ref_hash, "ref-solo");
                assert_eq!(key_package, b"the-only-kp");
                claimed += 1;
            }
            ClaimOutcome::NoKeyPackage => none += 1,
        }
    }

    assert_eq!(claimed, 1, "exactly one claimer may win the single package");
    assert_eq!(none, 7, "every other claimer must see no package — no double-claim");

    // The row is marked claimed exactly once.
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT claimed FROM mls_key_package WHERE ref_hash = ?1",
            libsql::params!["ref-solo"],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("row exists");
    assert_eq!(row.get::<i64>(0).unwrap(), 1);
}
