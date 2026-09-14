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
//!      valid session — the stronger credential, once offered, must verify;
//!   5. a session-authenticated rotation IS the reset: the memberships, key
//!      packages and every OTHER device are gone in the same transaction as the
//!      key rotation, and the DS itself appends an `identity_rotated` security
//!      event naming the credential. An email OTP alone can never mint a key
//!      that inherits the account (the finding this closes: rotate WITHOUT
//!      reset-recover used to leave a session-minted key on a fully-populated
//!      account, with no audit row);
//!   6. a device-SIGNED rotation stays a plain rotation (memberships kept) and
//!      is audited as `credential=signature`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey};
use pollis_delivery::auth::canonical_message;
use pollis_delivery::db::Db;
use pollis_delivery::otp::OtpConfig;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

mod common;


async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("ds.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db
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
        ..OtpConfig::default()
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
            "account_id_pub": b64(&[9u8; pollis_device_cert::MLDSA44_PUB_LEN]),
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
        "account_id_pub": b64(&[7u8; pollis_device_cert::MLDSA44_PUB_LEN]),
        "salt": b64(&[1u8; 16]),
        "nonce": b64(&[2u8; 12]),
        "wrapped_key": b64(&[3u8; 48]),
    })
}

async fn identity_version(db: &Db, user_id: &str) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query("SELECT identity_version FROM users WHERE id = ?1", libsql::params![user_id])
        .await
        .unwrap();
    rows.next().await.unwrap().expect("user row").get(0).unwrap()
}

async fn count(db: &Db, sql: &str, user_id: &str) -> i64 {
    let conn = db.conn().await.unwrap();
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
        let conn = db.conn().await.unwrap();
        for sql in [
            "INSERT INTO conversation (id, kind) VALUES ('g1', 'group')".to_string(),
            format!("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', '{user_id}')"),
            format!("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', '{user_id}', 'admin')"),
            "INSERT INTO users (id, email, username) VALUES ('other', 'o@x.com', 'other')".to_string(),
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'other', 'admin')".to_string(),
            format!("INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', '{user_id}', 'creator')"),
            format!("INSERT INTO mls_key_package (ref_hash, user_id, key_package) VALUES ('kp1', '{user_id}', x'00')"),
            format!("INSERT INTO user_device (device_id, user_id) VALUES ('dev-old', '{user_id}')"),
            format!("INSERT INTO mls_welcome (id, conversation_id, recipient_id, welcome_data) VALUES ('w1', 'g1', '{user_id}', x'00')"),
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

// ── 6. A session rotation IS the reset: rotate + wipe in ONE transaction ─────

/// Seed the account state a soft reset must destroy: a group membership (with
/// a co-admin so ownership handoff is trivial), a DM membership, a key package,
/// an OLD enrolled device, and a pending Welcome. `current` is the device
/// performing the reset — its row must SURVIVE (it re-enrolls under the new
/// identity).
async fn seed_account_state(db: &Db, user_id: &str, current: &str) {
    let conn = db.conn().await.unwrap();
    for sql in [
        "INSERT INTO conversation (id, kind) VALUES ('g1', 'group')".to_string(),
        format!("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', '{user_id}')"),
        format!("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', '{user_id}', 'admin')"),
        "INSERT INTO users (id, email, username) VALUES ('other', 'o@x.com', 'other')".to_string(),
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'other', 'admin')".to_string(),
        format!("INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', '{user_id}', 'creator')"),
        format!("INSERT INTO mls_key_package (ref_hash, user_id, key_package) VALUES ('kp1', '{user_id}', x'00')"),
        format!("INSERT INTO user_device (device_id, user_id) VALUES ('dev-old', '{user_id}')"),
        format!("INSERT INTO user_device (device_id, user_id) VALUES ('{current}', '{user_id}')"),
        format!("INSERT INTO mls_welcome (id, conversation_id, recipient_id, welcome_data) VALUES ('w1', 'g1', '{user_id}', x'00')"),
    ] {
        conn.execute(&sql, ()).await.expect("seed");
    }
}

/// `(device_id, metadata)` of every `identity_rotated` security event the DS
/// wrote for `user_id`.
async fn rotation_events(db: &Db, user_id: &str) -> Vec<(Option<String>, Option<String>)> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT device_id, metadata FROM security_event \
             WHERE user_id = ?1 AND kind = 'identity_rotated' ORDER BY created_at",
            libsql::params![user_id],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push((row.get(0).unwrap(), row.get(1).unwrap()));
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn session_rotation_is_the_reset_and_is_audited_by_the_ds() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, token) = login_established(&state, "alice@example.com", "dev-new").await;
    seed_account_state(&db, &user_id, "dev-new").await;

    // The attack shape: rotate-identity ALONE, with nothing but the OTP session,
    // and never call reset-recover.
    let (s, body) = send(&state, "/v1/account/rotate-identity", rotate_body(1), Some(&token), false).await;
    assert_eq!(s, StatusCode::OK, "session-authorized rotation should 200: {body}");
    assert_eq!(body["identity_version"], serde_json::json!(2));
    assert_eq!(identity_version(&db, &user_id).await, 2);

    // The session-minted key owns NOTHING: every membership and key package is
    // gone in the same transaction as the rotation …
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM group_member WHERE user_id = ?1", &user_id).await,
        0,
        "a session rotation must strip every group membership"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM dm_channel_member WHERE user_id = ?1", &user_id).await,
        0,
        "a session rotation must strip every DM membership"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM mls_key_package WHERE user_id = ?1", &user_id).await,
        0,
        "a session rotation must drop the stale key packages"
    );
    // … and so is every OTHER device — the victim's real devices cannot keep
    // authenticating alongside the attacker's key. Only the session's own device
    // (server-bound, never the body) survives.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM user_device WHERE user_id = ?1 AND device_id != 'dev-new'",
            &user_id
        )
        .await,
        0,
        "a session rotation must revoke every other enrolled device"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM user_device WHERE user_id = ?1 AND device_id = 'dev-new'",
            &user_id
        )
        .await,
        1,
        "the resetting device's own row is kept"
    );
    // Pending Welcomes are addressed to an identity that no longer exists
    // (single-DB fixture: the log DB is the main DB).
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = ?1", &user_id).await,
        0,
        "a session rotation must purge the pending Welcomes"
    );

    // The DS wrote the audit row itself, naming the credential — the client
    // never had a chance to omit it.
    let events = rotation_events(&db, &user_id).await;
    assert_eq!(events.len(), 1, "exactly one DS-authored identity_rotated event: {events:?}");
    assert_eq!(events[0].0.as_deref(), Some("dev-new"), "attributed to the session's device");
    assert_eq!(
        events[0].1.as_deref(),
        Some("credential=session,new_identity_version=2"),
        "the audit row records that a bare OTP session did this"
    );

    // The client's soft-reset flow still calls reset-recover and the Welcome
    // purge after the rotation; both are now idempotent no-ops, not errors.
    let (s, body) = send(
        &state,
        "/v1/account/reset-recover",
        serde_json::json!({ "current_device_id": "dev-new" }),
        Some(&token),
        false,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "reset-recover after a session rotation must still 200: {body}");
    let (s, _) = send(&state, "/v1/welcomes/purge", serde_json::json!({}), Some(&token), false).await;
    assert_eq!(s, StatusCode::OK, "welcomes purge after a session rotation must still 200");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM user_device WHERE user_id = ?1", &user_id).await,
        1,
        "the follow-up reset-recover keeps the same device and nothing else"
    );
}

// ── 7. A device-SIGNED rotation stays a plain rotation, audited as such ──────

/// Mint an ML-DSA-44 signing key (its private key IS the 32-byte seed).
fn gen_signing_key() -> SigningKey<MlDsa44> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    SigningKey::<MlDsa44>::from_seed(&seed.into())
}

/// A device-signed POST, exactly as an enrolled pollis-core client signs one.
async fn send_signed(
    state: &AppState,
    path: &str,
    body: serde_json::Value,
    user_id: &str,
    device_id: &str,
    sk: &SigningKey<MlDsa44>,
) -> (StatusCode, serde_json::Value) {
    let bytes = serde_json::to_vec(&body).unwrap();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let msg = canonical_message("POST", path, ts, &bytes);
    let sig = base64::engine::general_purpose::STANDARD.encode(sk.sign(&msg).encode());
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("X-Pollis-User", user_id)
        .header("X-Pollis-Device", device_id)
        .header("X-Pollis-Timestamp", ts.to_string())
        .header("X-Pollis-Signature", sig)
        .body(Body::from(bytes))
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

#[tokio::test(flavor = "multi_thread")]
async fn signed_rotation_keeps_memberships_and_is_audited_as_signature() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, _token) = login_established(&state, "alice@example.com", "dev-enrolled").await;
    seed_account_state(&db, &user_id, "dev-enrolled").await;

    // Enroll the signing device: give its row the pubkey the auth gate verifies
    // against.
    let sk = gen_signing_key();
    {
        let conn = db.conn().await.unwrap();
        conn.execute(
            "UPDATE user_device SET mls_signature_pub_pq = ?1 \
             WHERE device_id = 'dev-enrolled' AND user_id = ?2",
            libsql::params![sk.verifying_key().encode().to_vec(), user_id.clone()],
        )
        .await
        .unwrap();
    }

    let (s, body) = send_signed(
        &state,
        "/v1/account/rotate-identity",
        rotate_body(1),
        &user_id,
        "dev-enrolled",
        &sk,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "device-signed rotation should 200: {body}");
    assert_eq!(identity_version(&db, &user_id).await, 2);

    // An enrolled device holding the account key is trusted to rotate WITHOUT
    // the wipe (the client follows up with reset-recover itself).
    assert_eq!(count(&db, "SELECT COUNT(*) FROM group_member WHERE user_id = ?1", &user_id).await, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM dm_channel_member WHERE user_id = ?1", &user_id).await, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM mls_key_package WHERE user_id = ?1", &user_id).await, 1);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM user_device WHERE user_id = ?1", &user_id).await,
        2,
        "a signed rotation keeps every device"
    );

    // … but it is still audited by the DS, and the row says a signature did it.
    let events = rotation_events(&db, &user_id).await;
    assert_eq!(events.len(), 1, "exactly one DS-authored identity_rotated event: {events:?}");
    assert_eq!(events[0].0.as_deref(), Some("dev-enrolled"), "attributed to the signing device");
    assert_eq!(
        events[0].1.as_deref(),
        Some("credential=signature,new_identity_version=2"),
    );
}
