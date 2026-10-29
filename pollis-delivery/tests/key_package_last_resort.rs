//! #1571: the last-resort KeyPackage, against a real (local) libsql DB with the
//! real schema.

use pollis_delivery::account_reads::{has_last_resort_key_package, unclaimed_key_package_count};
use pollis_delivery::db::Db;
use pollis_delivery::devices::{
    apply_claim_key_package, apply_publish_key_packages, apply_replenish_key_packages,
    ClaimKeyPackageBody, ClaimOutcome, KeyPackageEntry, PublishKeyPackagesBody,
    ReplenishKeyPackagesBody, CIPHERSUITE_PQ,
};
use pollis_delivery::writes::WriteOutcome;

mod common;

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("kp-lr.db").await;
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

fn entry(ref_hash: &str, last_resort: bool) -> KeyPackageEntry {
    use base64::Engine as _;
    KeyPackageEntry {
        ref_hash: ref_hash.to_string(),
        key_package: base64::engine::general_purpose::STANDARD.encode(ref_hash.as_bytes()),
        ciphersuite: Some(CIPHERSUITE_PQ),
        last_resort,
    }
}

async fn publish(db: &Db, user: &str, device: &str, packages: Vec<KeyPackageEntry>) -> WriteOutcome {
    let body = PublishKeyPackagesBody {
        device_id: device.to_string(),
        packages,
        user_id: Some(user.to_string()),
    };
    apply_publish_key_packages(&db.conn().await.unwrap(), Some(user), &body)
        .await
        .unwrap()
}

async fn rotate(db: &Db, user: &str, device: &str, packages: Vec<KeyPackageEntry>) -> WriteOutcome {
    let body = ReplenishKeyPackagesBody {
        device_id: device.to_string(),
        packages,
        user_id: Some(user.to_string()),
    };
    apply_replenish_key_packages(&db.conn().await.unwrap(), Some(user), &body)
        .await
        .unwrap()
}

async fn claim(db: &Db, claimer: &str, user: &str, device: &str) -> Option<String> {
    let body = ClaimKeyPackageBody {
        target_user_id: user.to_string(),
        target_device_id: Some(device.to_string()),
        ciphersuite: None,
    };
    match apply_claim_key_package(&db.conn().await.unwrap(), Some(claimer), &body)
        .await
        .unwrap()
    {
        ClaimOutcome::Claimed { ref_hash, key_package } => {
            assert_eq!(key_package, ref_hash.as_bytes(), "the claimed row's own bytes");
            Some(ref_hash)
        }
        ClaimOutcome::NoKeyPackage => None,
        other => panic!("the claim gate refused a test claim: {other:?}"),
    }
}

async fn refs(db: &Db, sql_where: &str) -> Vec<String> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            &format!("SELECT ref_hash FROM mls_key_package WHERE {sql_where} ORDER BY ref_hash"),
            (),
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(0).unwrap());
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pool_is_spent_first_and_the_last_resort_package_is_never_consumed() {
    let db = fresh_db().await;
    assert!(matches!(
        publish(&db, "bob", "d1", vec![entry("r1", false), entry("r2", false), entry("lr", true)]).await,
        WriteOutcome::Ok
    ));

    let mut got = vec![
        claim(&db, "alice", "bob", "d1").await.unwrap(),
        claim(&db, "alice", "bob", "d1").await.unwrap(),
    ];
    got.sort();
    assert_eq!(got, vec!["r1", "r2"], "regular packages go first");

    for _ in 0..4 {
        assert_eq!(claim(&db, "alice", "bob", "d1").await.as_deref(), Some("lr"));
    }
    assert_eq!(refs(&db, "last_resort = 1 AND claimed = 0").await, vec!["lr"]);

    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM mls_key_package_claim WHERE target_device_id = 'd1'", ())
        .await
        .unwrap();
    assert_eq!(rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(), 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_update_can_mark_a_last_resort_package_claimed() {
    let db = fresh_db().await;
    publish(&db, "bob", "d1", vec![entry("r1", false), entry("lr", true)]).await;
    // A pre-#1571 DS's bare claim UPDATE gets the bytes; the row stays claimable.
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "UPDATE mls_key_package SET claimed = 1 WHERE ref_hash = 'lr' RETURNING ref_hash",
            (),
        )
        .await
        .unwrap();
    assert_eq!(rows.next().await.unwrap().unwrap().get::<String>(0).unwrap(), "lr");
    drop(rows);
    conn.execute("UPDATE mls_key_package SET claimed = 1", ()).await.unwrap();
    assert_eq!(refs(&db, "claimed = 0").await, vec!["lr"], "only the regular row was spent");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_refill_count_ignores_it_and_its_presence_is_reported() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    publish(&db, "bob", "d1", vec![entry("r1", false), entry("r2", false)]).await;
    assert_eq!(unclaimed_key_package_count(&conn, "bob", "d1", CIPHERSUITE_PQ).await.unwrap(), 2);
    assert!(!has_last_resort_key_package(&conn, "bob", "d1", CIPHERSUITE_PQ).await.unwrap());

    publish(&db, "bob", "d1", vec![entry("lr", true)]).await;
    assert_eq!(
        unclaimed_key_package_count(&conn, "bob", "d1", CIPHERSUITE_PQ).await.unwrap(),
        2,
        "the last-resort package is not part of the pool a refill tops up"
    );
    assert!(has_last_resort_key_package(&conn, "bob", "d1", CIPHERSUITE_PQ).await.unwrap());
    assert!(!has_last_resort_key_package(&conn, "bob", "d2", CIPHERSUITE_PQ).await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spent_pool_with_a_last_resort_package_stays_claimable() {
    let db = fresh_db().await;
    publish(&db, "bob", "d1", vec![entry("r1", false), entry("lr", true)]).await;
    publish(&db, "bob", "d2", vec![entry("s1", false)]).await;
    claim(&db, "alice", "bob", "d1").await.unwrap();
    claim(&db, "alice", "bob", "d2").await.unwrap();
    // The claimable-devices predicate (`claimed = 0`).
    let claimable = refs(&db, "user_id = 'bob' AND claimed = 0").await;
    assert_eq!(claimable, vec!["lr"], "d1 is still addable; d2 (no last-resort) is not");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_per_device_per_suite_a_new_one_replaces_the_old_and_two_are_refused() {
    let db = fresh_db().await;
    publish(&db, "bob", "d1", vec![entry("lr-old", true)]).await;
    publish(&db, "bob", "d1", vec![entry("lr-new", true)]).await;
    assert_eq!(refs(&db, "last_resort = 1").await, vec!["lr-new"]);

    match publish(&db, "bob", "d1", vec![entry("a", true), entry("b", true)]).await {
        WriteOutcome::Invalid(_) => {}
        other => panic!("two last-resort packages in one suite must be refused, got {other:?}"),
    }
    assert_eq!(refs(&db, "last_resort = 1").await, vec!["lr-new"], "nothing written");

    let conn = db.conn().await.unwrap();
    let dup = conn
        .execute(
            "INSERT INTO mls_key_package (ref_hash, user_id, key_package, device_id, ciphersuite, last_resort) \
             VALUES ('x', 'bob', X'00', 'd1', ?1, 1)",
            libsql::params![CIPHERSUITE_PQ],
        )
        .await;
    assert!(dup.is_err(), "the partial unique index admits one per device per suite");

    publish(&db, "bob", "d2", vec![entry("lr-d2", true)]).await;
    assert_eq!(refs(&db, "last_resort = 1").await, vec!["lr-d2", "lr-new"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rotation_replaces_it_and_an_old_clients_rotation_leaves_a_regular_pool_only() {
    let db = fresh_db().await;
    rotate(&db, "bob", "d1", vec![entry("r1", false), entry("lr1", true)]).await;
    rotate(&db, "bob", "d1", vec![entry("r2", false), entry("lr2", true)]).await;
    assert_eq!(refs(&db, "device_id = 'd1'").await, vec!["lr2", "r2"]);

    // A shipped client's rotation leaves a regular pool only.
    rotate(&db, "bob", "d1", vec![entry("r3", false)]).await;
    assert_eq!(refs(&db, "device_id = 'd1'").await, vec!["r3"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_predates_the_field_is_unchanged() {
    let json = r#"{"device_id":"d1","packages":[{"ref_hash":"r1","key_package":"cjE=","ciphersuite":82}],"user_id":"bob"}"#;
    let body: PublishKeyPackagesBody = serde_json::from_str(json).unwrap();
    assert!(!body.packages[0].last_resort);
    let wire = serde_json::to_string(&entry("r1", false)).unwrap();
    assert!(!wire.contains("last_resort"), "got {wire}");

    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    apply_publish_key_packages(&conn, Some("bob"), &body).await.unwrap();
    assert_eq!(refs(&db, "last_resort = 0").await, vec!["r1"]);
    assert_eq!(claim(&db, "alice", "bob", "d1").await.as_deref(), Some("r1"));
    assert_eq!(claim(&db, "alice", "bob", "d1").await, None, "and an empty pool is empty");
}
