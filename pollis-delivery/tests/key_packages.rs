//! Key-package CLAIM endpoint invariants, against a real (local) libsql DB.
//! The `ciphersuite` column stays (#669), so cross-suite tests use [`CIPHERSUITE_LEGACY`].

use std::sync::Arc;

use pollis_delivery::db::Db;
use pollis_delivery::devices::{
    apply_claim_key_package, apply_publish_key_packages, apply_replenish_key_packages,
    ClaimKeyPackageBody, ClaimOutcome, KeyPackageEntry, PublishKeyPackagesBody,
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

/// One unclaimed current-suite package; explicit `created_at` makes claim order deterministic.
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

/// `None` omits the column: the pre-#454 INSERT, taking the `000010` default.
async fn insert_kp_in_suite(
    db: &Db,
    ref_hash: &str,
    user_id: &str,
    device_id: &str,
    key_package: &[u8],
    created_at: &str,
    ciphersuite: Option<i64>,
) {
    let conn = db.conn().await.unwrap();
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
    let conn = db.conn().await.unwrap();

    match apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, key_package } => {
            assert_eq!(ref_hash, "ref-a");
            assert_eq!(key_package, b"kp-bytes-a");
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
    insert_kp(&db, "ref-old", "bob", "dev1", b"old", "2024-01-01 00:00:00").await;
    insert_kp(&db, "ref-new", "bob", "dev1", b"new", "2024-02-01 00:00:00").await;
    insert_kp(&db, "ref-d2", "bob", "dev2", b"d2", "2024-01-01 00:00:00").await;
    let conn = db.conn().await.unwrap();

    match apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, key_package } => {
            assert_eq!(ref_hash, "ref-old");
            assert_eq!(key_package, b"old");
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
    insert_kp(&db, "ref-solo", "bob", "dev1", b"the-only-kp", "2024-01-01 00:00:00").await;

    let mut handles = Vec::new();
    for _ in 0..8 {
        let db = Arc::clone(&db);
        handles.push(tokio::spawn(async move {
            let conn = db.conn().await.unwrap();
            // busy_timeout is per-connection: a local-file artifact; Turso serializes server-side.
            conn.execute_batch("PRAGMA busy_timeout=10000;").await.unwrap();
            apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap()
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
            other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
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

/// `None` reproduces what every deployed client sends.
async fn publish_one(db: &Db, user: &str, device: &str, ref_hash: &str, suite: Option<i64>) {
    let conn = db.conn().await.unwrap();
    use base64::Engine as _;
    let body = PublishKeyPackagesBody {
        device_id: device.to_string(),
        packages: vec![KeyPackageEntry {
            ref_hash: ref_hash.to_string(),
            key_package: base64::engine::general_purpose::STANDARD.encode(ref_hash.as_bytes()),
            ciphersuite: suite,
            last_resort: false,
        }],
        user_id: Some(user.to_string()),
    };
    match apply_publish_key_packages(&conn, Some(user), &body).await.unwrap() {
        WriteOutcome::Ok => {}
        _ => panic!("publish must succeed"),
    }
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
                last_resort: false,
            })
            .collect(),
        user_id: Some(user.to_string()),
    }
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
    publish_one(&db, "bob", "dev1", "ref-untagged", None).await;
    assert_eq!(stored_suite(&db, "ref-untagged").await, CIPHERSUITE_PQ);

    let conn = db.conn().await.unwrap();
    match apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-untagged"),
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
    insert_kp_in_suite(&db, "ref-old", "bob", "dev1", b"old", "2024-01-01 00:00:00", None).await;
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
    publish_one(&db, "bob", "dev1", "ref-legacy", Some(CIPHERSUITE_LEGACY)).await;
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
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-legacy"),
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
    publish_one(&db, "bob", "dev1", "ref-legacy", Some(CIPHERSUITE_LEGACY)).await;
    publish_one(&db, "bob", "dev1", "ref-pq", Some(CIPHERSUITE_PQ)).await;
    assert_eq!(stored_suite(&db, "ref-pq").await, CIPHERSUITE_PQ);
    let conn = db.conn().await.unwrap();

    match apply_claim_key_package(&conn, Some("alice"), &device_body("bob", "dev1")).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-pq"),
        ClaimOutcome::NoKeyPackage => panic!("the PQ pool must serve a suite-less claim"),
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
    }

    match apply_claim_key_package(&conn, Some("alice"), &device_body_in_suite("bob", "dev1", CIPHERSUITE_LEGACY))
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, "ref-legacy"),
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

    publish_one(&db, "bob", "dev1", "ref-pq", Some(CIPHERSUITE_PQ)).await;
    assert_eq!(
        pq_capable(&db, "bob", "dev1").await,
        0,
        "a PQ publish must no longer touch the retired flag"
    );

    let conn = db.conn().await.unwrap();
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

/// A rotation of one suite leaves the other pool intact, for code-point migrations.
#[tokio::test(flavor = "multi_thread")]
async fn rotating_one_suite_leaves_the_other_pool_intact() {
    let db = fresh_db().await;
    insert_device(&db, "bob", "dev1").await;

    let conn = db.conn().await.unwrap();
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

    assert_eq!(stored_suite(&db, "h1").await, CIPHERSUITE_PQ);
    let conn = db.conn().await.unwrap();
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
