//! Email-change OTP endpoints (Goal B #419 final piece), driven through the real
//! axum router with `tower::oneshot` against a local libsql DB.
//!
//! Unlike the signup bootstrap (OTP-session-gated), these are DEVICE-SIGNED: the
//! user is already authenticated, so each request carries the four `X-Pollis-*`
//! signature headers and is verified against the seeded `user_device`
//! `mls_signature_pub`. The OTP only proves control of the NEW mailbox.
//!
//! Coverage: the happy path, OTP wrong-code lockout, and the cross-user binding
//! (a different signed user can't consume someone else's pending change).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use http_body_util::BodyExt as _;
use pollis_delivery::auth::canonical_message;
use pollis_delivery::db::Db;
use pollis_delivery::otp::OtpConfig;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

const DEV_CODE: &str = "424242";

const SCHEMA: &str = "\
CREATE TABLE users (\
  id TEXT PRIMARY KEY,\
  email TEXT NOT NULL UNIQUE,\
  username TEXT NOT NULL UNIQUE,\
  account_id_pub BLOB,\
  identity_version INTEGER NOT NULL DEFAULT 1\
);\
CREATE TABLE user_device (\
  device_id   TEXT PRIMARY KEY,\
  user_id     TEXT NOT NULL,\
  device_name TEXT,\
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),\
  last_seen   TEXT NOT NULL DEFAULT (datetime('now')),\
  device_cert BLOB,\
  cert_issued_at TEXT,\
  cert_identity_version INTEGER,\
  mls_signature_pub BLOB,\
  revoked_at TEXT\
);";

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn gen_signing_key() -> SigningKey {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    SigningKey::from_bytes(&secret)
}

async fn fresh_db() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ec.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    db.conn().unwrap().execute_batch(SCHEMA).await.expect("schema");
    Arc::new(db)
}

/// State with auth ENFORCED (these endpoints are device-signed) and a fixed
/// DEV_OTP so the email-change code is deterministic with no email send.
fn dev_state(db: Arc<Db>) -> AppState {
    AppState::new(db, true).with_otp_config(OtpConfig {
        resend_api_key: None,
        dev_otp: Some(DEV_CODE.to_string()),
        ttl_secs: 600,
        session_ttl_secs: 600,
        resend_throttle_secs: 0,
        max_attempts: 5,
    })
}

/// Seed a `users` row + a live device with `vk` as its signing key.
async fn seed_user(db: &Db, user_id: &str, email: &str, device_id: &str, vk: &VerifyingKey) {
    let conn = db.conn().unwrap();
    conn.execute(
        "INSERT INTO users (id, email, username) VALUES (?1, ?2, ?3)",
        libsql::params![user_id, email, format!("{user_id}_name")],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub) VALUES (?1, ?2, ?3)",
        libsql::params![device_id, user_id, vk.to_bytes().to_vec()],
    )
    .await
    .unwrap();
}

async fn email_of(db: &Db, user_id: &str) -> String {
    let conn = db.conn().unwrap();
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

/// Build a device-signed POST for `path` with `body`.
fn signed_request(
    path: &str,
    user_id: &str,
    device_id: &str,
    signing_key: &SigningKey,
    body: &[u8],
) -> Request<Body> {
    let ts = now();
    let msg = canonical_message("POST", path, ts, body);
    let sig = b64(&signing_key.sign(&msg).to_bytes());
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
    sk: &SigningKey,
    body: serde_json::Value,
) -> StatusCode {
    let bytes = serde_json::to_vec(&body).unwrap();
    let req = signed_request(path, user_id, device_id, sk, &bytes);
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let _ = resp.into_body().collect().await;
    status
}

// ── 1. Happy path ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn email_change_happy_path() {
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
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "correct code should swap the email");
    assert_eq!(email_of(&db, "alice").await, new_email, "users.email must be updated");
}

// ── 2. Wrong-code lockout ─────────────────────────────────────────────────────

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

    // 5 wrong guesses → 401 invalid.
    for _ in 0..5 {
        let s = send_signed(
            &state,
            "/v1/auth/verify-email-change",
            "alice",
            "dev-a",
            &sk,
            serde_json::json!({ "new_email": new_email, "code": "000000" }),
        )
        .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    // 6th wrong → locked out (429) and the code is deleted.
    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email, "code": "000000" }),
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);

    // The correct code no longer works, and the email was never changed.
    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &sk,
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE }),
    )
    .await;
    assert_ne!(s, StatusCode::OK, "a locked-out code must not succeed");
    assert_eq!(email_of(&db, "alice").await, "alice@x.com", "email must be unchanged");
}

// ── 3. Cross-user binding ─────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn email_change_rejects_cross_user() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));
    let alice_sk = gen_signing_key();
    let bob_sk = gen_signing_key();
    seed_user(&db, "alice", "alice@x.com", "dev-a", &alice_sk.verifying_key()).await;
    seed_user(&db, "bob", "bob@x.com", "dev-b", &bob_sk.verifying_key()).await;

    let new_email = "alice-new@x.com";

    // Alice requests the change → DS records (alice → new_email).
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

    // Bob — a DIFFERENT signed user — tries to consume it WITH the correct code.
    // The requester binding rejects it (403) before the OTP is even checked, so
    // alice's code stays valid.
    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "bob",
        "dev-b",
        &bob_sk,
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a different signed user must be refused");
    // Neither account's email moved.
    assert_eq!(email_of(&db, "alice").await, "alice@x.com");
    assert_eq!(email_of(&db, "bob").await, "bob@x.com");

    // Alice's correct code still works — bob's attempt didn't burn it.
    let s = send_signed(
        &state,
        "/v1/auth/verify-email-change",
        "alice",
        "dev-a",
        &alice_sk,
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(email_of(&db, "alice").await, new_email);
    assert_eq!(email_of(&db, "bob").await, "bob@x.com", "bob untouched");
}
