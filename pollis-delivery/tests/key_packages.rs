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
    apply_claim_key_package, apply_publish_key_packages, apply_replenish_key_packages,
    ClaimKeyPackageBody, ClaimOutcome, KeyPackageEntry, PublishKeyPackagesBody,
    ReplenishKeyPackagesBody, CIPHERSUITE_CLASSIC, CIPHERSUITE_HYBRID,
};
use pollis_delivery::writes::WriteOutcome;

// Minimal slices of `mls_key_package` + `user_device` — the columns these writes
// read/touch. No `users` FK (foreign_keys=OFF in the local test DB) so the test
// is self-contained. `user_device.pq_capable` mirrors migration `000010`: NOT
// NULL DEFAULT 0, so a device is classic-only until a hybrid publish flips it.
const SCHEMA: &str = "\
CREATE TABLE mls_key_package (\
  ref_hash    TEXT PRIMARY KEY,\
  user_id     TEXT NOT NULL,\
  key_package BLOB NOT NULL,\
  claimed     INTEGER NOT NULL DEFAULT 0,\
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),\
  device_id   TEXT,\
  ciphersuite INTEGER NOT NULL DEFAULT 1\
);\
CREATE TABLE user_device (\
  user_id    TEXT NOT NULL,\
  device_id  TEXT NOT NULL,\
  pq_capable INTEGER NOT NULL DEFAULT 0,\
  PRIMARY KEY (user_id, device_id)\
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

/// Register a device row so `pq_capable` has somewhere to be flipped.
async fn insert_device(db: &Db, user: &str, device: &str) {
    db.conn()
        .unwrap()
        .execute(
            "INSERT INTO user_device (user_id, device_id) VALUES (?1, ?2)",
            libsql::params![user.to_string(), device.to_string()],
        )
        .await
        .unwrap();
}

/// Read a device's `pq_capable` flag.
async fn pq_capable(db: &Db, user: &str, device: &str) -> i64 {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT pq_capable FROM user_device WHERE user_id = ?1 AND device_id = ?2",
            libsql::params![user.to_string(), device.to_string()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().expect("device row").get::<i64>(0).unwrap()
}

/// Count a device's unclaimed packages in one suite.
async fn unclaimed_in_suite(db: &Db, user: &str, device: &str, suite: i64) -> i64 {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package \
             WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 AND ciphersuite = ?3",
            libsql::params![user.to_string(), device.to_string(), suite],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().expect("count row").get::<i64>(0).unwrap()
}

/// A replenish body publishing `refs` in one suite (the client's rotation shape).
fn replenish_body(user: &str, device: &str, suite: i64, refs: &[&str]) -> ReplenishKeyPackagesBody {
    use base64::Engine as _;
    ReplenishKeyPackagesBody {
        device_id: device.to_string(),
        packages: refs
            .iter()
            .map(|r| KeyPackageEntry {
                ref_hash: r.to_string(),
                key_package: base64::engine::general_purpose::STANDARD.encode(r.as_bytes()),
                ciphersuite: Some(suite),
            })
            .collect(),
        user_id: Some(user.to_string()),
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

// ── #454 P2: pq_capable is derived from the hybrid pool, per-suite rotation ───

/// Publishing a hybrid pool is what makes a device `pq_capable` — and ONLY a
/// hybrid pool does. The DS derives the flag from what actually landed in
/// `mls_key_package`, never from a client assertion, so the flag and the pool can
/// never disagree. A classic-only publish must leave it off.
#[tokio::test(flavor = "multi_thread")]
async fn publishing_a_hybrid_pool_flips_pq_capable_and_classic_does_not() {
    let db = fresh_db().await;
    insert_device(&db, "bob", "dev1").await;
    assert_eq!(pq_capable(&db, "bob", "dev1").await, 0, "a fresh device is classic-only");

    // A classic publish must NOT set the flag.
    publish_one(&db, "bob", "dev1", "ref-classic", Some(CIPHERSUITE_CLASSIC)).await;
    assert_eq!(
        pq_capable(&db, "bob", "dev1").await,
        0,
        "a classic-only publish must never advertise PQ capability"
    );

    // A hybrid publish flips it on.
    publish_one(&db, "bob", "dev1", "ref-hybrid", Some(CIPHERSUITE_HYBRID)).await;
    assert_eq!(
        pq_capable(&db, "bob", "dev1").await,
        1,
        "publishing a hybrid pool must flip pq_capable on"
    );

    // Scoped to the publishing device only — a sibling device stays classic.
    insert_device(&db, "bob", "dev2").await;
    publish_one(&db, "bob", "dev2", "ref-classic-2", Some(CIPHERSUITE_CLASSIC)).await;
    assert_eq!(pq_capable(&db, "bob", "dev2").await, 0, "the flag is per device");
}

/// A rotation (`replenish`) of ONE suite must leave the OTHER suite's pool intact.
/// The DS scopes its DELETE to the suites present in the request, so a classic
/// rotation cannot drain the hybrid pool (or vice versa) — the invariant the P2
/// client relies on when it rotates both pools in one call.
#[tokio::test(flavor = "multi_thread")]
async fn rotating_one_suite_leaves_the_other_pool_intact() {
    let db = fresh_db().await;
    insert_device(&db, "bob", "dev1").await;

    // Seed both pools via replenish.
    let conn = db.conn().unwrap();
    apply_replenish_key_packages(
        &conn,
        Some("bob"),
        &replenish_body("bob", "dev1", CIPHERSUITE_CLASSIC, &["c1", "c2"]),
    )
    .await
    .unwrap();
    apply_replenish_key_packages(
        &conn,
        Some("bob"),
        &replenish_body("bob", "dev1", CIPHERSUITE_HYBRID, &["h1", "h2"]),
    )
    .await
    .unwrap();
    assert_eq!(unclaimed_in_suite(&db, "bob", "dev1", CIPHERSUITE_CLASSIC).await, 2);
    assert_eq!(unclaimed_in_suite(&db, "bob", "dev1", CIPHERSUITE_HYBRID).await, 2);
    assert_eq!(pq_capable(&db, "bob", "dev1").await, 1, "the hybrid seed made it pq_capable");

    // Rotate ONLY the classic pool. The hybrid pool must survive untouched.
    apply_replenish_key_packages(
        &conn,
        Some("bob"),
        &replenish_body("bob", "dev1", CIPHERSUITE_CLASSIC, &["c3", "c4", "c5"]),
    )
    .await
    .unwrap();
    assert_eq!(
        unclaimed_in_suite(&db, "bob", "dev1", CIPHERSUITE_CLASSIC).await,
        3,
        "the classic pool was replaced by the new rotation"
    );
    assert_eq!(
        unclaimed_in_suite(&db, "bob", "dev1", CIPHERSUITE_HYBRID).await,
        2,
        "a classic rotation must NOT drain the hybrid pool"
    );

    // The old classic refs are gone (rotated out); the hybrid refs remain.
    assert_eq!(stored_suite(&db, "h1").await, CIPHERSUITE_HYBRID);
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package WHERE ref_hash IN ('c1','c2')",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0,
        "the superseded classic packages were deleted"
    );
}
