//! #454 P2 — hybrid key packages, end to end through the real command pipeline.
//!
//! A hybrid-capable device (every P2 build) publishes TWO KeyPackage pools on
//! login — `CS_CLASSIC` and `CS_HYBRID`, both through `PollisProvider` since
//! #668 collapsed the two crypto backends into one — and advertises
//! `user_device.pq_capable = 1` once the
//! hybrid pool has actually landed. This exercises that through
//! `sign_up` → `initialize_identity` → `ensure_mls_key_package` against the real
//! in-process Delivery Service, then asserts the mixed-fleet invariant: an old
//! app (no suite field) claiming a classic KP still works and never touches the
//! hybrid pool. No group goes hybrid in P2 — that is P3/P4.

use crate::harness::{wipe, world, TestClient};
use pollis_delivery::devices::{
    apply_claim_key_package, ClaimKeyPackageBody, ClaimOutcome, CIPHERSUITE_CLASSIC,
    CIPHERSUITE_HYBRID,
};
use serial_test::serial;

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

/// A P2 device advertises both pools and flips `pq_capable` on — and a classic
/// claim (the mixed-fleet, no-migration add) still works after it has both pools.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn hybrid_device_advertises_both_pools_and_pq_capable() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();
    let device_id = device_id_of(&user_id).await;

    // `ensure_mls_key_package` published TARGET (5) of EACH suite on login.
    assert_eq!(
        unclaimed_count(&user_id, &device_id, CIPHERSUITE_CLASSIC).await,
        5,
        "the classic pool must be published"
    );
    assert_eq!(
        unclaimed_count(&user_id, &device_id, CIPHERSUITE_HYBRID).await,
        5,
        "the hybrid pool must be published alongside it"
    );

    // The capability flag flipped ON — and only because the hybrid pool actually
    // landed (the DS derives it from the published packages, not a client claim).
    assert_eq!(pq_capable(&device_id).await, 1, "pq_capable must be set once the hybrid pool is published");

    // Mixed fleet: an OLD app claims a CLASSIC key package (no suite field on the
    // wire → classic default). This is the "no-migration" add path and must still
    // work unchanged now that the device also carries a hybrid pool…
    let conn = world().await.remote.conn().await.expect("remote conn");
    let classic_claim = ClaimKeyPackageBody {
        target_user_id: user_id.clone(),
        target_device_id: Some(device_id.clone()),
        ciphersuite: None,
    };
    match apply_claim_key_package(&conn, &classic_claim).await.expect("claim") {
        ClaimOutcome::Claimed { .. } => {}
        ClaimOutcome::NoKeyPackage => {
            panic!("an old-app classic claim must still succeed when both pools are present")
        }
    }

    // …and it drew from the classic pool ONLY — the hybrid pool is untouched.
    assert_eq!(
        unclaimed_count(&user_id, &device_id, CIPHERSUITE_CLASSIC).await,
        4,
        "the classic claim consumed exactly one classic KP"
    );
    assert_eq!(
        unclaimed_count(&user_id, &device_id, CIPHERSUITE_HYBRID).await,
        5,
        "a classic claim must never touch the hybrid pool"
    );

    drop(alice);
}
