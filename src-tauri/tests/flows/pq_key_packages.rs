//! Key packages, end to end through the real command pipeline.
//!
//! #454 P2 published TWO KeyPackage pools on login — classic and hybrid — and
//! had the DS derive `user_device.pq_capable` from whichever actually landed,
//! because a fleet mid-upgrade contained devices that could serve only one of
//! them. #669 retired the classic suite: there is one pool, one suite, and no
//! capability flag.
//!
//! This exercises that through `sign_up` → `initialize_identity` →
//! `ensure_mls_key_package` against the real in-process Delivery Service, and
//! asserts the two things the collapse must not break: the pool is published in
//! the current suite, and a claim that names no suite draws from it rather than
//! from a pool nobody publishes any more.

use crate::harness::{wipe, world, TestClient};
use pollis_delivery::devices::{
    apply_claim_key_package, ClaimKeyPackageBody, ClaimOutcome, CIPHERSUITE_PQ,
};
use serial_test::serial;

/// MLS code point `0x0001` — the suite Pollis shipped before #454, retired by
/// #669. Named here only to assert that nothing publishes into it any more.
const CIPHERSUITE_LEGACY: i64 = 0x0001;

/// This user's (single) device id, read from the row the bootstrap registered.
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

/// Count this device's unclaimed key packages in one suite.
async fn unclaimed_count(user_id: &str, device_id: &str, suite: i64) -> i64 {
    let conn = world().await.remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package \
             WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 AND ciphersuite = ?3",
            libsql::params![user_id.to_string(), device_id.to_string(), suite],
        )
        .await
        .expect("kp count");
    rows.next().await.expect("rows").expect("count row").get::<i64>(0).expect("count")
}

/// The retired `user_device.pq_capable` flag. Still a column (dropping it needs
/// a multi-release dance); no longer written by anything.
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

/// Login publishes ONE pool, in the post-quantum suite, and a suite-less claim
/// finds it.
///
/// The failure this guards against is silent and total: a device whose pool
/// lands in a suite nobody claims from is a device that can never be added to a
/// group, and every symptom of that shows up somewhere else entirely (an invite
/// that "does nothing", a member missing from a tree). Counting both pools makes
/// the mistake visible here instead.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn login_publishes_one_pool_in_the_current_suite() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();
    let device_id = device_id_of(&user_id).await;

    // `ensure_mls_key_package` published TARGET (5) in the one suite.
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

    // The capability flag is retired: publishing must not derive it any more.
    assert_eq!(
        pq_capable(&device_id).await,
        0,
        "pq_capable is retired in place; no write path may still set it"
    );

    // A claim that names no suite means "the current suite" and must find the
    // pool. This is the wire-compatibility hinge: #454 read an absent field as
    // classic, and keeping that reading would have made every untagged claim
    // miss.
    let conn = world().await.remote.conn().await.expect("remote conn");
    let untagged_claim = ClaimKeyPackageBody {
        target_user_id: user_id.clone(),
        target_device_id: Some(device_id.clone()),
        ciphersuite: None,
    };
    match apply_claim_key_package(&conn, &untagged_claim).await.expect("claim") {
        ClaimOutcome::Claimed { .. } => {}
        ClaimOutcome::NoKeyPackage => {
            panic!("a suite-less claim must draw from the current suite's pool")
        }
    }

    assert_eq!(
        unclaimed_count(&user_id, &device_id, CIPHERSUITE_PQ).await,
        4,
        "the claim consumed exactly one package"
    );

    drop(alice);
}
