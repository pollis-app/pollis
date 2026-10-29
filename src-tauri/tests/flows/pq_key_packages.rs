//! Key packages (#669): one pool, one suite, no capability flag.

use crate::harness::{wipe, world, TestClient};
use pollis_delivery::devices::{
    apply_claim_key_package, ClaimKeyPackageBody, ClaimOutcome, CIPHERSUITE_PQ,
};
use serial_test::serial;

/// The pre-#454 suite, retired by #669.
const CIPHERSUITE_LEGACY: i64 = 0x0001;

async fn device_id_of(user_id: &str) -> String {
    let conn = world().await.remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT device_id FROM user_device WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await
        .expect("user_device select");
    rows.next()
        .await
        .expect("rows")
        .expect("a registered device")
        .get::<String>(0)
        .expect("device_id")
}

async fn unclaimed_count(user_id: &str, device_id: &str, suite: i64) -> i64 {
    let conn = world().await.remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package \
             WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 AND ciphersuite = ?3 \
               AND last_resort = 0",
            libsql::params![user_id.to_string(), device_id.to_string(), suite],
        )
        .await
        .expect("kp count");
    rows.next().await.expect("rows").expect("count row").get::<i64>(0).expect("count")
}

/// Retired `user_device.pq_capable`; still a column, never written.
async fn pq_capable(device_id: &str) -> i64 {
    let conn = world().await.remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT pq_capable FROM user_device WHERE device_id = ?1",
            libsql::params![device_id.to_string()],
        )
        .await
        .expect("pq_capable select");
    rows.next().await.expect("rows").expect("device row").get::<i64>(0).expect("pq_capable")
}

/// A pool in a suite nobody claims from means a device that can never be added.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn login_publishes_one_pool_in_the_current_suite() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();
    let device_id = device_id_of(&user_id).await;

    assert_eq!(
        unclaimed_count(&user_id, &device_id, CIPHERSUITE_PQ).await,
        5,
        "the post-quantum pool must be published"
    );
    assert_eq!(
        unclaimed_count(&user_id, &device_id, CIPHERSUITE_LEGACY).await,
        0,
        "#669 retired the classic pool — nothing may still publish into it"
    );

    assert_eq!(
        pq_capable(&device_id).await,
        0,
        "pq_capable is retired in place; no write path may still set it"
    );

    // A suite-less claim means the current suite (#454 read it as classic).
    let conn = world().await.remote.conn().await.expect("remote conn");
    // Claims require a shared desired roster (#1161 L3), so state it here.
    for member in [&user_id, &"alice".to_string()] {
        conn.execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id) VALUES ('pq-kp-grp', ?1)",
            libsql::params![member.as_str()],
        )
        .await
        .expect("seed the claimer's shared roster");
    }
    let untagged_claim = ClaimKeyPackageBody {
        target_user_id: user_id.clone(),
        target_device_id: Some(device_id.clone()),
        ciphersuite: None,
    };
    match apply_claim_key_package(&conn, Some("alice"), &untagged_claim).await.expect("claim") {
        ClaimOutcome::Claimed { .. } => {}
        ClaimOutcome::NoKeyPackage => {
            panic!("a suite-less claim must draw from the current suite's pool")
        }
        other @ (ClaimOutcome::Forbidden | ClaimOutcome::RateLimited) => {
            panic!("the claim gate refused an unrelated test claim: {other:?}")
        }
    }

    assert_eq!(
        unclaimed_count(&user_id, &device_id, CIPHERSUITE_PQ).await,
        4,
        "the claim consumed exactly one package"
    );

    drop(alice);
}
