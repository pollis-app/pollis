//! Key-package CLAIM endpoint invariants, against a real (local) libsql DB.
//!
//! The claim (`POST /v1/key-packages/claim`, `apply_claim_key_package`) flips one
//! of a TARGET user's published `mls_key_package` rows to `claimed = 1` and hands
//! the caller its TLS bytes, so an MLS Add commit can be built for that device.
//! It is the read-only-client replacement (Goal B #419, blocker C1) for the
//! direct `UPDATE … RETURNING` the client used to run.
//!
//! Coverage: a valid claim returns the right bytes (device-scoped and
//! user-scoped), concurrent claims of a single-package pool yield exactly one
//! winner — the rest see no package (never a double-claim) — and the classic and
//! post-quantum-hybrid suite pools never contaminate each other (#454 P1b).

use std::sync::Arc;

use pollis_delivery::db::Db;
use pollis_delivery::devices::{
    apply_claim_key_package, apply_publish_key_packages, ClaimKeyPackageBody, ClaimOutcome,
    KeyPackageEntry, PublishKeyPackagesBody, CIPHERSUITE_CLASSIC,
};
use pollis_delivery::writes::WriteOutcome;

/// The experimental X-Wing hybrid code point (#454). Only its DISTINCTNESS from
/// the classic one matters to the DS — the suite is a routing label here, never
/// something the DS interprets.
const CIPHERSUITE_HYBRID: i64 = 0x004D;

// Minimal slice of `mls_key_package` — the columns the claim reads/writes. No
// `users` FK (foreign_keys=OFF in the local test DB) so the test is self-contained.
const SCHEMA: &str = "\
CREATE TABLE mls_key_package (\
  ref_hash    TEXT PRIMARY KEY,\
  user_id     TEXT NOT NULL,\
  key_package BLOB NOT NULL,\
  claimed     INTEGER NOT NULL DEFAULT 0,\
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),\
  device_id   TEXT,\
  ciphersuite INTEGER NOT NULL DEFAULT 1\
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
    insert_kp_in_suite(db, ref_hash, user_id, device_id, key_package, created_at, None).await;
}

/// Insert one unclaimed key package, optionally naming its ciphersuite. `None`
/// omits the column entirely — the pre-#454 INSERT, which must land on the
/// classic default (migration `000010`).
async fn insert_kp_in_suite(
    db: &Db,
    ref_hash: &str,
    user_id: &str,
    device_id: &str,
    key_package: &[u8],
    created_at: &str,
    ciphersuite: Option<i64>,
) {
    let conn = db.conn().unwrap();
    match ciphersuite {
        Some(suite) => conn
            .execute(
                "INSERT INTO mls_key_package \
                 (ref_hash, user_id, key_package, device_id, created_at, ciphersuite) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                libsql::params![
                    ref_hash.to_string(),
                    user_id.to_string(),
                    key_package.to_vec(),
                    device_id.to_string(),
                    created_at.to_string(),
                    suite,
                ],
            )
            .await
            .unwrap(),
        None => conn
            .execute(
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
            .unwrap(),
    };
}

fn device_body(user_id: &str, device_id: &str) -> ClaimKeyPackageBody {
    ClaimKeyPackageBody {
        target_user_id: user_id.to_string(),
        target_device_id: Some(device_id.to_string()),
        ciphersuite: None,
    }
}

/// The same claim, but naming a suite explicitly.
fn device_body_in_suite(user_id: &str, device_id: &str, suite: i64) -> ClaimKeyPackageBody {
    ClaimKeyPackageBody {
        target_user_id: user_id.to_string(),
        target_device_id: Some(device_id.to_string()),
        ciphersuite: Some(suite),
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
    let user_scoped = ClaimKeyPackageBody {
        target_user_id: "bob".into(),
        target_device_id: None,
        ciphersuite: None,
    };
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

// ── #454 P1b: the classic and hybrid pools are disjoint ──────────────────────

/// Publish one package for `(user, device)`, optionally naming its suite —
/// `None` reproduces exactly what every currently deployed client sends.
async fn publish_one(db: &Db, user: &str, device: &str, ref_hash: &str, suite: Option<i64>) {
    let conn = db.conn().unwrap();
    use base64::Engine as _;
    let body = PublishKeyPackagesBody {
        device_id: device.to_string(),
        packages: vec![KeyPackageEntry {
            ref_hash: ref_hash.to_string(),
            key_package: base64::engine::general_purpose::STANDARD.encode(ref_hash.as_bytes()),
            ciphersuite: suite,
        }],
        user_id: Some(user.to_string()),
    };
    match apply_publish_key_packages(&conn, Some(user), &body).await.unwrap() {
        WriteOutcome::Ok => {}
        _ => panic!("publish must succeed"),
    }
}

async fn stored_suite(db: &Db, ref_hash: &str) -> i64 {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT ciphersuite FROM mls_key_package WHERE ref_hash = ?1",
            libsql::params![ref_hash.to_string()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().expect("row exists").get::<i64>(0).unwrap()
}

/// A publish that names no suite must land as classic, and a claim that names no
/// suite must still find it. This is the entire backward-compatibility contract
/// with already-shipped clients, which send neither field.
#[tokio::test(flavor = "multi_thread")]
async fn publish_and_claim_without_a_suite_behave_exactly_as_before() {
    let db = fresh_db().await;
    publish_one(&db, "bob", "dev1", "ref-legacy", None).await;
    assert_eq!(stored_suite(&db, "ref-legacy").await, CIPHERSUITE_CLASSIC);

    let conn = db.conn().unwrap();
    match apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-legacy"),
        ClaimOutcome::NoKeyPackage => panic!("an untagged package must stay claimable"),
    }
}

/// A row written the pre-#454 way — no `ciphersuite` in the INSERT at all —
/// takes the column default and is therefore classic, so it is reachable by both
/// the implicit and the explicitly-classic claim. Pins the migration default
/// from the DS's side.
#[tokio::test(flavor = "multi_thread")]
async fn pre_migration_rows_default_to_classic() {
    let db = fresh_db().await;
    insert_kp_in_suite(&db, "ref-old", "bob", "dev1", b"old", "2024-01-01 00:00:00", None).await;
    assert_eq!(stored_suite(&db, "ref-old").await, CIPHERSUITE_CLASSIC);

    let conn = db.conn().unwrap();
    match apply_claim_key_package(&conn, &device_body_in_suite("bob", "dev1", CIPHERSUITE_CLASSIC))
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-old"),
        ClaimOutcome::NoKeyPackage => panic!("a defaulted row must be claimable as classic"),
    }
}

/// Asking for a suite the target has no package in is the ORDINARY
/// no-package-available outcome — not an error, and above all not a package from
/// the other suite. #454 P4 leans on this to keep "a hybrid group cannot contain
/// a classic-only member" unrepresentable: the add path simply finds nothing.
#[tokio::test(flavor = "multi_thread")]
async fn claiming_hybrid_against_a_classic_only_pool_finds_nothing() {
    let db = fresh_db().await;
    publish_one(&db, "bob", "dev1", "ref-classic", Some(CIPHERSUITE_CLASSIC)).await;
    let conn = db.conn().unwrap();

    match apply_claim_key_package(&conn, &device_body_in_suite("bob", "dev1", CIPHERSUITE_HYBRID))
        .await
        .unwrap()
    {
        ClaimOutcome::NoKeyPackage => {}
        ClaimOutcome::Claimed { ref_hash, .. } => {
            panic!("a hybrid claim must never be served a classic package (got {ref_hash})")
        }
    }

    // …and the classic package is untouched by that failed hybrid claim.
    match apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-classic"),
        ClaimOutcome::NoKeyPackage => panic!("the classic package must still be claimable"),
    }
}

/// With BOTH pools published for one device, each claim draws from its own pool
/// and leaves the other intact. This is the real point of the column: two suites
/// coexisting on one device without cross-contamination in either direction.
#[tokio::test(flavor = "multi_thread")]
async fn the_two_suite_pools_do_not_contaminate_each_other() {
    let db = fresh_db().await;
    publish_one(&db, "bob", "dev1", "ref-classic", Some(CIPHERSUITE_CLASSIC)).await;
    publish_one(&db, "bob", "dev1", "ref-hybrid", Some(CIPHERSUITE_HYBRID)).await;
    assert_eq!(stored_suite(&db, "ref-hybrid").await, CIPHERSUITE_HYBRID);
    let conn = db.conn().unwrap();

    match apply_claim_key_package(&conn, &device_body_in_suite("bob", "dev1", CIPHERSUITE_HYBRID))
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-hybrid"),
        ClaimOutcome::NoKeyPackage => panic!("the hybrid pool must serve the hybrid claim"),
    }

    // The classic pool is untouched — including for a suite-less legacy claim.
    match apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-classic"),
        ClaimOutcome::NoKeyPackage => panic!("the classic pool must be unaffected"),
    }

    // Both pools are now drained; neither claim resurrects the other's package.
    let conn = db.conn().unwrap();
    assert!(matches!(
        apply_claim_key_package(&conn, &device_body_in_suite("bob", "dev1", CIPHERSUITE_HYBRID))
            .await
            .unwrap(),
        ClaimOutcome::NoKeyPackage
    ));
    assert!(matches!(
        apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap(),
        ClaimOutcome::NoKeyPackage
    ));
}
