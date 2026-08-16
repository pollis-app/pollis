//! Device-certificate-signature auth, driven through the real axum router with
//! `tower::oneshot` against a local libsql DB.
//!
//! These tests are the executable spec for the signing contract the pollis-core
//! client must match: headers `X-Pollis-{User,Device,Timestamp,Signature}` over
//! the canonical message `{METHOD}\n{PATH}\n{TS}\n{hex(sha256(body))}`, signed
//! with the device's raw-1312-byte-pubkey ML-DSA-44 key (the same
//! `user_device.mls_signature_pub_pq` openmls produces).

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey, VerifyingKey};
use http_body_util::BodyExt as _;
use pollis_delivery::auth::{
    canonical_message, verify_request_identity_cached, DeviceKeyCache, DEVICE_KEY_CACHE_TTL_SECS,
};
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


fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

async fn fresh_db() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("delivery.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    pollis_schema::apply::single_db(&db.conn().unwrap()).await.expect("schema");
    Arc::new(db)
}

/// Make sure `user_id` exists. `user_device.user_id` and `group_member.user_id`
/// are real foreign keys in the shipped schema, and libsql's local backend
/// enforces them on any connection that has not been told otherwise — so a
/// fixture that invents a user id is only valid because nothing was looking.
async fn seed_user(db: &Db, user_id: &str) {
    let conn = db.conn().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO users (id, email, username) VALUES (?1, ?1 || '@x', ?1)",
        libsql::params![user_id],
    )
    .await
    .unwrap();
}

/// Seed a live (non-revoked) device row with the given raw 1312-byte pubkey —
/// exactly the bytes openmls `to_public_vec()` yields for ML-DSA-44.
async fn seed_device(db: &Db, user_id: &str, device_id: &str, vk: &VerifyingKey<MlDsa44>) {
    seed_user(db, user_id).await;
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
    seed_user(db, user_id).await;
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
    seed_user(db, user_id).await;
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

// ── Device-pubkey cache (#658) ────────────────────────────────────────────────
//
// The cache removes the per-request Turso read of `user_device.mls_signature_pub_pq`
// (2000–4700 ms from the DS's network position). Every test below is written so
// that it can ONLY pass because of the cache, or can only pass because the cache
// was correctly evicted — never "the DB happened to agree".
//
// The trick used throughout: mutate `user_device` DIRECTLY over the connection,
// behind the DS's back. No endpoint ran, so nothing evicted. A subsequent
// success therefore proves the answer came from cache (no DB read happened), and
// after a real endpoint runs, a subsequent rejection proves the entry was evicted.

/// Rip a device row out from under the DS without going through any endpoint —
/// so no invalidation hook fires. Used to prove whether an answer came from the
/// cache or from the DB.
async fn delete_device_row_behind_the_ds(db: &Db, device_id: &str) {
    db.conn()
        .unwrap()
        .execute(
            "DELETE FROM user_device WHERE device_id = ?1",
            libsql::params![device_id],
        )
        .await
        .unwrap();
}

/// Overwrite a device's registered pubkey directly, again bypassing every
/// endpoint (and therefore every eviction hook).
async fn set_device_pubkey_behind_the_ds(db: &Db, device_id: &str, vk: &VerifyingKey<MlDsa44>) {
    db.conn()
        .unwrap()
        .execute(
            "UPDATE user_device SET mls_signature_pub_pq = ?1 WHERE device_id = ?2",
            libsql::params![pq_pub(vk), device_id],
        )
        .await
        .unwrap();
}

/// `revoked_at` for a device — `None` when the row is absent or live.
async fn revoked_at(db: &Db, device_id: &str) -> Option<String> {
    let conn = db.conn().unwrap();
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
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT 1 FROM user_device WHERE device_id = ?1",
            libsql::params![device_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

/// Drive one signed POST through a fresh router built on `state` — the SAME
/// `AppState`, so every call shares one `device_keys` cache (exactly as in the
/// single-instance DS, where axum clones the state per request).
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

/// The four auth headers as a bare `HeaderMap`, for the tests that call
/// `verify_request_identity_cached` directly to pin the clock.
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

/// HAPPY PATH / the actual speedup: the second signed request for a device is
/// served WITHOUT touching the DB.
///
/// Proven the strongest way available without a query counter: after the first
/// (cache-populating) request, the device row is deleted directly over the
/// connection — bypassing every endpoint, so nothing evicts. A DB-backed lookup
/// would now return "unknown device" → 401. The request still succeeds, which is
/// only possible if the verifying key came from the cache and no Turso read
/// happened. That elided read is precisely the 2000–4700 ms of #658.
///
/// The two calls also deliberately hit the two DIFFERENT verify call sites
/// (`submit` → `verify_request_cached`, `report_commit_since` →
/// `verify_request_identity_cached`), proving they share one cache.
#[tokio::test(flavor = "multi_thread")]
async fn cached_device_authenticates_without_re_reading_the_row() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let state = AppState::new(Arc::clone(&db), true);
    assert!(state.device_keys.is_empty(), "cache starts cold");

    // 1st call — cache miss, one DB read, entry cached.
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

    // Remove the ONLY source of truth the DB path could have used.
    delete_device_row_behind_the_ds(&db, "dev-alice").await;
    assert!(!device_row_exists(&db, "dev-alice").await);

    // 2nd call — must still authenticate, and can only have done so from cache.
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

/// **THE security regression test.** A device that authenticated successfully a
/// moment ago — and is therefore sitting in the cache — must NOT be able to
/// authenticate once it is revoked.
///
/// If the eviction in `account::revoke_device` is removed, the final assertion
/// flips from 401 to 200 and this test fails: the revoked device keeps writing
/// for up to `DEVICE_KEY_CACHE_TTL_SECS`. That is the whole risk this change
/// takes on, so it is asserted end-to-end through the real router.
#[tokio::test(flavor = "multi_thread")]
async fn revoked_device_cannot_authenticate_from_a_stale_cache_entry() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let state = AppState::new(Arc::clone(&db), true);

    // 1. It authenticates NOW, and is cached.
    let body = submit_body_json("conv1", 0, "alice");
    let req = signed_request("alice", "dev-alice", now(), &sk, &body, None);
    assert_eq!(
        status_of(build_router_with_state(state.clone()), req).await,
        StatusCode::OK
    );
    assert_eq!(state.device_keys.len(), 1, "cached before revocation");

    // 2. Revoke it through the real endpoint (self-scoped: alice revokes her own
    //    device, signing as that device).
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

    // 3. The same, still perfectly valid, signature must now be refused.
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

/// Logout DELETEs the device's own row. Same property as revocation: the cached
/// entry must go with it, or a logged-out device keeps writing until the TTL.
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

/// `POST /v1/devices/resign` rewrites only the cert columns — it deliberately
/// does NOT touch `mls_signature_pub_pq`, so no cached key can be wrong. It still
/// evicts, so that stays true if the statement ever grows to touch the key
/// columns. Asserted both ways: the entry is dropped, and the device still
/// authenticates afterwards (re-read from the DB) rather than being locked out.
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

/// The pubkey really CHANGING is `POST /v1/auth/publish-device-cert` — the one
/// write that rewrites `mls_signature_pub_pq` on a live row. Without eviction the
/// device would be locked out (its new signatures checked against the old cached
/// key) AND the old key would stay accepted. Both halves are asserted.
#[tokio::test(flavor = "multi_thread")]
async fn republished_cert_swaps_the_cached_pubkey() {
    let db = fresh_db().await;
    let old_sk = gen_signing_key();
    let new_sk = gen_signing_key();
    let account = gen_signing_key();

    // The account identity the device cert must chain to.
    db.conn()
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

    // Warm the cache with the OLD key.
    let since = since_body_json("conv1", 0, 1);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &old_sk, &since).await,
        StatusCode::OK
    );
    assert_eq!(state.device_keys.len(), 1, "old key cached");

    // Publish a new cert carrying a NEW pq signing key (cert-validity gate: no
    // session header, so the cert signature against `account_id_pub` is the proof).
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

    // The NEW key works (it would not, if the old one were still cached) …
    let since = since_body_json("conv1", 0, 5);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &new_sk, &since).await,
        StatusCode::OK,
        "the re-keyed device authenticates with its new key"
    );
    // … and the OLD key does not.
    let since = since_body_json("conv1", 0, 6);
    assert_eq!(
        signed_call(&state, "/v1/commits/since", "alice", "dev-alice", &old_sk, &since).await,
        StatusCode::UNAUTHORIZED,
        "the superseded key is no longer accepted"
    );
}

/// The TTL backstop: even with NO eviction hook involved at all, an entry stops
/// being served after `DEVICE_KEY_CACHE_TTL_SECS`. This is the defence-in-depth
/// for a future endpoint that forgets to evict.
///
/// Driven through `verify_request_identity_cached` directly so the clock is
/// injected — no 30s sleep in CI.
#[tokio::test(flavor = "multi_thread")]
async fn ttl_backstop_expires_a_cached_entry() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let cache = DeviceKeyCache::default();
    let conn = db.conn().unwrap();
    let t0 = now();
    let body = since_body_json("conv1", 0, 1);
    let headers = signed_headers("/v1/commits/since", "alice", "dev-alice", t0, &sk, &body);
    let path = "/v1/commits/since";

    // Populate.
    assert!(
        verify_request_identity_cached(&cache, &conn, &headers, "POST", path, &body, t0)
            .await
            .is_ok()
    );
    assert_eq!(cache.len(), 1);

    // Delete the row behind the DS's back: NO endpoint ran, so NO eviction hook
    // fired. Only the TTL can save us now.
    delete_device_row_behind_the_ds(&db, "dev-alice").await;

    // One second inside the TTL — still served from cache.
    assert!(
        verify_request_identity_cached(
            &cache,
            &conn,
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

    // At the TTL — the entry is dropped, the DB says "unknown device", 401.
    assert!(
        verify_request_identity_cached(
            &cache,
            &conn,
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

/// A cached entry that has gone stale because the pubkey ROTATED is corrected by
/// `invalidate_device` — the primitive every eviction hook calls. Documents the
/// hazard (stale key → the real device is locked out) and that eviction is the fix.
#[tokio::test(flavor = "multi_thread")]
async fn invalidate_device_forces_a_re_read_of_a_rotated_pubkey() {
    let db = fresh_db().await;
    let old_sk = gen_signing_key();
    let new_sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &old_sk.verifying_key()).await;

    let cache = DeviceKeyCache::default();
    let conn = db.conn().unwrap();
    let t = now();
    let path = "/v1/commits/since";
    let body = since_body_json("conv1", 0, 1);

    let old_headers = signed_headers(path, "alice", "dev-alice", t, &old_sk, &body);
    assert!(
        verify_request_identity_cached(&cache, &conn, &old_headers, "POST", path, &body, t)
            .await
            .is_ok()
    );

    // Rotate the registered key with no eviction.
    set_device_pubkey_behind_the_ds(&db, "dev-alice", &new_sk.verifying_key()).await;
    let new_headers = signed_headers(path, "alice", "dev-alice", t, &new_sk, &body);
    assert!(
        verify_request_identity_cached(&cache, &conn, &new_headers, "POST", path, &body, t)
            .await
            .is_err(),
        "the stale cached key rejects the device's real, current signature"
    );

    // Eviction is the fix.
    cache.invalidate_device("alice", "dev-alice");
    assert!(
        verify_request_identity_cached(&cache, &conn, &new_headers, "POST", path, &body, t)
            .await
            .is_ok(),
        "after eviction the current key is re-read and accepted"
    );
    assert!(
        verify_request_identity_cached(&cache, &conn, &old_headers, "POST", path, &body, t)
            .await
            .is_err(),
        "and the superseded key is refused"
    );
}

/// Negatives are NOT cached — the decision that keeps enrollment working. A
/// device that does not exist yet is rejected, and when its row appears moments
/// later (the register-device → publish-device-cert bootstrap) it authenticates
/// immediately, with no cached "unknown device" to wait out.
#[tokio::test(flavor = "multi_thread")]
async fn a_miss_is_never_cached_so_a_new_device_is_not_locked_out() {
    let db = fresh_db().await;
    let sk = gen_signing_key();

    let cache = DeviceKeyCache::default();
    let conn = db.conn().unwrap();
    let t = now();
    let path = "/v1/commits/since";
    let body = since_body_json("conv1", 0, 1);
    let headers = signed_headers(path, "alice", "dev-new", t, &sk, &body);

    // No row yet → 401, and nothing is remembered about the miss.
    assert!(
        verify_request_identity_cached(&cache, &conn, &headers, "POST", path, &body, t)
            .await
            .is_err()
    );
    assert!(cache.is_empty(), "a negative lookup is never cached");

    // Enrollment completes.
    seed_device(&db, "alice", "dev-new", &sk.verifying_key()).await;

    // The very next request works — no TTL to wait out.
    assert!(
        verify_request_identity_cached(&cache, &conn, &headers, "POST", path, &body, t)
            .await
            .is_ok(),
        "a freshly-enrolled device authenticates immediately"
    );
}

/// **THE #721 regression test — the read-then-evict race, reproduced
/// deterministically.**
///
/// The window the TTL used to merely bound: request A reads a *live* device row,
/// then — before A inserts its cache entry — a revoke commits and evicts. Pre-fix,
/// A's insert lands *after* the eviction and resurrects the revoked key for up to
/// the TTL. This test forces exactly that interleaving with a scheduling hook (the
/// cache's miss barrier), not a sleep, so it fails reliably against the pre-fix
/// code and passes with the per-key generation guard.
///
/// Sequence (each step strictly ordered by the barrier + an atomic phase flag):
///   1. A hits the cache-miss path, reads the still-valid row, and parks in the
///      barrier — *before* inserting.
///   2. The test revokes dev-alice through the real endpoint: the row is
///      tombstoned and `invalidate_device` bumps the key's generation.
///   3. A is released and runs its insert. The generation it captured at step 1 is
///      now stale, so the insert is discarded — the revoked key is NOT resurrected.
///   4. A fresh request from the revoked device is refused (401), proving nothing
///      stale is being served from the cache.
///
/// Against the pre-fix cache (`insert` unconditional), step 3 repopulates the live
/// key and step 4 returns 200 — the test fails, which is the point.
#[tokio::test(flavor = "multi_thread")]
async fn read_then_evict_race_does_not_resurrect_a_revoked_key() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;
    seed_group_membership(&db, "conv1", "alice").await;

    let state = AppState::new(Arc::clone(&db), true);

    // Phase flag: 0 = initial, 1 = A parked in the barrier (row already read),
    // 2 = test has revoked and releases A. Only the FIRST caller (request A) parks;
    // every later miss (the revoke's own auth, request B) sails straight through.
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

    // Request A: submit a commit. It authenticates (reads the live key), then parks
    // in the barrier before its cache insert.
    let a_state = state.clone();
    let body_a = submit_body_json("conv1", 0, "alice");
    let req_a = signed_request("alice", "dev-alice", now(), &sk, &body_a, None);
    let a = tokio::spawn(async move {
        status_of(build_router_with_state(a_state), req_a).await
    });

    // Wait — deterministically, no sleep — until A has read the row and parked.
    while phase.load(Ordering::SeqCst) != 1 {
        tokio::task::yield_now().await;
    }
    assert!(
        state.device_keys.is_empty(),
        "A has not inserted yet — nothing is cached"
    );

    // Revoke dev-alice through the real endpoint: tombstones the row AND bumps the
    // key's generation via `invalidate_device`.
    let revoke = serde_json::to_vec(&serde_json::json!({ "device_id": "dev-alice" })).unwrap();
    assert_eq!(
        signed_call(&state, "/v1/devices/revoke", "alice", "dev-alice", &sk, &revoke).await,
        StatusCode::OK
    );
    assert!(revoked_at(&db, "dev-alice").await.is_some(), "row tombstoned");

    // Release A: its insert now carries a stale generation and must be discarded.
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

    // The revoked device must now be refused. Pre-fix, A's insert would have made
    // this a cache hit and this would be 200.
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

/// A revoked device's row still EXISTS (it is tombstoned, not deleted), so the
/// "revoked" branch of `lookup_device_pubkey` must also refuse to populate the
/// cache — otherwise the very next request would be served a revoked key.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_device_is_never_cached() {
    let db = fresh_db().await;
    let sk = gen_signing_key();
    seed_revoked_device(&db, "alice", "dev-alice", &sk.verifying_key()).await;

    let cache = DeviceKeyCache::default();
    let conn = db.conn().unwrap();
    let t = now();
    let path = "/v1/commits/since";
    let body = since_body_json("conv1", 0, 1);
    let headers = signed_headers(path, "alice", "dev-alice", t, &sk, &body);

    for _ in 0..3 {
        assert!(
            verify_request_identity_cached(&cache, &conn, &headers, "POST", path, &body, t)
                .await
                .is_err()
        );
        assert!(cache.is_empty(), "a revoked device is never cached");
    }
}
