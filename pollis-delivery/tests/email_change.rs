//! Email-change OTP endpoints (#419): device-signed; the OTP only proves the new mailbox.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey, VerifyingKey};
use http_body_util::BodyExt as _;
use pollis_delivery::auth::canonical_message;
use pollis_delivery::db::Db;
use pollis_delivery::otp::OtpConfig;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

mod common;

const DEV_CODE: &str = "424242";


fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn gen_signing_key() -> SigningKey<MlDsa44> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    SigningKey::<MlDsa44>::from_seed(&seed.into())
}

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("ec.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db
}

/// Auth enforced, with a fixed DEV_OTP so codes are deterministic.
fn dev_state(db: Arc<Db>) -> AppState {
    AppState::new(db, true).with_otp_config(OtpConfig {
        resend_api_key: None,
        dev_otp: Some(DEV_CODE.to_string()),
        ttl_secs: 600,
        session_ttl_secs: 600,
        resend_throttle_secs: 0,
        max_attempts: 5,
        ..OtpConfig::default()
    })
}

async fn seed_user(db: &Db, user_id: &str, email: &str, device_id: &str, vk: &VerifyingKey<MlDsa44>) {
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO users (id, email, username) VALUES (?1, ?2, ?3)",
        libsql::params![user_id, email, format!("{user_id}_name")],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES (?1, ?2, ?3)",
        libsql::params![device_id, user_id, vk.encode().to_vec()],
    )
    .await
    .unwrap();
}

async fn email_of(db: &Db, user_id: &str) -> String {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query("SELECT email FROM users WHERE id = ?1", libsql::params![user_id])
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<String>(0).unwrap()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn signed_request(
    path: &str,
    user_id: &str,
    device_id: &str,
    signing_key: &SigningKey<MlDsa44>,
    body: &[u8],
) -> Request<Body> {
    let ts = now();
    let msg = canonical_message("POST", path, ts, body);
    let sig = b64(&signing_key.sign(&msg).encode());
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("X-Pollis-User", user_id)
        .header("X-Pollis-Device", device_id)
        .header("X-Pollis-Timestamp", ts.to_string())
        .header("X-Pollis-Signature", sig)
        .body(Body::from(body.to_vec()))
        .unwrap()
}

async fn send_signed(
    state: &AppState,
    path: &str,
    user_id: &str,
    device_id: &str,
    sk: &SigningKey<MlDsa44>,
    body: serde_json::Value,
) -> StatusCode {
    let bytes = serde_json::to_vec(&body).unwrap();
    let req = signed_request(path, user_id, device_id, sk, &bytes);
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let _ = resp.into_body().collect().await;
    status
}

/// L3: the DS writes the audit row itself, naming the address left, so a client cannot suppress it.
#[tokio::test(flavor = "multi_thread")]
async fn a_completed_email_change_is_recorded_against_the_account() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));
    let sk = gen_signing_key();
    seed_user(&db, "alice", "alice@x.com", "dev-a", &sk.verifying_key()).await;
    let new_email = "alice-new@x.com";

    let s = send_signed(
        &state,
        "/v1/auth/request-email-change-otp",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "request-email-change-otp should always 200");
    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE, "current_code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "correct code should swap the email");
    assert_eq!(email_of(&db, "alice").await, new_email, "users.email must be updated");

    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT metadata FROM security_event \
             WHERE user_id = 'alice' AND kind = 'email_changed'",
            (),
        )
        .await
        .unwrap();
    let row = rows
        .next()
        .await
        .unwrap()
        .expect("a completed email change must leave an audit row");
    let metadata: String = row.get::<Option<String>>(0).unwrap().expect("metadata");
    let parsed: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(parsed["from"], "alice@x.com", "the row names the address left");
    assert_eq!(parsed["to"], new_email);
}

/// A refused change writes no audit row.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_email_change_records_nothing() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));
    let sk = gen_signing_key();
    seed_user(&db, "alice", "alice@x.com", "dev-a", &sk.verifying_key()).await;

    send_signed(
        &state,
        "/v1/auth/request-email-change-otp",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": "alice-new@x.com" }),
    )
    .await;
    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": "alice-new@x.com", "code": "000000", "current_code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM security_event WHERE user_id = 'alice'", ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 0);
    assert_eq!(email_of(&db, "alice").await, "alice@x.com");
}

/// #1161: a stolen unlocked device satisfies signature and new-address code; the old address must approve.
#[tokio::test(flavor = "multi_thread")]
async fn an_email_change_without_the_current_address_code_is_refused() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));
    let sk = gen_signing_key();
    seed_user(&db, "alice", "alice@x.com", "dev-a", &sk.verifying_key()).await;
    let new_email = "attacker-controlled@x.com";

    send_signed(
        &state,
        "/v1/auth/request-email-change-otp",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email }),
    )
    .await;

    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE }),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "a change that never proved the current address must be refused"
    );
    assert_eq!(email_of(&db, "alice").await, "alice@x.com", "the address must not move");

    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE, "current_code": "" }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(email_of(&db, "alice").await, "alice@x.com");

    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE, "current_code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(email_of(&db, "alice").await, new_email);
}

/// The current-address code is checked first, so wrong guesses don't burn the new address's budget.
#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_current_address_code_is_refused_and_spends_no_new_address_attempt() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));
    let sk = gen_signing_key();
    seed_user(&db, "alice", "alice@x.com", "dev-a", &sk.verifying_key()).await;
    let new_email = "alice-new@x.com";

    send_signed(
        &state,
        "/v1/auth/request-email-change-otp",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email }),
    )
    .await;

    for _ in 0..8 {
        let s = send_signed(
            &state,
            "/v1/auth/verify-email-change",
            "alice",
            "dev-a",
            &sk,
            serde_json::json!({
                "new_email": new_email,
                "code": DEV_CODE,
                "current_code": "000000",
            }),
        )
        .await;
        assert_ne!(s, StatusCode::OK, "a wrong current-address code must never swap");
        assert_eq!(email_of(&db, "alice").await, "alice@x.com");
    }

    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM security_event WHERE user_id = 'alice'", ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn email_change_wrong_code_lockout() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));
    let sk = gen_signing_key();
    seed_user(&db, "alice", "alice@x.com", "dev-a", &sk.verifying_key()).await;

    let new_email = "alice-new@x.com";
    let s = send_signed(
        &state,
        "/v1/auth/request-email-change-otp",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    for _ in 0..5 {
        let s = send_signed(
            &state,
            "/v1/auth/verify-email-change",
            "alice",
            "dev-a",
            &sk,
            serde_json::json!({ "new_email": new_email, "code": "000000", "current_code": DEV_CODE }),
        )
        .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email, "code": "000000", "current_code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);

    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE, "current_code": DEV_CODE }),
    )
    .await;
    assert_ne!(s, StatusCode::OK, "a locked-out code must not succeed");
    assert_eq!(email_of(&db, "alice").await, "alice@x.com", "email must be unchanged");
}

#[tokio::test(flavor = "multi_thread")]
async fn email_change_rejects_cross_user() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));
    let alice_sk = gen_signing_key();
    let bob_sk = gen_signing_key();
    seed_user(&db, "alice", "alice@x.com", "dev-a", &alice_sk.verifying_key()).await;
    seed_user(&db, "bob", "bob@x.com", "dev-b", &bob_sk.verifying_key()).await;

    let new_email = "alice-new@x.com";

    let s = send_signed(
        &state,
        "/v1/auth/request-email-change-otp",
        "alice",
        "dev-a",
        &alice_sk,
        serde_json::json!({ "new_email": new_email }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // The requester binding rejects bob (403) before the OTP is checked, so alice's code stays valid.
    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "bob",
        "dev-b",
        &bob_sk,
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE, "current_code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a different signed user must be refused");
    assert_eq!(email_of(&db, "alice").await, "alice@x.com");
    assert_eq!(email_of(&db, "bob").await, "bob@x.com");

    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &alice_sk,
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE, "current_code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(email_of(&db, "alice").await, new_email);
    assert_eq!(email_of(&db, "bob").await, "bob@x.com", "bob untouched");
}
