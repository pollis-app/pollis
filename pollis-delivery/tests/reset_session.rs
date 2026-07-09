//! The pre-enrollment soft-reset path (#487): `/v1/account/rotate-identity`,
//! `/v1/account/reset-recover`, and `/v1/welcomes/purge` must accept a
//! verified-OTP session (`X-Pollis-Session`) as the authenticating credential.
//!
//! A device performing the soft reset from the login gate has, by definition,
//! no registered `mls_signature_pub` and no open local DB — it cannot
//! device-sign. Before this gate existed, the reset flow failed unconditionally
//! ("not signed in for DS request signing"). These tests drive the real axum
//! router with auth ENFORCED and prove:
//!
//!   1. a verified-OTP session authorizes the rotation / reset / purge;
//!   2. `user_id` binds from the session — a body naming another user is 403;
//!   3. no credential at all is 401 (the gate never fails open);
//!   4. a request that presents a (bad) device signature is NOT rescued by a
//!      valid session — the stronger credential, once offered, must verify.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use pollis_delivery::db::Db;
use pollis_delivery::otp::OtpConfig;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

// Self-contained schema (foreign_keys=OFF in Db::connect_local): the tables the
// rotate / reset-recover / purge handlers touch, columns matching the Turso
// baseline.
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
CREATE TABLE groups (\
  id TEXT PRIMARY KEY,\
  name TEXT NOT NULL,\
  description TEXT,\
  owner_id TEXT NOT NULL,\
  created_at TEXT NOT NULL DEFAULT (datetime('now'))\
);\
CREATE TABLE group_member (\
  group_id TEXT NOT NULL,\
  user_id TEXT NOT NULL,\
  role TEXT NOT NULL DEFAULT 'member',\
  joined_at TEXT NOT NULL DEFAULT (datetime('now')),\
  PRIMARY KEY (group_id, user_id)\
);\
CREATE TABLE dm_channel_member (\
  dm_channel_id TEXT NOT NULL,\
  user_id TEXT NOT NULL,\
  accepted INTEGER NOT NULL DEFAULT 0,\
  PRIMARY KEY (dm_channel_id, user_id)\
);\
CREATE TABLE user_groups (\
  user_id TEXT NOT NULL,\
  group_id TEXT NOT NULL,\
  group_name TEXT NOT NULL,\
  role TEXT NOT NULL DEFAULT 'member',\
  joined_at TEXT NOT NULL DEFAULT (datetime('now')),\
  last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),\
  PRIMARY KEY (user_id, group_id)\
);\
CREATE TABLE user_dms (\
  user_id TEXT NOT NULL,\
  dm_channel_id TEXT NOT NULL,\
  created_by TEXT NOT NULL,\
  added_at TEXT NOT NULL DEFAULT (datetime('now')),\
  accepted_at TEXT,\
  last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),\
  PRIMARY KEY (user_id, dm_channel_id)\
);\
CREATE TABLE mls_key_package (\
  id INTEGER PRIMARY KEY AUTOINCREMENT,\
  user_id TEXT NOT NULL,\
  device_id TEXT,\
  key_package BLOB NOT NULL,\
  created_at TEXT NOT NULL DEFAULT (datetime('now'))\
);\
CREATE TABLE mls_welcome (\
  id INTEGER PRIMARY KEY AUTOINCREMENT,\
  recipient_id TEXT NOT NULL,\
  conversation_id TEXT,\
  welcome BLOB,\
  created_at TEXT NOT NULL DEFAULT (datetime('now'))\
);";

async fn fresh_db() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ds.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    db.conn().unwrap().execute_batch(SCHEMA).await.expect("schema");
    Arc::new(db)
}

/// Auth ENFORCED (unlike the bootstrap tests) — the point here is the gate.
/// OTP is the fixed dev code, no email send, no throttle.
fn authed_state(db: Arc<Db>) -> AppState {
    AppState::new(db, true).with_otp_config(OtpConfig {
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
    // Present a device-signature header (deliberately invalid) to prove the
    // signature path, once offered, is not rescued by a session.
    bogus_signature: bool,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(tok) = session {
        builder = builder.header("X-Pollis-Session", tok);
    }
    if bogus_signature {
        builder = builder
            .header("X-Pollis-User", "u-any")
            .header("X-Pollis-Device", "d-any")
            .header("X-Pollis-Timestamp", "0")
            .header("X-Pollis-Signature", base64::engine::general_purpose::STANDARD.encode([0u8; 64]));
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

/// request-otp + verify-otp, returning `(user_id, session_token)`.
async fn login(state: &AppState, email: &str, device_id: &str) -> (String, String) {
    let (s, _) = send(state, "/v1/auth/request-otp", serde_json::json!({ "email": email }), None, false).await;
    assert_eq!(s, StatusCode::OK, "request-otp should 200");
    let (s, body) = send(
        state,
        "/v1/auth/verify-otp",
        serde_json::json!({ "email": email, "code": DEV_CODE, "device_id": device_id }),
        None,
        false,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "verify-otp should 200: {body}");
    (
        body["user_id"].as_str().expect("user_id").to_string(),
        body["session_token"].as_str().expect("session_token").to_string(),
    )
}

/// [`login`] then version-1 identity establishment — the state every real
/// account is in before a rotation (the rotate CAS reads the
/// `account_key_log` head, which signup's establish-identity seeds at 1).
async fn login_established(state: &AppState, email: &str, device_id: &str) -> (String, String) {
    let (user_id, token) = login(state, email, device_id).await;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let (s, body) = send(
        state,
        "/v1/auth/establish-identity",
        serde_json::json!({
            "account_id_pub": b64(&[9u8; 32]),
            "salt": b64(&[1u8; 32]),
            "nonce": b64(&[2u8; 12]),
            "wrapped_key": b64(&[3u8; 48]),
        }),
        Some(&token),
        false,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "establish-identity should 200: {body}");
    (user_id, token)
}

fn rotate_body(based_on_version: i64) -> serde_json::Value {
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    serde_json::json!({
        "based_on_version": based_on_version,
        "account_id_pub": b64(&[7u8; 32]),
        "salt": b64(&[1u8; 16]),
        "nonce": b64(&[2u8; 12]),
        "wrapped_key": b64(&[3u8; 48]),
    })
}

async fn identity_version(db: &Db, user_id: &str) -> i64 {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query("SELECT identity_version FROM users WHERE id = ?1", libsql::params![user_id])
        .await
        .unwrap();
    rows.next().await.unwrap().expect("user row").get(0).unwrap()
}

async fn count(db: &Db, sql: &str, user_id: &str) -> i64 {
    let conn = db.conn().unwrap();
    let mut rows = conn.query(sql, libsql::params![user_id]).await.unwrap();
    rows.next().await.unwrap().expect("count row").get(0).unwrap()
}

// ── 1. Session authorizes the rotation, user bound from the session ──────────

#[tokio::test(flavor = "multi_thread")]
async fn rotate_identity_accepts_verified_otp_session() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, token) = login_established(&state, "alice@example.com", "dev-1").await;

    let (s, body) = send(&state, "/v1/account/rotate-identity", rotate_body(1), Some(&token), false).await;
    assert_eq!(s, StatusCode::OK, "session-authorized rotation should 200: {body}");
    assert_eq!(body["identity_version"], serde_json::json!(2));
    assert_eq!(identity_version(&db, &user_id).await, 2);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM account_key_log WHERE user_id = ?1", &user_id).await,
        2,
        "rotation must append the transparency log (v1 from establish, v2 from rotate)"
    );
}

// ── 2. The session binds the actor — a body naming another user is 403 ───────

#[tokio::test(flavor = "multi_thread")]
async fn rotate_identity_session_cannot_act_as_another_user() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (_alice, alice_token) = login_established(&state, "alice@example.com", "dev-1").await;
    let (bob, _bob_token) = login_established(&state, "bob@example.com", "dev-2").await;

    let mut body = rotate_body(1);
    body["user_id"] = serde_json::json!(bob);
    let (s, _) = send(&state, "/v1/account/rotate-identity", body, Some(&alice_token), false).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "alice's session must not rotate bob's identity");
    assert_eq!(identity_version(&db, &bob).await, 1, "bob's identity must be untouched");
}

// ── 3. No credential → 401; the gate never fails open ────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn rotate_identity_rejects_missing_and_stale_credentials() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, _token) = login_established(&state, "alice@example.com", "dev-1").await;

    // No credential at all.
    let (s, _) = send(&state, "/v1/account/rotate-identity", rotate_body(1), None, false).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "no credential must be rejected");

    // Garbage session token.
    let (s, _) = send(&state, "/v1/account/rotate-identity", rotate_body(1), Some("nonsense"), false).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "unknown session must be rejected");

    assert_eq!(identity_version(&db, &user_id).await, 1);
}

// ── 4. A bad signature is never rescued by a valid session ───────────────────

#[tokio::test(flavor = "multi_thread")]
async fn bad_signature_not_rescued_by_valid_session() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, token) = login_established(&state, "alice@example.com", "dev-1").await;

    let (s, _) = send(&state, "/v1/account/rotate-identity", rotate_body(1), Some(&token), true).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "a request offering a device signature must stand on that signature"
    );
    assert_eq!(identity_version(&db, &user_id).await, 1);
}

// ── 5. reset-recover with a session: memberships cleared, devices orphaned ───

#[tokio::test(flavor = "multi_thread")]
async fn reset_recover_accepts_session_and_cleans_up() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, token) = login(&state, "alice@example.com", "dev-new").await;

    // Seed pre-reset state: a group membership (with a co-admin so handoff is
    // trivial), a DM membership, a key package, an old enrolled device, and a
    // pending welcome.
    {
        let conn = db.conn().unwrap();
        for sql in [
            format!("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', '{user_id}')"),
            format!("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', '{user_id}', 'admin')"),
            "INSERT INTO users (id, email, username) VALUES ('other', 'o@x.com', 'other')".to_string(),
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'other', 'admin')".to_string(),
            format!("INSERT INTO dm_channel_member (dm_channel_id, user_id) VALUES ('dm1', '{user_id}')"),
            format!("INSERT INTO mls_key_package (user_id, key_package) VALUES ('{user_id}', x'00')"),
            format!("INSERT INTO user_device (device_id, user_id) VALUES ('dev-old', '{user_id}')"),
            format!("INSERT INTO mls_welcome (recipient_id) VALUES ('{user_id}')"),
        ] {
            conn.execute(&sql, ()).await.expect("seed");
        }
    }

    let (s, body) = send(
        &state,
        "/v1/account/reset-recover",
        serde_json::json!({ "current_device_id": "dev-new" }),
        Some(&token),
        false,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "session-authorized reset-recover should 200: {body}");

    assert_eq!(count(&db, "SELECT COUNT(*) FROM group_member WHERE user_id = ?1", &user_id).await, 0);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM dm_channel_member WHERE user_id = ?1", &user_id).await, 0);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM mls_key_package WHERE user_id = ?1", &user_id).await, 0);
    // Other devices orphaned; the resetting device's row (if any) is kept.
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM user_device WHERE user_id = ?1 AND device_id = 'dev-old'", &user_id).await,
        0,
        "old device must be orphaned"
    );

    // The purge leg (same session credential).
    let (s, _) = send(&state, "/v1/welcomes/purge", serde_json::json!({}), Some(&token), false).await;
    assert_eq!(s, StatusCode::OK, "session-authorized purge should 200");
    assert_eq!(count(&db, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = ?1", &user_id).await, 0);
}
