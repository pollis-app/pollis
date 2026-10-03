//! Abuse reports (#1213), end to end through the real command pipeline.
//!
//! What a user can do: report someone with a reason, optionally block them in
//! the same step. What the server learns: ids and the reason, never a word of
//! any message. And the operator's one lever: a suspended account's devices
//! stop authenticating, so it cannot write anything.

use crate::harness::{wipe, writable_remote, TestClient};
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

    // Invalid reports are refused on the client before anything is sent.
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

    // Any signed write will do; a report is the one this ticket added. The
    // device key cache may hold bob's key for up to its TTL, so allow for it
    // by retrying past it rather than asserting on the first call.
    let mut refused = false;
    for _ in 0..40 {
        let r = bob
            .invoke_try(
                "report_user",
                serde_json::json!({ "reportedId": alice_p.id, "reason": "other" }),
            )
            .await;
        if r.is_err() {
            refused = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert!(refused, "a suspended account was still able to write");
}
