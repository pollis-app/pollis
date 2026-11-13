//! Key-package CLAIM endpoint invariants, against a real (local) libsql DB.
//! The `ciphersuite` column stays (#669), so cross-suite tests use [`CIPHERSUITE_LEGACY`].

use std::sync::Arc;

use pollis_delivery::db::Db;
use pollis_delivery::devices::{
    apply_claim_key_package, apply_publish_key_packages, apply_replenish_key_packages,
    ClaimKeyPackageBody, ClaimOutcome, PublishKeyPackagesBody,
    ReplenishKeyPackagesBody, CIPHERSUITE_PQ,
};
use pollis_delivery::writes::WriteOutcome;

mod common;

/// MLS code point `0x0001`, retired by #669: a concrete "not the current suite".
const CIPHERSUITE_LEGACY: i64 = 0x0001;


/// `alice` and `bob` in one group: a claim requires a shared conversation.
async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("kp.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    let conn = db.conn().await.unwrap();
    for u in ["alice", "bob"] {
        conn.execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id) VALUES ('kp-grp', ?1)",
            libsql::params![u],
        )
        .await
        .unwrap();
    }
    db
}

/// One unclaimed current-suite package; explicit `created_at` makes claim order
/// deterministic. Returns the stored bytes: a real package of a certified device,
/// since the DS serves nothing else (#1851).
async fn insert_kp(
    db: &Db,
    ref_hash: &str,
    user_id: &str,
    device_id: &str,
    created_at: &str,
) -> Vec<u8> {
    insert_kp_in_suite(db, ref_hash, user_id, device_id, created_at, Some(CIPHERSUITE_PQ)).await
}

/// `None` omits the column: the pre-#454 INSERT, taking the `000010` default
/// (the retired classic suite, whose package it then is).
async fn insert_kp_in_suite(
    db: &Db,
    ref_hash: &str,
    user_id: &str,
    device_id: &str,
    created_at: &str,
    ciphersuite: Option<i64>,
) -> Vec<u8> {
    let conn = db.conn().await.unwrap();
    let suite = suite_of(ciphersuite.unwrap_or(CIPHERSUITE_LEGACY));
    let key_package = common::mls::shared_device(&conn, user_id, device_id)
        .await
        .key_package_in(suite, false)
        .bytes;
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
    key_package
}

fn suite_of(code: i64) -> openmls::prelude::Ciphersuite {
    openmls::prelude::Ciphersuite::try_from(code as u16).expect("known suite")
}

fn device_body(user_id: &str, device_id: &str) -> ClaimKeyPackageBody {
    ClaimKeyPackageBody {
        target_user_id: user_id.to_string(),
        target_device_id: Some(device_id.to_string()),
        ciphersuite: None,
    }
}

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
    let kp_bytes_a = insert_kp(&db, "ref-a", "bob", "dev1", "2024-01-01 00:00:00").await;
    let conn = db.conn().await.unwrap();

    match apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, key_package } => {
            assert_eq!(ref_hash, "ref-a");
            assert_eq!(key_package, kp_bytes_a);
        }
        ClaimOutcome::NoKeyPackage => panic!("expected a claimed package"),
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
    }

    match apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::NoKeyPackage => {}
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
        ClaimOutcome::Claimed { .. } => panic!("a claimed package must not be re-claimable"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn claim_is_oldest_first_and_device_scoped() {
    let db = fresh_db().await;
    let old = insert_kp(&db, "ref-old", "bob", "dev1", "2024-01-01 00:00:00").await;
    let _new = insert_kp(&db, "ref-new", "bob", "dev1", "2024-02-01 00:00:00").await;
    let _d2 = insert_kp(&db, "ref-d2", "bob", "dev2", "2024-01-01 00:00:00").await;
    let conn = db.conn().await.unwrap();

    match apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, key_package } => {
            assert_eq!(ref_hash, "ref-old");
            assert_eq!(key_package, old);
        }
        ClaimOutcome::NoKeyPackage => panic!("expected the oldest dev1 package"),
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
    }

    let user_scoped = ClaimKeyPackageBody {
        target_user_id: "bob".into(),
        target_device_id: None,
        ciphersuite: None,
    };
    // ref-new and ref-d2 share a timestamp, so either may win.
    match apply_claim_key_package(&conn, Some("alice"), &user_scoped).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => {
            assert!(ref_hash == "ref-new" || ref_hash == "ref-d2", "got {ref_hash}");
        }
        ClaimOutcome::NoKeyPackage => panic!("expected a remaining package"),
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_claims_of_one_package_yield_exactly_one_winner() {
    let db = fresh_db().await;
    let the_only_kp = insert_kp(&db, "ref-solo", "bob", "dev1", "2024-01-01 00:00:00").await;

    let mut handles = Vec::new();
    for _ in 0..8 {
        let db = Arc::clone(&db);
        let the_only_kp = the_only_kp.clone();
        handles.push(tokio::spawn(async move {
            let conn = db.conn().await.unwrap();
            // busy_timeout is per-connection: a local-file artifact; Turso serializes server-side.
            conn.execute_batch("PRAGMA busy_timeout=10000;").await.unwrap();
            (
                apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap(),
                the_only_kp,
            )
        }));
    }

    let mut claimed = 0;
    let mut none = 0;
    for h in handles {
        match h.await.unwrap() {
            (ClaimOutcome::Claimed { ref_hash, key_package }, the_only_kp) => {
                assert_eq!(ref_hash, "ref-solo");
                assert_eq!(key_package, the_only_kp);
                claimed += 1;
            }
            (ClaimOutcome::NoKeyPackage, _) => none += 1,
            (other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited), _) => {
                panic!("the claim gate refused an unrelated test claim: {other:?}")
            }
        }
    }

    assert_eq!(claimed, 1, "exactly one claimer may win the single package");
    assert_eq!(none, 7, "every other claimer must see no package — no double-claim");

    let conn = db.conn().await.unwrap();
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

/// `None` reproduces what every deployed client sends. Returns the package's reference.
async fn publish_one(db: &Db, user: &str, device: &str, suite: Option<i64>) -> String {
    let conn = db.conn().await.unwrap();
    let kp = common::mls::shared_device(&conn, user, device)
        .await
        .key_package_in(suite_of(suite.unwrap_or(CIPHERSUITE_PQ)), false);
    let mut entry = kp.entry();
    entry.ciphersuite = suite;
    let body = PublishKeyPackagesBody {
        device_id: device.to_string(),
        packages: vec![entry],
        user_id: Some(user.to_string()),
    };
    match apply_publish_key_packages(&conn, Some(user), &body).await.unwrap() {
        WriteOutcome::Ok => {}
        other => panic!("publish must succeed, got {other:?}"),
    }
    kp.ref_hash
}

async fn insert_device(db: &Db, user: &str, device: &str) {
    db.conn().await
        .unwrap()
        .execute(
            "INSERT INTO user_device (user_id, device_id) VALUES (?1, ?2)",
            libsql::params![user.to_string(), device.to_string()],
        )
        .await
        .unwrap();
}

async fn pq_capable(db: &Db, user: &str, device: &str) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT pq_capable FROM user_device WHERE user_id = ?1 AND device_id = ?2",
            libsql::params![user.to_string(), device.to_string()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().expect("device row").get::<i64>(0).unwrap()
}

async fn unclaimed_in_suite(db: &Db, user: &str, device: &str, suite: i64) -> i64 {
    let conn = db.conn().await.unwrap();
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

/// A replenish of `n` real packages in `suite`, and their references.
async fn replenish_body(
    db: &Db,
    user: &str,
    device: &str,
    suite: i64,
    n: usize,
) -> (ReplenishKeyPackagesBody, Vec<String>) {
    let conn = db.conn().await.unwrap();
    let dev = common::mls::shared_device(&conn, user, device).await;
    let kps: Vec<_> = (0..n).map(|_| dev.key_package_in(suite_of(suite), false)).collect();
    let body = ReplenishKeyPackagesBody {
        device_id: device.to_string(),
        packages: kps.iter().map(|k| k.entry()).collect(),
        user_id: Some(user.to_string()),
    };
    (body, kps.into_iter().map(|k| k.ref_hash).collect())
}

async fn stored_suite(db: &Db, ref_hash: &str) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT ciphersuite FROM mls_key_package WHERE ref_hash = ?1",
            libsql::params![ref_hash.to_string()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().expect("row exists").get::<i64>(0).unwrap()
}

/// An untagged publish and claim mean the current suite (#669), never a retired code point.
#[tokio::test(flavor = "multi_thread")]
async fn publish_and_claim_without_a_suite_land_on_the_current_suite() {
    let db = fresh_db().await;
    let ref_untagged = publish_one(&db, "bob", "dev1", None).await;
    assert_eq!(stored_suite(&db, &ref_untagged).await, CIPHERSUITE_PQ);

    let conn = db.conn().await.unwrap();
    match apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, ref_untagged),
        ClaimOutcome::NoKeyPackage => panic!("an untagged package must stay claimable"),
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
    }
}

/// The column default (`1`) is not the current suite, so every write path names the suite.
#[tokio::test(flavor = "multi_thread")]
async fn rows_written_without_the_column_take_the_schema_default_not_the_current_suite() {
    let db = fresh_db().await;
    insert_kp_in_suite(&db, "ref-old", "bob", "dev1", "2024-01-01 00:00:00", None).await;
    assert_eq!(stored_suite(&db, "ref-old").await, CIPHERSUITE_LEGACY);

    let conn = db.conn().await.unwrap();
    assert!(
        matches!(
            apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap(),
            ClaimOutcome::NoKeyPackage
        ),
        "a defaulted row is not in the current suite and must not answer a current-suite claim"
    );
    match apply_claim_key_package(&conn, Some("alice"), &device_body_in_suite("bob", "dev1", CIPHERSUITE_LEGACY))
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-old"),
        ClaimOutcome::NoKeyPackage => panic!("a defaulted row must be claimable as the legacy suite"),
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
    }
}

/// No package in the asked suite is ordinary absence, never another suite's package.
#[tokio::test(flavor = "multi_thread")]
async fn claiming_the_current_suite_against_an_off_suite_pool_finds_nothing() {
    let db = fresh_db().await;
    let ref_legacy = publish_one(&db, "bob", "dev1", Some(CIPHERSUITE_LEGACY)).await;
    let conn = db.conn().await.unwrap();

    match apply_claim_key_package(&conn, Some("alice"), &device_body_in_suite("bob", "dev1", CIPHERSUITE_PQ))
        .await
        .unwrap()
    {
        ClaimOutcome::NoKeyPackage => {}
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
        ClaimOutcome::Claimed { ref_hash, .. } => {
            panic!("a PQ claim must never be served an off-suite package (got {ref_hash})")
        }
    }

    match apply_claim_key_package(&conn, Some("alice"), &device_body_in_suite("bob", "dev1", CIPHERSUITE_LEGACY))
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, ref_legacy),
        ClaimOutcome::NoKeyPackage => panic!("the legacy package must still be claimable"),
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
    }
}

/// Two pools on one device stay disjoint in both directions.
#[tokio::test(flavor = "multi_thread")]
async fn the_two_suite_pools_do_not_contaminate_each_other() {
    let db = fresh_db().await;
    let ref_legacy = publish_one(&db, "bob", "dev1", Some(CIPHERSUITE_LEGACY)).await;
    let ref_pq = publish_one(&db, "bob", "dev1", Some(CIPHERSUITE_PQ)).await;
    assert_eq!(stored_suite(&db, &ref_pq).await, CIPHERSUITE_PQ);
    let conn = db.conn().await.unwrap();

    match apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, ref_pq),
        ClaimOutcome::NoKeyPackage => panic!("the PQ pool must serve a suite-less claim"),
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
    }

    match apply_claim_key_package(&conn, Some("alice"), &device_body_in_suite("bob", "dev1", CIPHERSUITE_LEGACY))
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, ref_legacy),
        ClaimOutcome::NoKeyPackage => panic!("the legacy pool must be unaffected"),
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
    }

    let conn = db.conn().await.unwrap();
    assert!(matches!(
        apply_claim_key_package(&conn, Some("alice"), &device_body_in_suite("bob", "dev1", CIPHERSUITE_LEGACY))
            .await
            .unwrap(),
        ClaimOutcome::NoKeyPackage
    ));
    assert!(matches!(
        apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap(),
        ClaimOutcome::NoKeyPackage
    ));
}

/// `pq_capable` is retired (#669): publishing must not write it.
#[tokio::test(flavor = "multi_thread")]
async fn publishing_no_longer_derives_the_retired_pq_capable_flag() {
    let db = fresh_db().await;
    insert_device(&db, "bob", "dev1").await;
    assert_eq!(pq_capable(&db, "bob", "dev1").await, 0, "a fresh device row defaults to 0");

    let _ref_pq = publish_one(&db, "bob", "dev1", Some(CIPHERSUITE_PQ)).await;
    assert_eq!(
        pq_capable(&db, "bob", "dev1").await,
        0,
        "a PQ publish must no longer touch the retired flag"
    );

    let conn = db.conn().await.unwrap();
    let (body, _) = replenish_body(&db, "bob", "dev1", CIPHERSUITE_PQ, 2).await;
    apply_replenish_key_packages(&conn, Some("bob"), &body).await.unwrap();
    assert_eq!(
        pq_capable(&db, "bob", "dev1").await,
        0,
        "replenish must no longer touch the retired flag either"
    );
}

/// A rotation of one suite leaves the other pool intact, for code-point migrations.
#[tokio::test(flavor = "multi_thread")]
async fn rotating_one_suite_leaves_the_other_pool_intact() {
    let db = fresh_db().await;
    insert_device(&db, "bob", "dev1").await;

    let conn = db.conn().await.unwrap();
    let (body, c_refs) = replenish_body(&db, "bob", "dev1", CIPHERSUITE_LEGACY, 2).await;
    assert!(matches!(
        apply_replenish_key_packages(&conn, Some("bob"), &body).await.unwrap(),
        WriteOutcome::Ok
    ));
    let (body, h_refs) = replenish_body(&db, "bob", "dev1", CIPHERSUITE_PQ, 2).await;
    assert!(matches!(
        apply_replenish_key_packages(&conn, Some("bob"), &body).await.unwrap(),
        WriteOutcome::Ok
    ));
    assert_eq!(unclaimed_in_suite(&db, "bob", "dev1", CIPHERSUITE_LEGACY).await, 2);
    assert_eq!(unclaimed_in_suite(&db, "bob", "dev1", CIPHERSUITE_PQ).await, 2);

    let (body, _) = replenish_body(&db, "bob", "dev1", CIPHERSUITE_LEGACY, 3).await;
    apply_replenish_key_packages(&conn, Some("bob"), &body).await.unwrap();
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

    assert_eq!(stored_suite(&db, &h_refs[0]).await, CIPHERSUITE_PQ);
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package WHERE ref_hash IN (?1, ?2)",
            libsql::params![c_refs[0].clone(), c_refs[1].clone()],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0,
        "the superseded legacy packages were deleted"
    );
}
