//! Device-certificate-signature auth, driven through the real axum router.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey, VerifyingKey};
use http_body_util::BodyExt as _;
use pollis_delivery::auth::{
    canonical_message, verify_request_identity, verify_request_identity_cached, DeviceKeyCache,
    DEVICE_KEY_CACHE_TTL_SECS,
};
use pollis_delivery::db::Db;
use pollis_delivery::error::AuthRejection;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

mod common;

fn gen_signing_key() -> SigningKey<MlDsa44> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    SigningKey::<MlDsa44>::from_seed(&seed.into())
}

fn pq_pub(vk: &VerifyingKey<MlDsa44>) -> Vec<u8> {
    vk.encode().to_vec()
}


fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db
}

/// FKs are not enforced (see `db::LOCAL_PRAGMAS`); seeding keeps the fixture honest.
async fn seed_user(db: &Db, user_id: &str) {
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO users (id, email, username) VALUES (?1, ?1 || '@x', ?1)",
        libsql::params![user_id],
    )
    .await
    .unwrap();
}

async fn seed_device(db: &Db, user_id: &str, device_id: &str, vk: &VerifyingKey<MlDsa44>) {
    seed_user(db, user_id).await;
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES (?1, ?2, ?3)",
        libsql::params![device_id, user_id, pq_pub(vk)],
    )
    .await
    .unwrap();
}

/// Satisfies the commit submit path's `is_member` gate.
async fn seed_group_membership(db: &Db, conversation_id: &str, user_id: &str) {
    seed_user(db, user_id).await;
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO group_member (group_id, user_id) VALUES (?1, ?2)",
        libsql::params![conversation_id, user_id],
    )
    .await
    .unwrap();
}

async fn seed_revoked_device(db: &Db, user_id: &str, device_id: &str, vk: &VerifyingKey<MlDsa44>) {
    seed_user(db, user_id).await;
    let conn = db.conn().await.unwrap();
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

async fn recorded_since(db: &Db, conv: &str, device_id: &str) -> Option<(i64, i64)> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT generation, since_epoch FROM mls_commit_since \
             WHERE conversation_id = ?1 AND device_id = ?2",
            libsql::params![conv, device_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().map(|r| (r.get::<i64>(0).unwrap(), r.get::<i64>(1).unwrap()))
}

async fn status_of(router: axum::Router, req: Request<Body>) -> StatusCode {
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let _ = resp.into_body().collect().await;
    status
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test(flavor = "multi_thread")]
async fn valid_signature_is_accepted() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);

    assert_eq!(status_of(router, req).await, StatusCode::OK);
}

/// #430: an authentic signer who is not a member may not write the commit log.
#[tokio::test(flavor = "multi_thread")]
async fn non_member_commit_is_forbidden_403() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
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
    // Outside the ±300s replay window, though validly signed.
    let stale = now() - 600;
    let req = signed_request("alice", "dev-alice", stale, &sk, &body, None);

    assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_device_row_is_rejected_401() {
    let db = fresh_db().await;
    let sk = gen_signing_key();

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

async fn rejection_of(router: axum::Router, req: Request<Body>) -> (StatusCode, String, Option<String>) {
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let header = resp
        .headers()
        .get("x-pollis-auth-error")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, json["error"].as_str().unwrap_or_default().to_string(), header)
}

/// #1338: a revoked device signing with its own key is told so, so it can sign out.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_device_signing_with_its_own_key_is_told_it_was_revoked() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_revoked_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);

    let (status, error, header) = rejection_of(router, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error, "device_revoked");
    assert_eq!(header.as_deref(), Some("device_revoked"));
}

/// #1338: only the device's own key earns `device_revoked`, so revocation can't be probed.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_revoked_devices_own_signature_learns_it_was_revoked() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    let other = gen_signing_key();
    seed_revoked_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    let body = submit_body_json("conv1", 0, "alice");

    let bad_sig = b64(&[0u8; pollis_device_cert::MLDSA44_SIG_LEN]);
    let cases = [
        ("a zeroed signature", signed_request("alice", "dev-alice", now(), &sk, &body, Some(&bad_sig))),
        ("another key", signed_request("alice", "dev-alice", now(), &other, &body, None)),
        ("an unknown device", signed_request("alice", "dev-ghost", now(), &sk, &body, None)),
        ("a stale timestamp", signed_request("alice", "dev-alice", now() - 600, &sk, &body, None)),
    ];
    for (label, req) in cases {
        let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
        let (status, error, header) = rejection_of(router, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{label}");
        assert_eq!(error, "unauthorized", "{label}");
        assert_eq!(header, None, "{label}");
    }
}

/// #1338: a revoke evicts the cached key, so the next call gets `device_revoked`.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_revoked_by_its_sibling_is_told_on_its_next_call() {
    let db = fresh_db().await;
    let sk_mac = gen_signing_key();
    let sk_phone = gen_signing_key();
    seed_device(&db, "alice", "dev-mac", &sk_mac.verifying_key()).await;
    seed_device(&db, "alice", "dev-phone", &sk_phone.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let state = AppState::new(Arc::clone(&db), true);
    let body = submit_body_json("conv1", 0, "alice");
    let warm = signed_request("alice", "dev-mac", now(), &sk_mac, &body, None);
    assert_eq!(status_of(build_router_with_state(state.clone()), warm).await, StatusCode::OK);

    let revoke = serde_json::to_vec(&serde_json::json!({ "device_id": "dev-mac" })).unwrap();
    let req = signed_post("/v1/devices/revoke", "alice", "dev-phone", now(), &sk_phone, &revoke);
    assert_eq!(status_of(build_router_with_state(state.clone()), req).await, StatusCode::OK);

    let body = submit_body_json("conv1", 1, "alice");
    let req = signed_request("alice", "dev-mac", now(), &sk_mac, &body, None);
    let (status, error, _) = rejection_of(build_router_with_state(state), req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error, "device_revoked");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_headers_are_rejected_401() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
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
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "bob");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);

    assert_eq!(status_of(router, req).await, StatusCode::FORBIDDEN);
}

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

/// #681: an unsigned report must leave the recorded high-water unchanged.
#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_commit_since_report_is_ignored() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    pollis_delivery::commit::record_commit_since(&db.conn().await.unwrap(), "conv1", "alice", "dev-alice", 0, 5)
        .await
        .unwrap();

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
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

#[tokio::test(flavor = "multi_thread")]
async fn auth_off_accepts_unauthenticated_commit() {
    let db = fresh_db().await;
    // Only reachable via an explicit POLLIS_DS_REQUIRE_AUTH off value (#921).
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

/// The unauthenticated commit-history read is gone; it leaked any group's history.
#[tokio::test(flavor = "multi_thread")]
async fn the_open_commit_log_read_no_longer_exists() {
    let db = fresh_db().await;
    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/commits/conv1?since=0")
        .body(Body::empty())
        .unwrap();

    assert_eq!(
        status_of(router, req).await,
        StatusCode::NOT_FOUND,
        "an unauthenticated GET must not be able to read a conversation's commit log"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_authed_replacement_refuses_an_unsigned_caller() {
    let db = fresh_db().await;
    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = serde_json::json!({ "queries": [{ "conversation_id": "conv1" }] });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/mls/conversation-state")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    assert_eq!(status_of(router, req).await, StatusCode::UNAUTHORIZED);
}

async fn delete_device_row_behind_the_ds(db: &Db, device_id: &str) {
    db.conn().await
        .unwrap()
        .execute(
            "DELETE FROM user_device WHERE device_id = ?1",
            libsql::params![device_id],
        )
        .await
        .unwrap();
}

async fn set_device_pubkey_behind_the_ds(db: &Db, device_id: &str, vk: &VerifyingKey<MlDsa44>) {
    db.conn().await
        .unwrap()
        .execute(
            "UPDATE user_device SET mls_signature_pub_pq = ?1 WHERE device_id = ?2",
            libsql::params![pq_pub(vk), device_id],
        )
        .await
        .unwrap();
}

async fn revoked_at(db: &Db, device_id: &str) -> Option<String> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT revoked_at FROM user_device WHERE device_id = ?1",
            libsql::params![device_id],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .and_then(|r| r.get::<Option<String>>(0).ok().flatten())
}

async fn device_row_exists(db: &Db, device_id: &str) -> bool {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT 1 FROM user_device WHERE device_id = ?1",
            libsql::params![device_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

/// Shares `state`, so every call shares one `device_keys` cache.
async fn signed_call(
    state: &AppState,
    path: &str,
    user_id: &str,
    device_id: &str,
    sk: &SigningKey<MlDsa44>,
    body: &[u8],
) -> StatusCode {
    let req = signed_post(path, user_id, device_id, now(), sk, body);
    status_of(build_router_with_state(state.clone()), req).await
}

fn signed_headers(
    path: &str,
    user_id: &str,
    device_id: &str,
    timestamp: i64,
    sk: &SigningKey<MlDsa44>,
    body: &[u8],
) -> axum::http::HeaderMap {
    let msg = canonical_message("POST", path, timestamp, body);
    let mut h = axum::http::HeaderMap::new();
    h.insert("x-pollis-user", user_id.parse().unwrap());
    h.insert("x-pollis-device", device_id.parse().unwrap());
    h.insert("x-pollis-timestamp", timestamp.to_string().parse().unwrap());
    h.insert(
        "x-pollis-signature",
        b64(&sk.sign(&msg).encode()).parse().unwrap(),
    );
    h
}

/// After the row is deleted behind the DS, success proves the key came from cache; the two calls hit both verify call sites.
#[tokio::test(flavor = "multi_thread")]
async fn cached_device_authenticates_without_re_reading_the_row() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let state = AppState::new(Arc::clone(&db), true);
    assert!(state.device_keys.is_empty(), "cache starts cold");

    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);
    assert_eq!(
        status_of(build_router_with_state(state.clone()), req).await,
        StatusCode::OK
    );
    assert_eq!(
        state.device_keys.len(),
        1,
        "a successful lookup is cached for the next request"
    );

    delete_device_row_behind_the_ds(&db, "dev-alice").await;
    assert!(!device_row_exists(&db, "dev-alice").await);

    let since = since_body_json("conv1", 0, 3);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &sk, &since).await,
        StatusCode::OK,
        "second request served from the cache — no Turso read on the auth path"
    );
    assert_eq!(
        recorded_since(&db, "conv1", "dev-alice").await,
        Some((0, 3)),
        "and it really ran the handler, not just the auth gate"
    );
}

/// A cached device must stop authenticating once revoked via `account::revoke_device`.
#[tokio::test(flavor = "multi_thread")]
async fn revoked_device_cannot_authenticate_from_a_stale_cache_entry() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let state = AppState::new(Arc::clone(&db), true);

    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);
    assert_eq!(
        status_of(build_router_with_state(state.clone()), req).await,
        StatusCode::OK
    );
    assert_eq!(state.device_keys.len(), 1, "cached before revocation");

    let revoke = serde_json::to_vec(&serde_json::json!({ "device_id": "dev-alice" })).unwrap();
    assert_eq!(
        signed_call(&state, "/v1/devices/revoke", "alice", "dev-alice", &sk, &revoke).await,
        StatusCode::OK
    );
    assert!(
        revoked_at(&db, "dev-alice").await.is_some(),
        "the row really is tombstoned"
    );
    assert!(
        state.device_keys.is_empty(),
        "revocation evicted the cached key"
    );

    let since = since_body_json("conv1", 0, 9);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &sk, &since).await,
        StatusCode::UNAUTHORIZED,
        "a revoked device must NOT authenticate from a stale cache entry"
    );
    assert_eq!(
        recorded_since(&db, "conv1", "dev-alice").await,
        None,
        "and nothing it sent was applied"
    );
}

/// Logout deletes the row; the cached entry must go with it.
#[tokio::test(flavor = "multi_thread")]
async fn logged_out_device_cannot_authenticate_from_a_stale_cache_entry() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let state = AppState::new(Arc::clone(&db), true);

    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);
    assert_eq!(
        status_of(build_router_with_state(state.clone()), req).await,
        StatusCode::OK
    );
    assert_eq!(state.device_keys.len(), 1, "cached before logout");

    let logout = serde_json::to_vec(&serde_json::json!({ "device_id": "dev-alice" })).unwrap();
    assert_eq!(
        signed_call(&state, "/v1/auth/logout", "alice", "dev-alice", &sk, &logout).await,
        StatusCode::OK
    );
    assert!(
        !device_row_exists(&db, "dev-alice").await,
        "the row really is gone"
    );
    assert!(state.device_keys.is_empty(), "logout evicted the cached key");

    let since = since_body_json("conv1", 0, 9);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &sk, &since).await,
        StatusCode::UNAUTHORIZED,
        "a logged-out device must NOT authenticate from a stale cache entry"
    );
}

/// Resign evicts even though it doesn't touch the key, and the device still authenticates.
#[tokio::test(flavor = "multi_thread")]
async fn resign_evicts_the_cached_key_and_the_device_still_authenticates() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let state = AppState::new(Arc::clone(&db), true);

    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);
    assert_eq!(
        status_of(build_router_with_state(state.clone()), req).await,
        StatusCode::OK
    );
    assert_eq!(state.device_keys.len(), 1, "cached before resign");

    let resign = serde_json::to_vec(&serde_json::json!({
        "certs": [{
            "device_id": "dev-alice",
            "device_cert": b64(b"re-signed-cert-bytes"),
            "cert_issued_at": "1700000000",
            "cert_identity_version": 2,
        }],
    }))
    .unwrap();
    assert_eq!(
        signed_call(&state, "/v1/devices/resign", "alice", "dev-alice", &sk, &resign).await,
        StatusCode::OK
    );
    assert!(state.device_keys.is_empty(), "resign evicted the cached key");

    let since = since_body_json("conv1", 0, 4);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &sk, &since).await,
        StatusCode::OK,
        "the key is unchanged, so the device re-reads and still authenticates"
    );
    assert_eq!(state.device_keys.len(), 1, "and is re-cached from the DB");
}

/// Publishing a new cert evicts: the new key works and the old one doesn't.
#[tokio::test(flavor = "multi_thread")]
async fn republished_cert_swaps_the_cached_pubkey() {
    let db = fresh_db().await;
    let old_sk = gen_signing_key();
    let new_sk = gen_signing_key();
    let account = gen_signing_key();

    db.conn().await
        .unwrap()
        .execute(
            "INSERT INTO users (id, email, username, account_id_pub, identity_version) \
             VALUES ('alice', 'alice@example.com', 'alice', ?1, 1)",
            libsql::params![account.verifying_key().encode().to_vec()],
        )
        .await
        .unwrap();
    seed_device(&db, "alice", "dev-alice", &old_sk.verifying_key()).await;

    let state = AppState::new(Arc::clone(&db), true);

    let since = since_body_json("conv1", 0, 1);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &old_sk, &since).await,
        StatusCode::OK
    );
    assert_eq!(state.device_keys.len(), 1, "old key cached");

    // No session header: the cert signature against `account_id_pub` is the proof.
    let mut ed_pub = [0u8; 32];
    OsRng.fill_bytes(&mut ed_pub);
    let new_pq = pq_pub(&new_sk.verifying_key());
    let issued_at: u64 = 1_700_000_000;
    let payload =
        pollis_device_cert::device_cert_signed_payload("dev-alice", &ed_pub, &new_pq, 1, issued_at)
            .expect("cert payload");
    let cert = account.sign(&payload).encode().to_vec();
    let publish = serde_json::to_vec(&serde_json::json!({
        "device_id": "dev-alice",
        "device_cert": b64(&cert),
        "cert_issued_at": issued_at as i64,
        "cert_identity_version": 1,
        "mls_signature_pub": b64(&ed_pub),
        "mls_signature_pub_pq": b64(&new_pq),
        "user_id": "alice",
    }))
    .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/auth/publish-device-cert")
        .header("content-type", "application/json")
        .body(Body::from(publish))
        .unwrap();
    assert_eq!(
        status_of(build_router_with_state(state.clone()), req).await,
        StatusCode::OK
    );
    assert!(
        state.device_keys.is_empty(),
        "publishing a new cert evicted the stale key"
    );

    let since = since_body_json("conv1", 0, 5);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &new_sk, &since).await,
        StatusCode::OK,
        "the re-keyed device authenticates with its new key"
    );
    let since = since_body_json("conv1", 0, 6);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &old_sk, &since).await,
        StatusCode::UNAUTHORIZED,
        "the superseded key is no longer accepted"
    );
}

/// TTL backstop for an endpoint that forgets to evict; clock injected, no sleep.
#[tokio::test(flavor = "multi_thread")]
async fn ttl_backstop_expires_a_cached_entry() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let cache = DeviceKeyCache::default();
    let t0 = now();
    let body = since_body_json("conv1", 0, 1);
    let headers = signed_headers("/v1/commits/since", "alice", "dev-alice", t0, &sk, &body);
    let path = "/v1/commits/since";

    assert!(
        verify_request_identity_cached(&cache, &db, &headers, "POST", path, &body, t0)
            .await
            .is_ok()
    );
    assert_eq!(cache.len(), 1);

    delete_device_row_behind_the_ds(&db, "dev-alice").await;

    assert!(
        verify_request_identity_cached(
            &cache,
            &db,
            &headers,
            "POST",
            path,
            &body,
            t0 + DEVICE_KEY_CACHE_TTL_SECS - 1
        )
        .await
        .is_ok(),
        "inside the TTL the entry is still served"
    );

    assert!(
        verify_request_identity_cached(
            &cache,
            &db,
            &headers,
            "POST",
            path,
            &body,
            t0 + DEVICE_KEY_CACHE_TTL_SECS
        )
        .await
        .is_err(),
        "at the TTL the entry must no longer be served"
    );
    assert!(cache.is_empty(), "and the expired entry is dropped, not kept");
}

#[tokio::test(flavor = "multi_thread")]
async fn invalidate_device_forces_a_re_read_of_a_rotated_pubkey() {
    let db = fresh_db().await;
    let old_sk = gen_signing_key();
    let new_sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &old_sk.verifying_key()).await;

    let cache = DeviceKeyCache::default();
    let t = now();
    let path = "/v1/commits/since";
    let body = since_body_json("conv1", 0, 1);

    let old_headers = signed_headers(path, "alice", "dev-alice", t, &old_sk, &body);
    assert!(
        verify_request_identity_cached(&cache, &db, &old_headers, "POST", path, &body, t)
            .await
            .is_ok()
    );

    set_device_pubkey_behind_the_ds(&db, "dev-alice", &new_sk.verifying_key()).await;
    let new_headers = signed_headers(path, "alice", "dev-alice", t, &new_sk, &body);
    assert!(
        verify_request_identity_cached(&cache, &db, &new_headers, "POST", path, &body, t)
            .await
            .is_err(),
        "the stale cached key rejects the device's real, current signature"
    );

    cache.invalidate_device("alice", "dev-alice");
    assert!(
        verify_request_identity_cached(&cache, &db, &new_headers, "POST", path, &body, t)
            .await
            .is_ok(),
        "after eviction the current key is re-read and accepted"
    );
    assert!(
        verify_request_identity_cached(&cache, &db, &old_headers, "POST", path, &body, t)
            .await
            .is_err(),
        "and the superseded key is refused"
    );
}

/// Negatives aren't cached, so enrollment authenticates right after the row appears.
#[tokio::test(flavor = "multi_thread")]
async fn a_miss_is_never_cached_so_a_new_device_is_not_locked_out() {
    let db = fresh_db().await;
    let sk = gen_signing_key();

    let cache = DeviceKeyCache::default();
    let t = now();
    let path = "/v1/commits/since";
    let body = since_body_json("conv1", 0, 1);
    let headers = signed_headers(path, "alice", "dev-new", t, &sk, &body);

    // No row yet → 401, and nothing is remembered about the miss.
    assert!(
        verify_request_identity_cached(&cache, &db, &headers, "POST", path, &body, t)
            .await
            .is_err()
    );
    assert!(cache.is_empty(), "a negative lookup is never cached");

    // Enrollment completes.
    seed_device(&db, "alice", "dev-new", &sk.verifying_key()).await;

    // The very next request works — no TTL to wait out.
    assert!(
        verify_request_identity_cached(&cache, &db, &headers, "POST", path, &body, t)
            .await
            .is_ok(),
        "a freshly-enrolled device authenticates immediately"
    );
}

/// #721: a revoke landing between A's row read and its cache insert must not resurrect the key (forced via the miss barrier).
#[tokio::test(flavor = "multi_thread")]
async fn read_then_evict_race_does_not_resurrect_a_revoked_key() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let state = AppState::new(Arc::clone(&db), true);

    // 0 = initial, 1 = A parked after reading the row, 2 = revoked, A released. Only A parks.
    let phase = Arc::new(AtomicU8::new(0));
    let phase_hook = Arc::clone(&phase);
    state.device_keys.set_miss_barrier(Arc::new(move |_user, _device| {
        let phase = Arc::clone(&phase_hook);
        Box::pin(async move {
            if phase.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                while phase.load(Ordering::SeqCst) != 2 {
                    tokio::task::yield_now().await;
                }
            }
        })
    }));

    let a_state = state.clone();
    let body_a = submit_body_json("conv1", 0, "alice");
    let req_a = signed_request("alice", "dev-alice", now(), &sk, &body_a, None);
    let a = tokio::spawn(async move {
        status_of(build_router_with_state(a_state), req_a).await
    });

    while phase.load(Ordering::SeqCst) != 1 {
        tokio::task::yield_now().await;
    }
    assert!(
        state.device_keys.is_empty(),
        "A has not inserted yet — nothing is cached"
    );

    let revoke = serde_json::to_vec(&serde_json::json!({ "device_id": "dev-alice" })).unwrap();
    assert_eq!(
        signed_call(&state, "/v1/devices/revoke", "alice", "dev-alice", &sk, &revoke).await,
        StatusCode::OK
    );
    assert!(revoked_at(&db, "dev-alice").await.is_some(), "row tombstoned");

    phase.store(2, Ordering::SeqCst);
    assert_eq!(
        a.await.unwrap(),
        StatusCode::OK,
        "A itself was authenticated (it read the live key before the revoke)"
    );
    assert!(
        state.device_keys.is_empty(),
        "A's stale insert was discarded — no live key resurrected"
    );

    let since = since_body_json("conv1", 0, 9);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &sk, &since).await,
        StatusCode::UNAUTHORIZED,
        "a revoked device must NOT authenticate from a resurrected cache entry"
    );
    assert_eq!(
        recorded_since(&db, "conv1", "dev-alice").await,
        None,
        "and nothing it sent was applied"
    );
}

/// A tombstoned row still exists, so the revoked branch must not populate the cache.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_device_is_never_cached() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_revoked_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let cache = DeviceKeyCache::default();
    let t = now();
    let path = "/v1/commits/since";
    let body = since_body_json("conv1", 0, 1);
    let headers = signed_headers(path, "alice", "dev-alice", t, &sk, &body);

    for _ in 0..3 {
        assert!(
            verify_request_identity_cached(&cache, &db, &headers, "POST", path, &body, t)
                .await
                .is_err()
        );
        assert!(cache.is_empty(), "a revoked device is never cached");
    }
}

/// The lookup query names `account_suspension`, so dropping it fails every lookup.
async fn break_key_lookup(db: &Db) {
    db.conn()
        .await
        .unwrap()
        .execute("DROP TABLE account_suspension", ())
        .await
        .unwrap();
}

async fn status_and_error(router: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, v["error"].as_str().unwrap_or_default().to_string())
}

/// #1500: a DB failure during key lookup is 503 `unavailable`, not 401, and still a refusal.
#[tokio::test(flavor = "multi_thread")]
async fn a_database_failure_during_key_lookup_is_503_not_401() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;
    break_key_lookup(&db).await;

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);

    assert_eq!(
        status_and_error(router, req).await,
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable".to_string())
    );
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM mls_commit_log", ())
        .await
        .unwrap();
    let commits: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(commits, 0, "a request that could not be authenticated wrote nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_device_is_still_401_when_the_database_is_healthy() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let body = submit_body_json("conv1", 0, "alice");
    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let unknown = signed_request("alice", "dev-nobody", now(), &sk, &body, None);
    assert_eq!(
        status_and_error(router, unknown).await,
        (StatusCode::UNAUTHORIZED, "unauthorized".to_string())
    );

    let router = build_router_with_state(AppState::new(Arc::clone(&db), true));
    let bad_sig = b64(&[0u8; pollis_device_cert::MLDSA44_SIG_LEN]);
    let forged = signed_request("alice", "dev-alice", now(), &sk, &body, Some(&bad_sig));
    assert_eq!(
        status_and_error(router, forged).await,
        (StatusCode::UNAUTHORIZED, "unauthorized".to_string())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_key_lookup_never_authenticates_and_is_never_cached() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    break_key_lookup(&db).await;

    let t = now();
    let path = "/v1/commits/since";
    let body = since_body_json("conv1", 0, 1);
    let headers = signed_headers(path, "alice", "dev-alice", t, &sk, &body);

    let cache = DeviceKeyCache::default();
    assert_eq!(
        verify_request_identity_cached(&cache, &db, &headers, "POST", path, &body, t).await,
        Err(AuthRejection::Unavailable)
    );
    assert!(cache.is_empty(), "a failed lookup is not cached");

    let conn = db.conn().await.unwrap();
    assert_eq!(
        verify_request_identity(&conn, &headers, "POST", path, &body, t).await,
        Err(AuthRejection::Unavailable)
    );
}

const LONG_AGO: &str = "2020-01-01 00:00:00";

async fn set_last_seen(db: &Db, device_id: &str, value: &str) {
    let conn = db.conn().await.unwrap();
    conn.execute(
        "UPDATE user_device SET last_seen = ?2 WHERE device_id = ?1",
        libsql::params![device_id, value],
    )
    .await
    .unwrap();
}

async fn last_seen_of(db: &Db, device_id: &str) -> String {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT last_seen FROM user_device WHERE device_id = ?1",
            libsql::params![device_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().expect("device row").get(0).unwrap()
}

async fn await_last_seen_bumped(db: &Db, device_id: &str) {
    for _ in 0..200 {
        if last_seen_of(db, device_id).await != LONG_AGO {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("last_seen of {device_id} was never bumped");
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}

async fn verify_alice_at(cache: &DeviceKeyCache, db: &Db, sk: &SigningKey<MlDsa44>, t: i64) {
    let path = "/v1/commits/since";
    let body = since_body_json("conv1", 0, 1);
    let headers = signed_headers(path, "alice", "dev-alice", t, sk, &body);
    verify_request_identity_cached(cache, db, &headers, "POST", path, &body, t)
        .await
        .expect("verifies");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verified_request_bumps_last_seen_of_that_device_only() {
    let db = fresh_db().await;
    let alice = gen_signing_key();
    let sibling = gen_signing_key();
    let bob = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &alice.verifying_key()).await;
    seed_device(&db, "alice", "dev-alice-2", &sibling.verifying_key()).await;
    seed_device(&db, "bob", "dev-bob", &bob.verifying_key()).await;
    for d in ["dev-alice", "dev-alice-2", "dev-bob"] {
        set_last_seen(&db, d, LONG_AGO).await;
    }

    let state = AppState::new(Arc::clone(&db), true);
    let since = since_body_json("conv1", 0, 1);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &alice, &since).await,
        StatusCode::OK
    );

    await_last_seen_bumped(&db, "dev-alice").await;
    settle().await;
    assert_eq!(state.device_keys.last_seen().writes(), 1);
    assert_eq!(last_seen_of(&db, "dev-alice-2").await, LONG_AGO, "sibling untouched");
    assert_eq!(last_seen_of(&db, "dev-bob").await, LONG_AGO, "other account untouched");
}

#[tokio::test(flavor = "multi_thread")]
async fn last_seen_is_bumped_at_most_once_per_interval() {
    use pollis_delivery::last_seen::LAST_SEEN_BUMP_INTERVAL_SECS;
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    set_last_seen(&db, "dev-alice", LONG_AGO).await;

    let cache = DeviceKeyCache::default();
    let t0 = now();
    verify_alice_at(&cache, &db, &sk, t0).await;
    await_last_seen_bumped(&db, "dev-alice").await;
    assert_eq!(cache.last_seen().writes(), 1);

    set_last_seen(&db, "dev-alice", LONG_AGO).await;
    verify_alice_at(&cache, &db, &sk, t0 + 60).await;
    verify_alice_at(&cache, &db, &sk, t0 + LAST_SEEN_BUMP_INTERVAL_SECS - 1).await;
    settle().await;
    assert_eq!(cache.last_seen().writes(), 1, "no second write inside the interval");
    assert_eq!(last_seen_of(&db, "dev-alice").await, LONG_AGO);

    verify_alice_at(&cache, &db, &sk, t0 + LAST_SEEN_BUMP_INTERVAL_SECS).await;
    await_last_seen_bumped(&db, "dev-alice").await;
    assert_eq!(cache.last_seen().writes(), 2);
}

/// The write also skips a revoked row, covering a revoke between verify and UPDATE.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_device_is_never_bumped() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_revoked_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    set_last_seen(&db, "dev-alice", LONG_AGO).await;

    let state = AppState::new(Arc::clone(&db), true);
    let since = since_body_json("conv1", 0, 1);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &sk, &since).await,
        StatusCode::UNAUTHORIZED
    );
    settle().await;
    assert_eq!(state.device_keys.last_seen().writes(), 0);
    assert_eq!(last_seen_of(&db, "dev-alice").await, LONG_AGO);

    let conn = db.conn().await.unwrap();
    assert_eq!(
        pollis_delivery::last_seen::touch_last_seen(&conn, "alice", "dev-alice")
            .await
            .unwrap(),
        0,
        "the UPDATE itself refuses a revoked row"
    );
    assert_eq!(last_seen_of(&db, "dev-alice").await, LONG_AGO);
}

/// The key lookup and the write are both bound to user AND device id.
#[tokio::test(flavor = "multi_thread")]
async fn another_accounts_device_is_never_bumped() {
    let db = fresh_db().await;
    let alice = gen_signing_key();
    let bob = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &alice.verifying_key()).await;
    seed_device(&db, "bob", "dev-bob", &bob.verifying_key()).await;
    set_last_seen(&db, "dev-alice", LONG_AGO).await;

    let state = AppState::new(Arc::clone(&db), true);
    let since = since_body_json("conv1", 0, 1);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "bob", "dev-alice", &bob, &since).await,
        StatusCode::UNAUTHORIZED
    );
    settle().await;
    assert_eq!(state.device_keys.last_seen().writes(), 0);
    assert_eq!(last_seen_of(&db, "dev-alice").await, LONG_AGO);

    let conn = db.conn().await.unwrap();
    assert_eq!(
        pollis_delivery::last_seen::touch_last_seen(&conn, "bob", "dev-alice")
            .await
            .unwrap(),
        0,
        "the UPDATE is bound to the owner, not just the device id"
    );
    assert_eq!(last_seen_of(&db, "dev-alice").await, LONG_AGO);
}
