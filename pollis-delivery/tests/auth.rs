//! Device-certificate-signature auth, driven through the real axum router with
//! `tower::oneshot` against a local libsql DB.
//!
//! These tests are the executable spec for the signing contract the pollis-core
//! client must match: headers `X-Pollis-{User,Device,Timestamp,Signature}` over
//! the canonical message `{METHOD}\n{PATH}\n{TS}\n{hex(sha256(body))}`, signed
//! with the device's raw-1312-byte-pubkey ML-DSA-44 key (the same
//! `user_device.mls_signature_pub_pq` openmls produces).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey, VerifyingKey};
use http_body_util::BodyExt as _;
use pollis_delivery::auth::canonical_message;
use pollis_delivery::db::Db;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

/// Mint a fresh ML-DSA-44 signing key from OS randomness. An ML-DSA private key
/// IS its 32-byte seed, so 32 random bytes is the whole key.
fn gen_signing_key() -> SigningKey<MlDsa44> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    SigningKey::<MlDsa44>::from_seed(&seed.into())
}

/// The raw 1312-byte encoding stored in `user_device.mls_signature_pub_pq`.
fn pq_pub(vk: &VerifyingKey<MlDsa44>) -> Vec<u8> {
    vk.encode().to_vec()
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
  added_device_ids TEXT,\
  generation INTEGER NOT NULL DEFAULT 0\
);\
CREATE UNIQUE INDEX idx_mls_commit_conv_gen_epoch ON mls_commit_log (conversation_id, generation, epoch);\
CREATE TABLE mls_commit_since (\
  conversation_id TEXT NOT NULL,\
  user_id TEXT NOT NULL,\
  device_id TEXT NOT NULL,\
  since_epoch INTEGER NOT NULL,\
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),\
  generation INTEGER NOT NULL DEFAULT 0,\
  PRIMARY KEY (conversation_id, user_id, device_id)\
);\
CREATE TABLE mls_group_info (\
  conversation_id TEXT PRIMARY KEY,\
  epoch INTEGER NOT NULL,\
  group_info BLOB NOT NULL,\
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),\
  updated_by_device_id TEXT NOT NULL,\
  generation INTEGER NOT NULL DEFAULT 0\
);\
CREATE TABLE mls_welcome (\
  id TEXT PRIMARY KEY,\
  conversation_id TEXT NOT NULL,\
  recipient_id TEXT NOT NULL,\
  welcome_data BLOB NOT NULL,\
  delivered INTEGER NOT NULL DEFAULT 0,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  recipient_device_id TEXT,\
  generation INTEGER NOT NULL DEFAULT 0\
);\
CREATE UNIQUE INDEX idx_mls_welcome_recipient ON mls_welcome (conversation_id, recipient_id, recipient_device_id);\
CREATE TABLE group_member (\
  group_id TEXT NOT NULL,\
  user_id  TEXT NOT NULL,\
  role     TEXT NOT NULL DEFAULT 'member',\
  joined_at TEXT NOT NULL DEFAULT (datetime('now')),\
  PRIMARY KEY (group_id, user_id)\
);\
CREATE TABLE dm_channel_member (\
  dm_channel_id TEXT NOT NULL,\
  user_id       TEXT NOT NULL,\
  added_by      TEXT NOT NULL DEFAULT '',\
  added_at      TEXT NOT NULL DEFAULT (datetime('now')),\
  accepted_at   TEXT,\
  PRIMARY KEY (dm_channel_id, user_id)\
);\
CREATE TABLE channels (\
  id       TEXT PRIMARY KEY,\
  group_id TEXT NOT NULL,\
  name     TEXT NOT NULL DEFAULT '',\
  channel_type TEXT NOT NULL DEFAULT 'text',\
  created_at TEXT NOT NULL DEFAULT (datetime('now'))\
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
  mls_signature_pub_pq BLOB,\
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

/// Seed a live (non-revoked) device row with the given raw 1312-byte pubkey —
/// exactly the bytes openmls `to_public_vec()` yields for ML-DSA-44.
async fn seed_device(db: &Db, user_id: &str, device_id: &str, vk: &VerifyingKey<MlDsa44>) {
    let conn = db.conn().unwrap();
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES (?1, ?2, ?3)",
        libsql::params![device_id, user_id, pq_pub(vk)],
    )
    .await
    .unwrap();
}

/// Seed `user_id` as a member of the group `conversation_id` so the commit
/// submit path's `is_member` authz gate passes (a group id is a valid MLS
/// conversation id — a group's text channels share one MLS group keyed by it).
async fn seed_group_membership(db: &Db, conversation_id: &str, user_id: &str) {
    let conn = db.conn().unwrap();
    conn.execute(
        "INSERT INTO group_member (group_id, user_id) VALUES (?1, ?2)",
        libsql::params![conversation_id, user_id],
    )
    .await
    .unwrap();
}

/// Seed a *revoked* device (revoked_at set) — auth must reject it.
async fn seed_revoked_device(db: &Db, user_id: &str, device_id: &str, vk: &VerifyingKey<MlDsa44>) {
    let conn = db.conn().unwrap();
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq, revoked_at) \
         VALUES (?1, ?2, ?3, datetime('now'))",
        libsql::params![device_id, user_id, pq_pub(vk)],
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
    signing_key: &SigningKey<MlDsa44>,
    body: &[u8],
    sig_override: Option<&str>,
) -> Request<Body> {
    let msg = canonical_message("POST", "/v1/commits", timestamp, body);
    let sig_b64 = match sig_override {
        Some(s) => s.to_string(),
        None => b64(&signing_key.sign(&msg).encode()),
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

/// A signed POST to an arbitrary path (the retention-report endpoint uses
/// `/v1/commits/since`, not `/v1/commits`), signing over the exact body bytes.
fn signed_post(
    path: &str,
    user_id: &str,
    device_id: &str,
    timestamp: i64,
    signing_key: &SigningKey<MlDsa44>,
    body: &[u8],
) -> Request<Body> {
    let msg = canonical_message("POST", path, timestamp, body);
    let sig_b64 = b64(&signing_key.sign(&msg).encode());
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("X-Pollis-User", user_id)
        .header("X-Pollis-Device", device_id)
        .header("X-Pollis-Timestamp", timestamp.to_string())
        .header("X-Pollis-Signature", sig_b64)
        .body(Body::from(body.to_vec()))
        .unwrap()
}

fn since_body_json(conv: &str, generation: i64, since: i64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "conversation_id": conv,
        "generation": generation,
        "since": since,
    }))
    .unwrap()
}

/// The `(generation, since_epoch)` recorded for a device, or `None` if no report
/// was recorded — the observable the #681 report-auth tests assert on.
async fn recorded_since(db: &Db, conv: &str, device_id: &str) -> Option<(i64, i64)> {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT generation, since_epoch FROM mls_commit_since \
             WHERE conversation_id = ?1 AND device_id = ?2",
            libsql::params![conv, device_id],
        )
        .await
        .unwrap();
    match rows.next().await.unwrap() {
        Some(r) => Some((r.get::<i64>(0).unwrap(), r.get::<i64>(1).unwrap())),
        None => None,
    }
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
    // alice must be a current member of the conversation to submit a commit.
    seed_group_membership(&db, "conv1", "alice").await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);

    assert_eq!(status_of(router, req).await, StatusCode::OK);
}

/// Security gap (issue #430 P2): a validly-signed commit submit from someone who
/// is NOT a current member of the conversation is Forbidden — the identity is
/// authentic and matches `sender_id`, but membership is what authorizes writing
/// to the group's commit log (mirrors `/v1/group-info`).
#[tokio::test(flavor = "multi_thread")]
async fn non_member_commit_is_forbidden_403() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    // alice signs authentically as herself, but is seeded into NO conversation.
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);

    assert_eq!(status_of(router, req).await, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn tampered_signature_is_rejected_401() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    // A valid-length-but-wrong signature (all zero bytes).
    let bad_sig = b64(&[0u8; pollis_device_cert::MLDSA44_SIG_LEN]);
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
    let sig = b64(&sk.sign(&canonical_message("POST", "/v1/commits", ts, &body_a)).encode());
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

// ── Retention high-water report auth (#681) ───────────────────────────────────

/// A validly-signed `POST /v1/commits/since` is accepted AND recorded as the
/// signing device's retention high-water. This is the authenticated replacement
/// for the old unsigned GET side-effect.
#[tokio::test(flavor = "multi_thread")]
async fn signed_commit_since_report_is_recorded() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = since_body_json("conv1", 0, 7);
    let req = signed_post("/v1/commits/since", "alice", "dev-alice", now(), &sk, &body);

    assert_eq!(status_of(router, req).await, StatusCode::OK);
    assert_eq!(
        recorded_since(&db, "conv1", "dev-alice").await,
        Some((0, 7)),
        "a signed report is recorded as the device's high-water"
    );
}

/// An UNAUTHENTICATED report is ignored — the recorded high-water is UNCHANGED,
/// not merely the request rejected. This is the core #681 fix: nobody can raise
/// the retention floor on a device's behalf without its signature. (Dropping the
/// report only ever leaves the floor MORE conservative — it can never widen
/// pruning.)
#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_commit_since_report_is_ignored() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    // A truthful prior high-water for alice's device.
    pollis_delivery::commit::record_commit_since(&db.conn().unwrap(), "conv1", "alice", "dev-alice", 0, 5)
        .await
        .unwrap();

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    // No signature headers, but a body that WOULD raise the high-water to 999.
    let body = serde_json::to_vec(&serde_json::json!({
        "conversation_id": "conv1",
        "generation": 0,
        "since": 999,
        "user_id": "alice",
        "device_id": "dev-alice",
    }))
    .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/commits/since")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        recorded_since(&db, "conv1", "dev-alice").await,
        Some((0, 5)),
        "an unauthenticated report must NOT move the recorded high-water"
    );
}

/// A signed report from a REVOKED device is rejected (auth refuses a revoked
/// device — I5), and nothing is recorded. A revoked device can never rejoin, so
/// it must not be able to influence the retention floor either.
#[tokio::test(flavor = "multi_thread")]
async fn revoked_device_commit_since_report_is_rejected() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_revoked_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = since_body_json("conv1", 0, 7);
    let req = signed_post("/v1/commits/since", "alice", "dev-alice", now(), &sk, &body);

    assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        recorded_since(&db, "conv1", "dev-alice").await,
        None,
        "a revoked device's report is not recorded"
    );
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
