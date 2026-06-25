//! Device-certificate-signature auth, driven through the real axum router with
//! `tower::oneshot` against a local libsql DB.
//!
//! These tests are the executable spec for the signing contract the pollis-core
//! client must match: headers `X-Pollis-{User,Device,Timestamp,Signature}` over
//! the canonical message `{METHOD}\n{PATH}\n{TS}\n{hex(sha256(body))}`, signed
//! with the device's raw-32-byte-pubkey Ed25519 key (the same
//! `user_device.mls_signature_pub` openmls produces).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use http_body_util::BodyExt as _;
use pollis_delivery::auth::canonical_message;
use pollis_delivery::db::Db;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

/// Mint a fresh Ed25519 signing key from OS randomness. (`SigningKey::generate`
/// needs ed25519-dalek's `rand_core` feature, which the lib doesn't enable;
/// building from 32 random bytes is equivalent and feature-free.)
fn gen_signing_key() -> SigningKey {
    let mut secret = [0u8; 32];
    OsRng.fill_bytes(&mut secret);
    SigningKey::from_bytes(&secret)
}

// Minimal schema: the commit-log table the DS writes, plus the `user_device`
// table the auth path reads. Columns match the real Turso baseline
// (mls_signature_pub BLOB, revoked_at TEXT).
const SCHEMA: &str = "\
CREATE TABLE mls_commit_log (\
  seq INTEGER PRIMARY KEY AUTOINCREMENT,\
  conversation_id TEXT NOT NULL,\
  epoch INTEGER NOT NULL,\
  sender_id TEXT NOT NULL,\
  commit_data BLOB NOT NULL,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  added_user_id TEXT,\
  added_device_ids TEXT\
);\
CREATE UNIQUE INDEX idx_mls_commit_conv_epoch ON mls_commit_log (conversation_id, epoch);\
CREATE TABLE mls_group_info (\
  conversation_id TEXT PRIMARY KEY,\
  epoch INTEGER NOT NULL,\
  group_info BLOB NOT NULL,\
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),\
  updated_by_device_id TEXT NOT NULL\
);\
CREATE TABLE mls_welcome (\
  id TEXT PRIMARY KEY,\
  conversation_id TEXT NOT NULL,\
  recipient_id TEXT NOT NULL,\
  welcome_data BLOB NOT NULL,\
  delivered INTEGER NOT NULL DEFAULT 0,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  recipient_device_id TEXT\
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

async fn fresh_db() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("delivery.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    db.conn().unwrap().execute_batch(SCHEMA).await.expect("schema");
    Arc::new(db)
}

/// Seed a live (non-revoked) device row with the given raw 32-byte pubkey —
/// exactly the bytes openmls `to_public_vec()` yields for Ed25519.
async fn seed_device(db: &Db, user_id: &str, device_id: &str, vk: &VerifyingKey) {
    let conn = db.conn().unwrap();
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub) VALUES (?1, ?2, ?3)",
        libsql::params![device_id, user_id, vk.to_bytes().to_vec()],
    )
    .await
    .unwrap();
}

/// Seed a *revoked* device (revoked_at set) — auth must reject it.
async fn seed_revoked_device(db: &Db, user_id: &str, device_id: &str, vk: &VerifyingKey) {
    let conn = db.conn().unwrap();
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub, revoked_at) \
         VALUES (?1, ?2, ?3, datetime('now'))",
        libsql::params![device_id, user_id, vk.to_bytes().to_vec()],
    )
    .await
    .unwrap();
}

fn submit_body_json(conv: &str, epoch: i64, sender: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "conversation_id": conv,
        "based_on_epoch": epoch,
        "sender_id": sender,
        "commit": b64(format!("commit-{conv}-{epoch}-{sender}").as_bytes()),
        "group_info": b64(b"group-info"),
    }))
    .unwrap()
}

/// Build a signed POST /v1/commits request. `timestamp` is what we put in the
/// header AND sign over; `signing_key` produces the signature; `sig_override`
/// lets a test tamper the signature.
fn signed_request(
    user_id: &str,
    device_id: &str,
    timestamp: i64,
    signing_key: &SigningKey,
    body: &[u8],
    sig_override: Option<&str>,
) -> Request<Body> {
    let msg = canonical_message("POST", "/v1/commits", timestamp, body);
    let sig_b64 = match sig_override {
        Some(s) => s.to_string(),
        None => b64(&signing_key.sign(&msg).to_bytes()),
    };
    Request::builder()
        .method("POST")
        .uri("/v1/commits")
        .header("content-type", "application/json")
        .header("X-Pollis-User", user_id)
        .header("X-Pollis-Device", device_id)
        .header("X-Pollis-Timestamp", timestamp.to_string())
        .header("X-Pollis-Signature", sig_b64)
        .body(Body::from(body.to_vec()))
        .unwrap()
}

async fn status_of(router: axum::Router, req: Request<Body>) -> StatusCode {
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    // Drain the body so nothing leaks; status is what we assert on.
    let _ = resp.into_body().collect().await;
    status
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// ── Auth ON ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn valid_signature_is_accepted() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);

    assert_eq!(status_of(router, req).await, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn tampered_signature_is_rejected_401() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    // A valid-length-but-wrong signature (all zero bytes).
    let bad_sig = b64(&[0u8; 64]);
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, Some(&bad_sig));

    assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn signature_over_different_body_is_rejected_401() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    // Sign over body A, then send body B — the sha256(body) binding must fail.
    let body_a = submit_body_json("conv1", 0, "alice");
    let body_b = submit_body_json("conv1", 0, "alice-different");
    let ts = now();
    let sig = b64(&sk.sign(&canonical_message("POST", "/v1/commits", ts, &body_a)).to_bytes());
    let req = Request::builder()
        .method("POST")
        .uri("/v1/commits")
        .header("X-Pollis-User", "alice")
        .header("X-Pollis-Device", "dev-alice")
        .header("X-Pollis-Timestamp", ts.to_string())
        .header("X-Pollis-Signature", sig)
        .body(Body::from(body_b))
        .unwrap();

    assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_timestamp_is_rejected_401() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    // 10 minutes in the past — outside the ±300s replay window. The signature
    // itself is valid over this (stale) timestamp.
    let stale = now() - 600;
    let req = signed_request("alice", "dev-alice", stale, &sk, &body, None);

    assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_device_row_is_rejected_401() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    // Deliberately do NOT seed a user_device row.

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-ghost", now(), &sk, &body, None);

    assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_device_is_rejected_401() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_revoked_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);

    assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_headers_are_rejected_401() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    // No auth headers at all.
    let req = Request::builder()
        .method("POST")
        .uri("/v1/commits")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn sender_id_mismatch_is_forbidden_403() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    // Authenticated as alice/dev-alice, but the commit claims sender "bob".
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "bob");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);

    assert_eq!(status_of(router, req).await, StatusCode::FORBIDDEN);
}

// ── Auth OFF (default) ────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn auth_off_accepts_unauthenticated_commit() {
    let db = fresh_db().await;
    // require_auth = false: today's behavior. No headers, no device row.
    let router = build_router_with_state(AppState::new(Arc::clone(&db), false));
    let body = submit_body_json("conv1", 0, "alice");
    let req = Request::builder()
        .method("POST")
        .uri("/v1/commits")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    assert_eq!(status_of(router, req).await, StatusCode::OK);
}

// ── Reads stay open ───────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn reads_are_open_even_with_auth_on() {
    let db = fresh_db().await;
    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/commits/conv1?since=0")
        .body(Body::empty())
        .unwrap();

    assert_eq!(status_of(router, req).await, StatusCode::OK);
}
