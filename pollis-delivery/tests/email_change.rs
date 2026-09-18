//! Email-change OTP endpoints (Goal B #419 final piece), driven through the real
//! axum router with `tower::oneshot` against a local libsql DB.
//!
//! Unlike the signup bootstrap (OTP-session-gated), these are DEVICE-SIGNED: the
//! user is already authenticated, so each request carries the four `X-Pollis-*`
//! signature headers and is verified against the seeded `user_device`
//! `mls_signature_pub_pq`. The OTP only proves control of the NEW mailbox.
//!
//! Coverage: the happy path, OTP wrong-code lockout, and the cross-user binding
//! (a different signed user can't consume someone else's pending change).

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
        ..OtpConfig::default()
    })
}

/// Seed a `users` row + a live device with `vk` as its signing key.
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

/// Build a device-signed POST for `path` with `body`.
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
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE, "current_code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "correct code should swap the email");
    assert_eq!(email_of(&db, "alice").await, new_email, "users.email must be updated");
}

// ── 1b. The change is not silent (L3) ────────────────────────────────────────

/// **L3.** The OTP proves control of the NEW mailbox and the device signature
/// proves the current account — but a stolen unlocked device satisfies both, and
/// the change used to leave no trace: the old address was never told and nothing
/// in the app recorded it, so the account's recovery address could be moved
/// silently.
///
/// The DS now writes the audit row itself, so a client cannot suppress it by
/// omitting a call, and it names the address that was left.
#[tokio::test(flavor = "multi_thread")]
async fn a_completed_email_change_is_recorded_against_the_account() {
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

/// The other half: a REFUSED change writes no audit row, so the log records what
/// happened rather than what was attempted through this endpoint (wrong-code
/// attempts are the OTP store's business, not the account log's).
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

// ── 1c. The CURRENT address has to approve the change (#1161) ────────────────

/// **#1161.** The device signature proves the account and the new-address code
/// proves the new mailbox — and a borrowed or stolen unlocked device satisfies
/// BOTH: it signs because the keystore is unlocked, and it receives the
/// new-address code because the attacker chose the destination. Nothing in the
/// flow ever asked the mailbox that actually owns the account. So the account's
/// recovery address could be moved by whoever picked up the device, with the
/// real owner finding out only from the notice sent afterwards.
///
/// A second code, to the address being LEFT, is what turns that into a change
/// the owner has to approve. FAILS before the change: the swap goes through.
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

    // Everything the old flow required, and nothing more: a valid signature and
    // the code from the mailbox the attacker chose.
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

    // An empty string is not an answer either — it is the same omission with a
    // key present.
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

    // And the control: with the current address's code, the same request lands.
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

/// A WRONG current-address code is refused the same way, and — because the
/// current-address challenge is checked FIRST — it does not burn an attempt
/// against the new address's counter, so the legitimate owner's retry is not
/// spent by somebody else's guessing.
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

    // More wrong current-address guesses than the new address's attempt budget.
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

    // The audit log records nothing: no change happened.
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM security_event WHERE user_id = 'alice'", ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 0);
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
            serde_json::json!({ "new_email": new_email, "code": "000000", "current_code": DEV_CODE }),
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
        serde_json::json!({ "new_email": new_email, "code": "000000", "current_code": DEV_CODE }),
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
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE, "current_code": DEV_CODE }),
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
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE, "current_code": DEV_CODE }),
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
        serde_json::json!({ "new_email": new_email, "code": DEV_CODE, "current_code": DEV_CODE }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(email_of(&db, "alice").await, new_email);
    assert_eq!(email_of(&db, "bob").await, "bob@x.com", "bob untouched");
}
