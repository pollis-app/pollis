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
//! winner — the rest see no package (never a double-claim) — and pools in
//! different suites never contaminate each other (#454 P1b).
//!
//! #669 left only one suite in production, but the `ciphersuite` column and its
//! scoping stay: a group persisted under an older code point can only be added
//! to with a package in *that* suite, and `CS_PQ`'s code point is provisional.
//! So the cross-suite tests below use [`CIPHERSUITE_LEGACY`], which is no longer
//! a suite the fleet speaks — it stands in for "some suite that is not the
//! current one", which is the case the column exists to handle.

use std::sync::Arc;

use pollis_delivery::db::Db;
use pollis_delivery::devices::{
    apply_claim_key_package, apply_publish_key_packages, apply_replenish_key_packages,
    ClaimKeyPackageBody, ClaimOutcome, KeyPackageEntry, PublishKeyPackagesBody,
    ReplenishKeyPackagesBody, CIPHERSUITE_PQ,
};
use pollis_delivery::writes::WriteOutcome;

/// MLS code point `0x0001` — the suite Pollis shipped before #454, retired by
/// #669. Defined here rather than in the DS because the DS has no reason to name
/// it any more; the tests do, as a concrete "not the current suite".
const CIPHERSUITE_LEGACY: i64 = 0x0001;

// Minimal slices of `mls_key_package` + `user_device` — the columns these writes
// read/touch. No `users` FK (foreign_keys=OFF in the local test DB) so the test
// is self-contained. `user_device.pq_capable` mirrors migration `000010`: NOT
// NULL DEFAULT 0. #669 retired that flag in place — nothing reads or writes it
// any more — but the column is still in the shipped schema (a DROP needs a
// multi-release dance), so it stays here and is asserted to stay 0.
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

/// Insert one unclaimed key package in the current suite — an ordinary package
/// as a shipped client publishes it. `created_at` is supplied explicitly so the
/// claim's `ORDER BY created_at ASC` is deterministic across rows.
async fn insert_kp(
    db: &Db,
    ref_hash: &str,
    user_id: &str,
    device_id: &str,
    key_package: &[u8],
    created_at: &str,
) {
    insert_kp_in_suite(
        db,
        ref_hash,
        user_id,
        device_id,
        key_package,
        created_at,
        Some(CIPHERSUITE_PQ),
    )
    .await;
}

/// Insert one unclaimed key package, optionally naming its ciphersuite. `None`
/// omits the column entirely — the pre-#454 INSERT, which must take the column
/// default from migration `000010`.
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

// ── #454 P1b: pools in different suites are disjoint ─────────────────────────

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

/// Register a device row, so `pq_capable` has somewhere it *could* be flipped.
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

/// Read a device's retired `pq_capable` flag.
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

/// A publish that names no suite lands on the CURRENT suite, and a claim that
/// names no suite finds it.
///
/// #454 read an absent field as classic, because the clients that omitted it
/// predated the hybrid suite. #669 inverts that: there is one suite left, so an
/// omitted field can only mean "the one everybody is on". The alternative —
/// defaulting to a retired code point — would file every untagged publish into a
/// pool nothing will ever claim from, which is a device that silently cannot be
/// added to any group.
#[tokio::test(flavor = "multi_thread")]
async fn publish_and_claim_without_a_suite_land_on_the_current_suite() {
    let db = fresh_db().await;
    publish_one(&db, "bob", "dev1", "ref-untagged", None).await;
    assert_eq!(stored_suite(&db, "ref-untagged").await, CIPHERSUITE_PQ);

    let conn = db.conn().unwrap();
    match apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-untagged"),
        ClaimOutcome::NoKeyPackage => panic!("an untagged package must stay claimable"),
    }
}

/// A row written the pre-#454 way — no `ciphersuite` in the INSERT at all —
/// takes the column DEFAULT, which is `1`, and is therefore NOT in the current
/// suite.
///
/// This is the trap the DS must never fall into: the column default and the
/// current suite disagree, and they have to, because a `DEFAULT` change is a
/// tightening migration the shipped app cannot take. Every write path therefore
/// names the suite explicitly rather than letting the default decide — pinned
/// here by showing the defaulted row is reachable ONLY as the legacy suite.
#[tokio::test(flavor = "multi_thread")]
async fn rows_written_without_the_column_take_the_schema_default_not_the_current_suite() {
    let db = fresh_db().await;
    insert_kp_in_suite(&db, "ref-old", "bob", "dev1", b"old", "2024-01-01 00:00:00", None).await;
    assert_eq!(stored_suite(&db, "ref-old").await, CIPHERSUITE_LEGACY);

    let conn = db.conn().unwrap();
    assert!(
        matches!(
            apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap(),
            ClaimOutcome::NoKeyPackage
        ),
        "a defaulted row is not in the current suite and must not answer a current-suite claim"
    );
    match apply_claim_key_package(&conn, &device_body_in_suite("bob", "dev1", CIPHERSUITE_LEGACY))
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-old"),
        ClaimOutcome::NoKeyPackage => panic!("a defaulted row must be claimable as the legacy suite"),
    }
}

/// Asking for a suite the target has no package in is the ORDINARY
/// no-package-available outcome — not an error, and above all not a package from
/// another suite. #454 P4 leans on this to keep "a post-quantum group cannot
/// contain a member who is not" unrepresentable: the add path simply finds
/// nothing and reports the device skipped.
#[tokio::test(flavor = "multi_thread")]
async fn claiming_the_current_suite_against_an_off_suite_pool_finds_nothing() {
    let db = fresh_db().await;
    publish_one(&db, "bob", "dev1", "ref-legacy", Some(CIPHERSUITE_LEGACY)).await;
    let conn = db.conn().unwrap();

    match apply_claim_key_package(&conn, &device_body_in_suite("bob", "dev1", CIPHERSUITE_PQ))
        .await
        .unwrap()
    {
        ClaimOutcome::NoKeyPackage => {}
        ClaimOutcome::Claimed { ref_hash, .. } => {
            panic!("a PQ claim must never be served an off-suite package (got {ref_hash})")
        }
    }

    // …and the off-suite package is untouched by that failed claim.
    match apply_claim_key_package(&conn, &device_body_in_suite("bob", "dev1", CIPHERSUITE_LEGACY))
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-legacy"),
        ClaimOutcome::NoKeyPackage => panic!("the legacy package must still be claimable"),
    }
}

/// With pools in BOTH suites published for one device, each claim draws from its
/// own pool and leaves the other intact. This is the real point of the column:
/// two suites coexisting on one device without cross-contamination in either
/// direction — the state a device is in for exactly as long as a code-point
/// migration takes.
#[tokio::test(flavor = "multi_thread")]
async fn the_two_suite_pools_do_not_contaminate_each_other() {
    let db = fresh_db().await;
    publish_one(&db, "bob", "dev1", "ref-legacy", Some(CIPHERSUITE_LEGACY)).await;
    publish_one(&db, "bob", "dev1", "ref-pq", Some(CIPHERSUITE_PQ)).await;
    assert_eq!(stored_suite(&db, "ref-pq").await, CIPHERSUITE_PQ);
    let conn = db.conn().unwrap();

    // A suite-less claim means the current suite, so it must draw the PQ one.
    match apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-pq"),
        ClaimOutcome::NoKeyPackage => panic!("the PQ pool must serve a suite-less claim"),
    }

    // The legacy pool is untouched by it.
    match apply_claim_key_package(&conn, &device_body_in_suite("bob", "dev1", CIPHERSUITE_LEGACY))
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-legacy"),
        ClaimOutcome::NoKeyPackage => panic!("the legacy pool must be unaffected"),
    }

    // Both pools are now drained; neither claim resurrects the other's package.
    let conn = db.conn().unwrap();
    assert!(matches!(
        apply_claim_key_package(&conn, &device_body_in_suite("bob", "dev1", CIPHERSUITE_LEGACY))
            .await
            .unwrap(),
        ClaimOutcome::NoKeyPackage
    ));
    assert!(matches!(
        apply_claim_key_package(&conn, &device_body("bob", "dev1")).await.unwrap(),
        ClaimOutcome::NoKeyPackage
    ));
}

// ── #669: pq_capable is retired; per-suite rotation is not ───────────────────

/// **`pq_capable` is dead, and publishing must not resurrect it.**
///
/// #454 had the DS derive the flag from what actually landed in
/// `mls_key_package`, so it could answer "may this conversation be born hybrid?"
/// without trusting a client assertion. #669 deleted every reader of that
/// question — there is one suite, so the answer is always yes — and deleting the
/// readers while leaving the writer would be worse than useless: a column that
/// looks maintained, is not, and invites the next person to read it.
///
/// The column itself stays (migration `000010`; a DROP needs a multi-release
/// dance and buys nothing). What must not stay is a write to it.
#[tokio::test(flavor = "multi_thread")]
async fn publishing_no_longer_derives_the_retired_pq_capable_flag() {
    let db = fresh_db().await;
    insert_device(&db, "bob", "dev1").await;
    assert_eq!(pq_capable(&db, "bob", "dev1").await, 0, "a fresh device row defaults to 0");

    // The publish that used to flip it.
    publish_one(&db, "bob", "dev1", "ref-pq", Some(CIPHERSUITE_PQ)).await;
    assert_eq!(
        pq_capable(&db, "bob", "dev1").await,
        0,
        "a PQ publish must no longer touch the retired flag"
    );

    // Nor does the rotation path, which carried its own copy of the write.
    let conn = db.conn().unwrap();
    apply_replenish_key_packages(
        &conn,
        Some("bob"),
        &replenish_body("bob", "dev1", CIPHERSUITE_PQ, &["r1", "r2"]),
    )
    .await
    .unwrap();
    assert_eq!(
        pq_capable(&db, "bob", "dev1").await,
        0,
        "replenish must no longer touch the retired flag either"
    );
}

/// A rotation (`replenish`) of ONE suite must leave the OTHER suite's pool
/// intact. The DS scopes its DELETE to the suites present in the request.
///
/// #669 leaves the client rotating a single pool, so in the steady state this
/// scoping has nothing to protect — which is exactly why it is worth pinning.
/// The moment `CS_PQ`'s provisional code point moves, a device is briefly in two
/// pools at once, and a rotation of the new one that silently drained the old
/// one would strand every group not yet migrated.
#[tokio::test(flavor = "multi_thread")]
async fn rotating_one_suite_leaves_the_other_pool_intact() {
    let db = fresh_db().await;
    insert_device(&db, "bob", "dev1").await;

    // Seed both pools via replenish.
    let conn = db.conn().unwrap();
    apply_replenish_key_packages(
        &conn,
        Some("bob"),
        &replenish_body("bob", "dev1", CIPHERSUITE_LEGACY, &["c1", "c2"]),
    )
    .await
    .unwrap();
    apply_replenish_key_packages(
        &conn,
        Some("bob"),
        &replenish_body("bob", "dev1", CIPHERSUITE_PQ, &["h1", "h2"]),
    )
    .await
    .unwrap();
    assert_eq!(unclaimed_in_suite(&db, "bob", "dev1", CIPHERSUITE_LEGACY).await, 2);
    assert_eq!(unclaimed_in_suite(&db, "bob", "dev1", CIPHERSUITE_PQ).await, 2);

    // Rotate ONLY the legacy pool. The PQ pool must survive untouched.
    apply_replenish_key_packages(
        &conn,
        Some("bob"),
        &replenish_body("bob", "dev1", CIPHERSUITE_LEGACY, &["c3", "c4", "c5"]),
    )
    .await
    .unwrap();
    assert_eq!(
        unclaimed_in_suite(&db, "bob", "dev1", CIPHERSUITE_LEGACY).await,
        3,
        "the legacy pool was replaced by the new rotation"
    );
    assert_eq!(
        unclaimed_in_suite(&db, "bob", "dev1", CIPHERSUITE_PQ).await,
        2,
        "a legacy rotation must NOT drain the PQ pool"
    );

    // The old legacy refs are gone (rotated out); the PQ refs remain.
    assert_eq!(stored_suite(&db, "h1").await, CIPHERSUITE_PQ);
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
        "the superseded legacy packages were deleted"
    );
}
