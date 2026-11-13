//! #1879: a mailbox's sign-in code limits hold across a DS restart.
//!
//! A "restart" here is what the container gives a new process: a fresh
//! `AppState` (empty memory) over the same database, with the mailbox key
//! derived again from the same secret.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::db::Db;
use pollis_delivery::otp::{MailboxKey, OtpConfig, OtpScope, OtpStore, PrepareOutcome};
use pollis_delivery::ratelimit::RateLimitConfig;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

const DEV_CODE: &str = "123456";
const SECRET: &str = "test-deployment-secret";
const EMAIL: &str = "alice@example.com";

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("otp.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db
}

/// The config one process builds at boot: the key is derived, not shared.
fn boot_config() -> OtpConfig {
    OtpConfig {
        dev_otp: Some(DEV_CODE.to_string()),
        resend_throttle_secs: 0,
        mailbox_key: MailboxKey::derive(SECRET),
        ..OtpConfig::default()
    }
}

/// One DS process: fresh memory over `db`.
fn boot(db: Arc<Db>) -> AppState {
    AppState::new(db, true)
        .with_otp_config(boot_config())
        // Every request comes from one test client; this file is about the
        // per-mailbox limits, not the per-IP ones.
        .with_ratelimit_config(RateLimitConfig {
            request_otp_max: 1_000,
            verify_otp_max: 1_000,
            ..RateLimitConfig::default()
        })
}

async fn post(state: &AppState, path: &str, body: serde_json::Value) -> StatusCode {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let _ = resp.into_body().collect().await.unwrap();
    status
}

async fn request_code(state: &AppState) -> StatusCode {
    post(state, "/v1/auth/request-otp", serde_json::json!({ "email": EMAIL })).await
}

async fn verify(state: &AppState, code: &str) -> StatusCode {
    post(
        state,
        "/v1/auth/verify-otp",
        serde_json::json!({ "email": EMAIL, "code": code, "device_id": "dev-1" }),
    )
    .await
}

#[tokio::test]
async fn a_lockout_holds_across_a_restart() {
    let db = fresh_db().await;
    let before = boot(db.arc());
    assert_eq!(request_code(&before).await, StatusCode::OK);
    for _ in 0..5 {
        assert_eq!(verify(&before, "000000").await, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(verify(&before, "000000").await, StatusCode::TOO_MANY_REQUESTS);

    let after = boot(db.arc());
    // A new request is answered as always but issues no code.
    assert_eq!(request_code(&after).await, StatusCode::OK);
    assert_eq!(verify(&after, DEV_CODE).await, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn the_guess_count_holds_across_a_restart() {
    let db = fresh_db().await;
    let before = boot(db.arc());
    assert_eq!(request_code(&before).await, StatusCode::OK);
    for _ in 0..5 {
        assert_eq!(verify(&before, "000000").await, StatusCode::UNAUTHORIZED);
    }

    let after = boot(db.arc());
    assert_eq!(request_code(&after).await, StatusCode::OK);
    assert_eq!(
        verify(&after, "000000").await,
        StatusCode::TOO_MANY_REQUESTS,
        "the sixth wrong guess locks the mailbox, whichever process saw the first five"
    );
    assert_eq!(verify(&after, DEV_CODE).await, StatusCode::TOO_MANY_REQUESTS);
}

/// Codes are per process: after a restart the user asks again, and nothing
/// else about the mailbox has changed.
#[tokio::test]
async fn after_a_restart_a_new_code_signs_in() {
    let db = fresh_db().await;
    let before = boot(db.arc());
    assert_eq!(request_code(&before).await, StatusCode::OK);

    let after = boot(db.arc());
    assert_eq!(verify(&after, DEV_CODE).await, StatusCode::UNAUTHORIZED);
    assert_eq!(request_code(&after).await, StatusCode::OK);
    assert_eq!(verify(&after, DEV_CODE).await, StatusCode::OK);
}

#[tokio::test]
async fn the_resend_throttle_holds_across_a_restart() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let cfg = OtpConfig { resend_throttle_secs: 30, ..boot_config() };

    let before = OtpStore::new(OtpScope::SignIn);
    assert!(matches!(
        before.prepare(&conn, EMAIL, DEV_CODE, &cfg, 1_000).await.unwrap(),
        PrepareOutcome::Send(_)
    ));

    let after = OtpStore::new(OtpScope::SignIn);
    let cfg = OtpConfig { mailbox_key: MailboxKey::derive(SECRET), ..cfg };
    assert_eq!(
        after.prepare(&conn, EMAIL, DEV_CODE, &cfg, 1_010).await.unwrap(),
        PrepareOutcome::Refused
    );
    assert!(matches!(
        after.prepare(&conn, EMAIL, DEV_CODE, &cfg, 1_030).await.unwrap(),
        PrepareOutcome::Send(_)
    ));
}

#[tokio::test]
async fn the_send_budget_holds_across_a_restart() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let cfg = boot_config();

    let before = OtpStore::new(OtpScope::SignIn);
    for i in 0..cfg.max_sends_per_window {
        assert!(matches!(
            before.prepare(&conn, EMAIL, DEV_CODE, &cfg, 1_000 + u64::from(i)).await.unwrap(),
            PrepareOutcome::Send(_)
        ));
    }

    let after = OtpStore::new(OtpScope::SignIn);
    let cfg = boot_config();
    assert_eq!(
        after.prepare(&conn, EMAIL, DEV_CODE, &cfg, 1_100).await.unwrap(),
        PrepareOutcome::Refused
    );
    // The window still rolls over.
    assert!(matches!(
        after.prepare(&conn, EMAIL, DEV_CODE, &cfg, 1_000 + cfg.ttl_secs).await.unwrap(),
        PrepareOutcome::Send(_)
    ));
}

/// After the sign-in flows above, the table holds hashes only.
#[tokio::test]
async fn the_table_never_holds_an_address() {
    let db = fresh_db().await;
    let state = boot(db.arc());
    request_code(&state).await;
    verify(&state, "000000").await;

    let conn = db.conn().await.unwrap();
    let mut rows = conn.query("SELECT * FROM otp_mailbox", ()).await.unwrap();
    let mut n = 0;
    while let Some(row) = rows.next().await.unwrap() {
        n += 1;
        for i in 0..row.column_count() {
            let v = format!("{:?}", row.get_value(i).unwrap());
            assert!(!v.contains('@') && !v.contains("alice"), "column {i} holds {v}");
        }
    }
    assert_eq!(n, 1);
}
