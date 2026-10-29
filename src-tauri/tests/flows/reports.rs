//! Abuse reports (#1213), end to end through the real command pipeline.

use crate::harness::{expire_ds_device_keys, wipe, writable_remote, TestClient};
use serial_test::serial;

async fn count(sql: &str, a: &str, b: &str) -> i64 {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let mut rows = conn
        .query(sql, libsql::params![a.to_string(), b.to_string()])
        .await
        .expect("query");
    rows.next().await.expect("row").expect("one row").get(0).expect("count")
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_report_reaches_the_server_with_ids_only_and_can_also_block() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    alice
        .invoke_json(
            "report_user",
            serde_json::json!({ "reportedId": bob_p.id, "reason": "spam", "alsoBlock": false }),
        )
        .await;
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM user_report WHERE reporter_id = ?1 AND reported_id = ?2 AND reason = 'spam'",
            &alice_p.id,
            &bob_p.id
        )
        .await,
        1
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM user_block WHERE blocker_id = ?1 AND blocked_id = ?2", &alice_p.id, &bob_p.id).await,
        0,
        "a report alone must not block"
    );

    alice
        .invoke_json(
            "report_user",
            serde_json::json!({ "reportedId": bob_p.id, "reason": "harassment", "alsoBlock": true }),
        )
        .await;
    assert_eq!(
        count("SELECT COUNT(*) FROM user_block WHERE blocker_id = ?1 AND blocked_id = ?2", &alice_p.id, &bob_p.id).await,
        1,
        "report-and-block must block"
    );

    let me = alice
        .invoke_try("report_user", serde_json::json!({ "reportedId": alice_p.id, "reason": "spam" }))
        .await;
    assert!(me.is_err(), "reporting yourself must fail");
    let bad = alice
        .invoke_try("report_user", serde_json::json!({ "reportedId": bob_p.id, "reason": "rude" }))
        .await;
    assert!(bad.is_err(), "a reason outside the set must fail");
    assert_eq!(
        count("SELECT COUNT(*) FROM user_report WHERE reporter_id = ?1 AND reported_id != ?2", &alice_p.id, &bob_p.id).await,
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_suspended_account_cannot_write() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let remote = writable_remote().await;
    remote
        .conn()
        .await
        .expect("conn")
        .execute(
            "INSERT INTO account_suspension (user_id, reason) VALUES (?1, 'test')",
            libsql::params![bob_p.id.clone()],
        )
        .await
        .expect("suspend");

    // Suspension has no device-key cache eviction hook; it relies on the TTL, so expire it.
    expire_ds_device_keys(&bob_p.id);

    let r = bob
        .invoke_try(
            "report_user",
            serde_json::json!({ "reportedId": alice_p.id, "reason": "other" }),
        )
        .await;
    assert!(r.is_err(), "a suspended account was still able to write: {r:?}");
    assert_eq!(
        count("SELECT COUNT(*) FROM user_report WHERE reporter_id = ?1 AND reported_id = ?2", &bob_p.id, &alice_p.id).await,
        0,
        "the suspended account's refused report must not have landed"
    );
}
