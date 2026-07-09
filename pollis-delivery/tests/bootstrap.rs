//! Server-side OTP + bootstrap (Goal B #419, Slice 0), driven through the real
//! axum router with `tower::oneshot` against a local libsql DB.
//!
//! Slice 0 has no client seam yet, so these exercise the DS directly: the full
//! request-otp → verify-otp → establish-identity → register-device →
//! publish-device-cert happy path, plus the security properties — OTP lockout,
//! single-use, the establish-identity CAS (no overwrite), session→user binding,
//! and the cert-validity gate.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::BodyExt as _;
use pollis_delivery::db::Db;
use pollis_delivery::otp::OtpConfig;
use pollis_delivery::ratelimit::RateLimitConfig;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

// Self-contained schema (foreign_keys=OFF in Db::connect_local, so no FK
// targets needed). Columns match the Turso baseline + migration 5.
const SCHEMA: &str = "\
CREATE TABLE users (\
  id TEXT PRIMARY KEY,\
  email TEXT NOT NULL UNIQUE,\
  username TEXT NOT NULL UNIQUE,\
  phone TEXT,\
  avatar_url TEXT,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  account_id_pub BLOB,\
  identity_version INTEGER NOT NULL DEFAULT 1\
);\
CREATE TABLE account_key_log (\
  seq INTEGER PRIMARY KEY AUTOINCREMENT,\
  user_id TEXT NOT NULL,\
  account_id_pub BLOB NOT NULL,\
  identity_version INTEGER NOT NULL,\
  created_at TEXT NOT NULL DEFAULT (datetime('now'))\
);\
CREATE UNIQUE INDEX idx_account_key_log_user_version \
  ON account_key_log (user_id, identity_version);\
CREATE TABLE account_recovery (\
  user_id TEXT PRIMARY KEY,\
  identity_version INTEGER NOT NULL,\
  salt BLOB NOT NULL,\
  nonce BLOB NOT NULL,\
  wrapped_key BLOB NOT NULL,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))\
);\
CREATE TABLE user_device (\
  device_id TEXT PRIMARY KEY,\
  user_id TEXT NOT NULL,\
  device_name TEXT,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  last_seen TEXT NOT NULL DEFAULT (datetime('now')),\
  device_cert BLOB,\
  cert_issued_at TEXT,\
  cert_identity_version INTEGER,\
  mls_signature_pub BLOB,\
  revoked_at TEXT\
);\
CREATE TABLE conversation_watermark (\
  conversation_id TEXT NOT NULL,\
  user_id TEXT NOT NULL,\
  device_id TEXT NOT NULL,\
  last_fetched_at TEXT NOT NULL,\
  PRIMARY KEY (conversation_id, user_id, device_id)\
);\
CREATE TABLE channels (\
  id TEXT PRIMARY KEY, group_id TEXT NOT NULL, name TEXT NOT NULL\
);\
CREATE TABLE group_member (\
  group_id TEXT NOT NULL, user_id TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'member',\
  PRIMARY KEY (group_id, user_id)\
);\
CREATE TABLE dm_channel_member (\
  dm_channel_id TEXT NOT NULL, user_id TEXT NOT NULL,\
  PRIMARY KEY (dm_channel_id, user_id)\
);\
CREATE TABLE device_enrollment_request (\
  id TEXT PRIMARY KEY,\
  user_id TEXT NOT NULL,\
  new_device_id TEXT NOT NULL,\
  new_device_ephemeral_pub BLOB NOT NULL,\
  verification_code TEXT NOT NULL,\
  wrapped_account_key BLOB,\
  status TEXT NOT NULL,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  expires_at TEXT NOT NULL,\
  approved_by_device_id TEXT\
);";

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn gen_key() -> SigningKey {
    let mut s = [0u8; 32];
    OsRng.fill_bytes(&mut s);
    SigningKey::from_bytes(&s)
}

/// Build the canonical device-cert payload — byte-identical to the DS's
/// `cert.rs` and pollis-core's `device_cert_signed_payload`.
fn cert_payload(device_id: &str, mls_pub: &[u8], version: u32, issued_at: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"pollis-device-cert-v1\x00");
    out.push(device_id.len() as u8);
    out.extend_from_slice(device_id.as_bytes());
    out.push(mls_pub.len() as u8);
    out.extend_from_slice(mls_pub);
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&issued_at.to_be_bytes());
    out
}

async fn fresh_db() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ds.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    db.conn().unwrap().execute_batch(SCHEMA).await.expect("schema");
    Arc::new(db)
}

/// A state whose OTP `DEV_OTP` is the fixed code below — no email send, no
/// throttle. The OTP/session stores are Arc-shared, so cloning the state for each
/// request preserves them.
fn dev_state(db: Arc<Db>) -> AppState {
    AppState::new(db, false).with_otp_config(OtpConfig {
        resend_api_key: None,
        dev_otp: Some("123456".to_string()),
        ttl_secs: 600,
        session_ttl_secs: 600,
        resend_throttle_secs: 0,
        max_attempts: 5,
    })
}

const DEV_CODE: &str = "123456";

async fn send(
    state: &AppState,
    path: &str,
    body: serde_json::Value,
    session: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(tok) = session {
        builder = builder.header("X-Pollis-Session", tok);
    }
    let req = builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = build_router_with_state(state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, val)
}

/// Like [`send`], but stamps a `CF-Connecting-IP` header so the per-IP rate
/// limiter (#345) sees a specific client IP.
async fn send_ip(
    state: &AppState,
    path: &str,
    body: serde_json::Value,
    ip: &str,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("CF-Connecting-IP", ip)
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, val)
}

// #345: request-otp is per-IP rate limited (email-bomb / mass-enumeration
// defense), and the throttle is scoped to the client IP — a different IP is
// unaffected, and a 429 leaks nothing about account existence.
#[tokio::test(flavor = "multi_thread")]
async fn request_otp_is_ip_rate_limited() {
    let db = fresh_db().await;
    let state = AppState::new(Arc::clone(&db), false)
        .with_otp_config(OtpConfig {
            resend_api_key: None,
            dev_otp: Some(DEV_CODE.to_string()),
            ttl_secs: 600,
            session_ttl_secs: 600,
            // No per-email throttle, so only the per-IP limit is under test.
            resend_throttle_secs: 0,
            max_attempts: 5,
        })
        .with_ratelimit_config(RateLimitConfig {
            request_otp_max: 2,
            request_otp_window_secs: 600,
            verify_otp_max: 30,
            verify_otp_window_secs: 600,
            write_max: 1200,
            write_window_secs: 60,
        });

    // First two requests from one IP pass; the third is throttled.
    for _ in 0..2 {
        let (s, _) =
            send_ip(&state, "/v1/auth/request-otp", serde_json::json!({ "email": "a@x.com" }), "203.0.113.1").await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, _) =
        send_ip(&state, "/v1/auth/request-otp", serde_json::json!({ "email": "a@x.com" }), "203.0.113.1").await;
    assert_eq!(
        s,
        StatusCode::TOO_MANY_REQUESTS,
        "the 3rd request from the same IP is throttled"
    );

    // A different client IP has its own budget — the limit is per-IP, not global.
    let (s, _) =
        send_ip(&state, "/v1/auth/request-otp", serde_json::json!({ "email": "a@x.com" }), "203.0.113.2").await;
    assert_eq!(s, StatusCode::OK, "a different IP is unaffected");
}

// #345: every DS response carries the baseline security headers — including the
// rate-limiter's own 429 (which proves the header middleware wraps the limiter).
#[tokio::test(flavor = "multi_thread")]
async fn security_headers_on_every_response_including_429() {
    let db = fresh_db().await;
    let state = AppState::new(Arc::clone(&db), false)
        .with_otp_config(OtpConfig {
            resend_api_key: None,
            dev_otp: Some(DEV_CODE.to_string()),
            ttl_secs: 600,
            session_ttl_secs: 600,
            resend_throttle_secs: 0,
            max_attempts: 5,
        })
        .with_ratelimit_config(RateLimitConfig {
            request_otp_max: 1,
            request_otp_window_secs: 600,
            verify_otp_max: 30,
            verify_otp_window_secs: 600,
            write_max: 1200,
            write_window_secs: 60,
        });

    async fn hit(state: &AppState, ip: &str) -> (StatusCode, axum::http::HeaderMap) {
        let req = Request::builder()
            .method("POST")
            .uri("/v1/auth/request-otp")
            .header("content-type", "application/json")
            .header("CF-Connecting-IP", ip)
            .body(Body::from(serde_json::to_vec(&serde_json::json!({ "email": "h@x.com" })).unwrap()))
            .unwrap();
        let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
        (resp.status(), resp.headers().clone())
    }

    let assert_headers = |h: &axum::http::HeaderMap| {
        assert_eq!(h.get("cache-control").unwrap(), "no-store");
        assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(h.get("referrer-policy").unwrap(), "no-referrer");
        assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    };

    // First request (200) carries the headers.
    let (s, h) = hit(&state, "198.51.100.5").await;
    assert_eq!(s, StatusCode::OK);
    assert_headers(&h);

    // Second request from the same IP is rate-limited (429) — and STILL carries the
    // headers, proving the header middleware wraps the rate limiter.
    let (s, h) = hit(&state, "198.51.100.5").await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_headers(&h);
}

/// Run request-otp + verify-otp for `email`/`device_id`, returning the verify-otp
/// JSON (carries `user_id` + `session_token`).
async fn login(state: &AppState, email: &str, device_id: &str) -> serde_json::Value {
    let (s, _) = send(state, "/v1/auth/request-otp", serde_json::json!({ "email": email }), None).await;
    assert_eq!(s, StatusCode::OK, "request-otp should always 200");
    let (s, body) = send(
        state,
        "/v1/auth/verify-otp",
        serde_json::json!({ "email": email, "code": DEV_CODE, "device_id": device_id }),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "verify-otp with correct code should 200: {body}");
    body
}

async fn account_pub(db: &Db, user_id: &str) -> Option<Vec<u8>> {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query("SELECT account_id_pub FROM users WHERE id = ?1", libsql::params![user_id])
        .await
        .unwrap();
    let row = rows.next().await.unwrap()?;
    row.get::<Option<Vec<u8>>>(0).unwrap()
}

// ── 1. Full happy path ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn full_bootstrap_happy_path() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));

    let device_id = "dev-1";
    let v = login(&state, "alice@example.com", device_id).await;
    assert_eq!(v["is_new_account"], serde_json::json!(true));
    assert_eq!(v["has_identity"], serde_json::json!(false));
    let user_id = v["user_id"].as_str().unwrap().to_string();
    let token = v["session_token"].as_str().unwrap().to_string();

    // establish-identity
    let account = gen_key();
    let account_pub_b = account.verifying_key().to_bytes();
    let (s, _) = send(
        &state,
        "/v1/auth/establish-identity",
        serde_json::json!({
            "account_id_pub": b64(&account_pub_b),
            "salt": b64(&[1u8; 32]),
            "nonce": b64(&[2u8; 12]),
            "wrapped_key": b64(&[3u8; 48]),
        }),
        Some(&token),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "establish-identity should succeed");
    assert_eq!(account_pub(&db, &user_id).await.unwrap(), account_pub_b.to_vec());

    // register-device
    let (s, _) = send(
        &state,
        "/v1/auth/register-device",
        serde_json::json!({ "device_id": device_id, "device_name": "Alice's laptop" }),
        Some(&token),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "register-device should succeed");

    // publish-device-cert (valid cert signed by the account key)
    let mut mls_pub = [0u8; 32];
    OsRng.fill_bytes(&mut mls_pub);
    let issued_at: u64 = 1_700_000_000;
    let cert = account.sign(&cert_payload(device_id, &mls_pub, 1, issued_at));
    let (s, _) = send(
        &state,
        "/v1/auth/publish-device-cert",
        serde_json::json!({
            "device_id": device_id,
            "device_cert": b64(&cert.to_bytes()),
            "cert_issued_at": issued_at as i64,
            "cert_identity_version": 1,
            "mls_signature_pub": b64(&mls_pub),
        }),
        Some(&token),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "publish-device-cert should succeed");

    // The pivot landed: mls_signature_pub is now set, and the session is spent.
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query("SELECT mls_signature_pub FROM user_device WHERE device_id = ?1", libsql::params![device_id])
        .await
        .unwrap();
    let stored: Vec<u8> = rows.next().await.unwrap().unwrap().get::<Option<Vec<u8>>>(0).unwrap().unwrap();
    assert_eq!(stored, mls_pub.to_vec());

    // Session invalidated on success — a replay is now unauthorized.
    let (s, _) = send(
        &state,
        "/v1/auth/register-device",
        serde_json::json!({ "device_id": device_id }),
        Some(&token),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "session must be spent after cert publish");
}

// ── 2. OTP lockout (the bug fix) ─────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn otp_lockout_after_six_wrong() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));

    let (s, _) = send(&state, "/v1/auth/request-otp", serde_json::json!({ "email": "b@x.com" }), None).await;
    assert_eq!(s, StatusCode::OK);

    // 5 wrong guesses → 401 invalid.
    for _ in 0..5 {
        let (s, _) = send(
            &state,
            "/v1/auth/verify-otp",
            serde_json::json!({ "email": "b@x.com", "code": "000000", "device_id": "d" }),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    // 6th wrong → locked out (429) and the code is deleted.
    let (s, _) = send(
        &state,
        "/v1/auth/verify-otp",
        serde_json::json!({ "email": "b@x.com", "code": "000000", "device_id": "d" }),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);

    // The correct code no longer works after lockout.
    let (s, _) = send(
        &state,
        "/v1/auth/verify-otp",
        serde_json::json!({ "email": "b@x.com", "code": DEV_CODE, "device_id": "d" }),
        None,
    )
    .await;
    assert_ne!(s, StatusCode::OK, "correct code must fail once locked out");
}

// ── 3. Single-use ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn otp_is_single_use() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));

    send(&state, "/v1/auth/request-otp", serde_json::json!({ "email": "c@x.com" }), None).await;
    let (s, _) = send(
        &state,
        "/v1/auth/verify-otp",
        serde_json::json!({ "email": "c@x.com", "code": DEV_CODE, "device_id": "d" }),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // Replaying the same correct code fails — it was consumed.
    let (s, _) = send(
        &state,
        "/v1/auth/verify-otp",
        serde_json::json!({ "email": "c@x.com", "code": DEV_CODE, "device_id": "d" }),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "a verified code can't be replayed");
}

// ── 4. establish-identity CAS — never overwrite ──────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn establish_identity_is_cas_no_overwrite() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));

    let v = login(&state, "d@x.com", "dev-d").await;
    let user_id = v["user_id"].as_str().unwrap().to_string();
    let token = v["session_token"].as_str().unwrap().to_string();

    let first = gen_key().verifying_key().to_bytes();
    let establish = |pubk: Vec<u8>, tok: String| {
        let state = state.clone();
        async move {
            send(
                &state,
                "/v1/auth/establish-identity",
                serde_json::json!({
                    "account_id_pub": b64(&pubk),
                    "salt": b64(&[1u8; 32]),
                    "nonce": b64(&[2u8; 12]),
                    "wrapped_key": b64(&[3u8; 48]),
                }),
                Some(&tok),
            )
            .await
        }
    };

    let (s, _) = establish(first.to_vec(), token.clone()).await;
    assert_eq!(s, StatusCode::OK);

    // A second establish (e.g. a re-login trying to replace the account key) must
    // 409 and leave account_id_pub unchanged.
    let second = gen_key().verifying_key().to_bytes();
    let (s, _) = establish(second.to_vec(), token.clone()).await;
    assert_eq!(s, StatusCode::CONFLICT, "second establish must conflict");
    assert_eq!(
        account_pub(&db, &user_id).await.unwrap(),
        first.to_vec(),
        "account_id_pub must be unchanged after the 409"
    );
}

// ── 5. Session→user binding ──────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn session_binds_user_and_device() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));

    // Two distinct accounts.
    let a = login(&state, "a2@x.com", "dev-a").await;
    let b = login(&state, "b2@x.com", "dev-b").await;
    let a_id = a["user_id"].as_str().unwrap().to_string();
    let b_id = b["user_id"].as_str().unwrap().to_string();
    let a_token = a["session_token"].as_str().unwrap().to_string();

    // establish-identity with A's token writes A's row only — there is no body
    // user_id to point at B, and the write is bound to the session user.
    let apub = gen_key().verifying_key().to_bytes();
    let (s, _) = send(
        &state,
        "/v1/auth/establish-identity",
        serde_json::json!({
            "account_id_pub": b64(&apub),
            "salt": b64(&[1u8; 32]),
            "nonce": b64(&[2u8; 12]),
            "wrapped_key": b64(&[3u8; 48]),
        }),
        Some(&a_token),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(account_pub(&db, &a_id).await.unwrap(), apub.to_vec());
    assert!(account_pub(&db, &b_id).await.is_none(), "B's identity must be untouched");

    // register-device with A's token but B's device_id → 403 (the session is
    // bound to A's device dev-a).
    let (s, _) = send(
        &state,
        "/v1/auth/register-device",
        serde_json::json!({ "device_id": "dev-b" }),
        Some(&a_token),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a token can only register its own device");
}

// ── 6. Cert-validity gate ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn publish_cert_rejects_wrong_account_key() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));

    let device_id = "dev-e";
    let v = login(&state, "e@x.com", device_id).await;
    let token = v["session_token"].as_str().unwrap().to_string();

    // Establish identity under the legit account key.
    let account = gen_key();
    send(
        &state,
        "/v1/auth/establish-identity",
        serde_json::json!({
            "account_id_pub": b64(&account.verifying_key().to_bytes()),
            "salt": b64(&[1u8; 32]),
            "nonce": b64(&[2u8; 12]),
            "wrapped_key": b64(&[3u8; 48]),
        }),
        Some(&token),
    )
    .await;
    send(
        &state,
        "/v1/auth/register-device",
        serde_json::json!({ "device_id": device_id }),
        Some(&token),
    )
    .await;

    let mut mls_pub = [0u8; 32];
    OsRng.fill_bytes(&mut mls_pub);
    let issued_at: u64 = 1_700_000_000;

    // A cert signed by an ATTACKER key (not the account key) must be rejected.
    let attacker = gen_key();
    let bad_cert = attacker.sign(&cert_payload(device_id, &mls_pub, 1, issued_at));
    let (s, _) = send(
        &state,
        "/v1/auth/publish-device-cert",
        serde_json::json!({
            "device_id": device_id,
            "device_cert": b64(&bad_cert.to_bytes()),
            "cert_issued_at": issued_at as i64,
            "cert_identity_version": 1,
            "mls_signature_pub": b64(&mls_pub),
        }),
        Some(&token),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "cert not signed by the account key must be rejected");

    // The session is NOT spent on a rejected cert — a valid cert still works.
    let good_cert = account.sign(&cert_payload(device_id, &mls_pub, 1, issued_at));
    let (s, _) = send(
        &state,
        "/v1/auth/publish-device-cert",
        serde_json::json!({
            "device_id": device_id,
            "device_cert": b64(&good_cert.to_bytes()),
            "cert_issued_at": issued_at as i64,
            "cert_identity_version": 1,
            "mls_signature_pub": b64(&mls_pub),
        }),
        Some(&token),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "a valid cert must be accepted");
}

// ── 7. Subsequent device — cert-validity ALONE (no session) ──────────────────

/// Establish an account identity under `account` and register `device_id`, both
/// using the session minted by re-login. Returns once the device row exists but
/// has NO cert yet — the state a subsequent device is in just before its first
/// cert publish.
async fn setup_registered_device(
    state: &AppState,
    email: &str,
    device_id: &str,
    account: &SigningKey,
) -> String {
    let v = login(state, email, device_id).await;
    let user_id = v["user_id"].as_str().unwrap().to_string();
    let token = v["session_token"].as_str().unwrap().to_string();
    let (s, _) = send(
        state,
        "/v1/auth/establish-identity",
        serde_json::json!({
            "account_id_pub": b64(&account.verifying_key().to_bytes()),
            "salt": b64(&[1u8; 32]),
            "nonce": b64(&[2u8; 12]),
            "wrapped_key": b64(&[3u8; 48]),
        }),
        Some(&token),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "establish-identity should succeed");
    let (s, _) = send(
        state,
        "/v1/auth/register-device",
        serde_json::json!({ "device_id": device_id }),
        Some(&token),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "register-device should succeed");
    user_id
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_cert_validity_alone_no_session() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));

    let device_id = "dev-sub";
    let account = gen_key();
    let user_id = setup_registered_device(&state, "sub@x.com", device_id, &account).await;

    let mut mls_pub = [0u8; 32];
    OsRng.fill_bytes(&mut mls_pub);
    let issued_at: u64 = 1_700_000_000;
    let cert = account.sign(&cert_payload(device_id, &mls_pub, 1, issued_at));

    // No session header — the cert (signed by the established account key) is the
    // ONLY proof. user_id comes from the body.
    let (s, _) = send(
        &state,
        "/v1/auth/publish-device-cert",
        serde_json::json!({
            "user_id": user_id,
            "device_id": device_id,
            "device_cert": b64(&cert.to_bytes()),
            "cert_issued_at": issued_at as i64,
            "cert_identity_version": 1,
            "mls_signature_pub": b64(&mls_pub),
        }),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::OK,
        "a valid cert with no session must be accepted (cert-validity gate)"
    );

    // The pivot landed.
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT mls_signature_pub FROM user_device WHERE device_id = ?1",
            libsql::params![device_id],
        )
        .await
        .unwrap();
    let stored: Vec<u8> = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<Option<Vec<u8>>>(0)
        .unwrap()
        .unwrap();
    assert_eq!(stored, mls_pub.to_vec());
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_cert_validity_alone_rejects_wrong_account_key() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));

    let device_id = "dev-sub2";
    let account = gen_key();
    let user_id = setup_registered_device(&state, "sub2@x.com", device_id, &account).await;

    let mut mls_pub = [0u8; 32];
    OsRng.fill_bytes(&mut mls_pub);
    let issued_at: u64 = 1_700_000_000;

    // A cert signed by an ATTACKER key, no session — must be rejected. This is
    // the load-bearing property: cert-validity-alone never accepts a cert that
    // doesn't chain to the account's stored account_id_pub.
    let attacker = gen_key();
    let bad_cert = attacker.sign(&cert_payload(device_id, &mls_pub, 1, issued_at));
    let (s, _) = send(
        &state,
        "/v1/auth/publish-device-cert",
        serde_json::json!({
            "user_id": user_id,
            "device_id": device_id,
            "device_cert": b64(&bad_cert.to_bytes()),
            "cert_issued_at": issued_at as i64,
            "cert_identity_version": 1,
            "mls_signature_pub": b64(&mls_pub),
        }),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "a cert not signed by the account key must be rejected with no session"
    );

    // No session AND no body user_id → unauthorized (nothing to bind the user).
    let (s, _) = send(
        &state,
        "/v1/auth/publish-device-cert",
        serde_json::json!({
            "device_id": device_id,
            "device_cert": b64(&account.sign(&cert_payload(device_id, &mls_pub, 1, issued_at)).to_bytes()),
            "cert_issued_at": issued_at as i64,
            "cert_identity_version": 1,
            "mls_signature_pub": b64(&mls_pub),
        }),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "no session and no user_id leaves no one to authorize as"
    );
}

// ── 8. Enrollment request — session-gated, binds user from session ───────────

#[tokio::test(flavor = "multi_thread")]
async fn enrollment_request_is_session_gated_and_binds_user() {
    let db = fresh_db().await;
    let state = dev_state(Arc::clone(&db));

    let device_id = "dev-enr";
    let v = login(&state, "enr@x.com", device_id).await;
    let user_id = v["user_id"].as_str().unwrap().to_string();
    let token = v["session_token"].as_str().unwrap().to_string();

    let req_body = serde_json::json!({
        "request_id": "req-1",
        "new_device_ephemeral_pub": b64(&[7u8; 32]),
        "verification_code": "123456",
        "created_at": "2026-06-27T00:00:00Z",
        "expires_at": "2026-06-27T00:10:00Z",
    });

    // No session → unauthorized.
    let (s, _) = send(&state, "/v1/auth/enrollment-request", req_body.clone(), None).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "enrollment-request must be session-gated"
    );

    // With the session → 200, and the row is bound to the session's user + device.
    let (s, _) = send(&state, "/v1/auth/enrollment-request", req_body, Some(&token)).await;
    assert_eq!(s, StatusCode::OK, "enrollment-request with session should 200");

    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT user_id, new_device_id, status FROM device_enrollment_request WHERE id = ?1",
            libsql::params!["req-1"],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("enrollment row inserted");
    let row_user: String = row.get(0).unwrap();
    let row_device: String = row.get(1).unwrap();
    let row_status: String = row.get(2).unwrap();
    assert_eq!(row_user, user_id, "user_id must be bound from the session");
    assert_eq!(row_device, device_id, "new_device_id must be bound from the session");
    assert_eq!(row_status, "pending");
}
