//! Pre-enrollment soft reset (#487): a verified-OTP session authenticates rotate/reset/purge.

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
    // An invalid device signature, which a session must not rescue.
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

/// [`login`] then version-1 identity establishment (the rotate CAS reads that head).
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

#[tokio::test(flavor = "multi_thread")]
async fn rotate_identity_rejects_missing_and_stale_credentials() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, _token) = login_established(&state, "alice@example.com", "dev-1").await;

    let (s, _) = send(&state, "/v1/account/rotate-identity", rotate_body(1), None, false).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "no credential must be rejected");

    let (s, _) = send(&state, "/v1/account/rotate-identity", rotate_body(1), Some("nonsense"), false).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "unknown session must be rejected");

    assert_eq!(identity_version(&db, &user_id).await, 1);
}

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

#[tokio::test(flavor = "multi_thread")]
async fn reset_recover_accepts_session_and_cleans_up() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, token) = login(&state, "alice@example.com", "dev-new").await;

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
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM user_device WHERE user_id = ?1 AND device_id = 'dev-old'", &user_id).await,
        0,
        "old device must be orphaned"
    );

    let (s, _) = send(&state, "/v1/welcomes/purge", serde_json::json!({}), Some(&token), false).await;
    assert_eq!(s, StatusCode::OK, "session-authorized purge should 200");
    assert_eq!(count(&db, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = ?1", &user_id).await, 0);
}

/// Seed what a soft reset must destroy; `current`'s own device row must survive.
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

    // The attack shape: rotate-identity alone, with only the OTP session.
    let (s, body) = send(&state, "/v1/account/rotate-identity", rotate_body(1), Some(&token), false).await;
    assert_eq!(s, StatusCode::OK, "session-authorized rotation should 200: {body}");
    assert_eq!(body["identity_version"], serde_json::json!(2));
    assert_eq!(identity_version(&db, &user_id).await, 2);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM account_key_log WHERE user_id = ?1", &user_id).await,
        2,
        "rotation must append the transparency log (v1 from establish, v2 from rotate)"
    );

    // The session-minted key owns nothing: memberships and key packages go with the rotation …
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
    // … and so does every other device; only the session's own device survives.
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
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = ?1", &user_id).await,
        0,
        "a session rotation must purge the pending Welcomes"
    );

    let events = rotation_events(&db, &user_id).await;
    assert_eq!(events.len(), 1, "exactly one DS-authored identity_rotated event: {events:?}");
    assert_eq!(events[0].0.as_deref(), Some("dev-new"), "attributed to the session's device");
    assert_eq!(
        events[0].1.as_deref(),
        Some("credential=session,new_identity_version=2"),
        "the audit row records that a bare OTP session did this"
    );

    // The client's follow-up reset-recover and purge are idempotent no-ops.
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

fn gen_signing_key() -> SigningKey<MlDsa44> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    SigningKey::<MlDsa44>::from_seed(&seed.into())
}

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

    // An enrolled device rotates without the wipe; the client runs reset-recover itself.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM group_member WHERE user_id = ?1", &user_id).await, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM dm_channel_member WHERE user_id = ?1", &user_id).await, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM mls_key_package WHERE user_id = ?1", &user_id).await, 1);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM user_device WHERE user_id = ?1", &user_id).await,
        2,
        "a signed rotation keeps every device"
    );

    let events = rotation_events(&db, &user_id).await;
    assert_eq!(events.len(), 1, "exactly one DS-authored identity_rotated event: {events:?}");
    assert_eq!(events[0].0.as_deref(), Some("dev-enrolled"), "attributed to the signing device");
    assert_eq!(
        events[0].1.as_deref(),
        Some("credential=signature,new_identity_version=2"),
    );
}

/// #1616: a fresh device reads its own account row with its OTP session, which is
/// all the soft reset needs before it can sign.
#[tokio::test(flavor = "multi_thread")]
async fn account_status_accepts_the_callers_own_otp_session() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, token) = login_established(&state, "alice@example.com", "dev-new").await;

    let (s, body) = send(
        &state,
        "/v1/read/account-status",
        serde_json::json!({ "user_id": user_id }),
        Some(&token),
        false,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "an OTP session reads its own row: {body}");
    assert_eq!(body["account"]["user_id"], serde_json::json!(user_id));
    assert_eq!(body["account"]["email"], "alice@example.com");
    assert_eq!(body["account"]["identity_version"], 1);

    // No body id at all: the session alone names the account.
    let (s, body) = send(&state, "/v1/read/account-status", serde_json::json!({}), Some(&token), false).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["account"]["user_id"], serde_json::json!(user_id));
}

/// The session never widens the read: another account's row (and email) stays a 403.
#[tokio::test(flavor = "multi_thread")]
async fn account_status_session_cannot_read_another_account() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (_alice, alice_token) = login_established(&state, "alice@example.com", "dev-1").await;
    let (bob, _bob_token) = login_established(&state, "bob@example.com", "dev-2").await;

    let (s, body) = send(
        &state,
        "/v1/read/account-status",
        serde_json::json!({ "user_id": bob }),
        Some(&alice_token),
        false,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "alice's session must not read bob's row");
    assert!(
        !body.to_string().contains("bob@example.com"),
        "a refused read must not carry the other account's email: {body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn account_status_rejects_missing_unknown_and_rescued_credentials() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, token) = login_established(&state, "alice@example.com", "dev-1").await;
    let body = serde_json::json!({ "user_id": user_id });

    let (s, _) = send(&state, "/v1/read/account-status", body.clone(), None, false).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "no credential must be rejected");

    let (s, _) = send(&state, "/v1/read/account-status", body.clone(), Some("nonsense"), false).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "an unknown session must be rejected");

    let (s, _) = send(&state, "/v1/read/account-status", body, Some(&token), true).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "a request offering a device signature must stand on that signature"
    );
}

/// Enrolled clients (every shipped build) keep reading it device-signed.
#[tokio::test(flavor = "multi_thread")]
async fn account_status_still_answers_a_device_signature() {
    let db = fresh_db().await;
    let state = authed_state(Arc::clone(&db));
    let (user_id, _token) = login_established(&state, "alice@example.com", "dev-enrolled").await;
    seed_account_state(&db, &user_id, "dev-enrolled").await;

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
        "/v1/read/account-status",
        serde_json::json!({ "user_id": user_id }),
        &user_id,
        "dev-enrolled",
        &sk,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "a device-signed read still 200s: {body}");
    assert_eq!(body["account"]["email"], "alice@example.com");
}
