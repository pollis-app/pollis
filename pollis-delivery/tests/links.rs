//! QR device links (#1207): a claimed link's session can only enroll one device.
//!
//! The refused endpoints are sent an empty body on purpose: past the gate an
//! empty body is a 400, so "OTP session → 400, link session → 401" proves the
//! GATE discriminates, not the parser.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use pollis_delivery::db::Db;
use pollis_delivery::otp::OtpConfig;
use pollis_delivery::{build_router_with_state, AppState};
use sha2::{Digest, Sha256};
use tower::ServiceExt as _;

mod common;

const USER: &str = "01J0000000000000000000ALICE";
const PHONE: &str = "phone-device-1";

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

async fn fresh_state() -> (common::TempDb, AppState) {
    let db = common::TempDb::open("links.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute(
        "INSERT INTO users (id, email, username, account_id_pub) VALUES (?1, 'alice@x.com', 'alice', ?2)",
        libsql::params![USER, vec![7u8; 1312]],
    )
    .await
    .unwrap();
    let arc: Arc<Db> = db.arc();
    let state = AppState::new(arc, true).with_otp_config(OtpConfig {
        session_ttl_secs: 600,
        ..OtpConfig::default()
    });
    (db, state)
}

async fn send(state: &AppState, path: &str, body: serde_json::Value, session: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method("POST").uri(path).header("content-type", "application/json");
    if let Some(tok) = session {
        builder = builder.header("X-Pollis-Session", tok);
    }
    let req = builder.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap();
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, val)
}

/// Create an open link as `/v1/link/create` would, and return the claim secret.
fn open_link(state: &AppState, link_id: &str) -> [u8; 32] {
    let claim = [42u8; 32];
    let verifier: [u8; 32] = Sha256::digest(claim).into();
    assert!(state.links.create(link_id, USER, verifier, pollis_delivery::util::now_unix()));
    claim
}

async fn claim(state: &AppState, link_id: &str, claim: &[u8]) -> (StatusCode, serde_json::Value) {
    send(
        state,
        "/v1/link/claim",
        serde_json::json!({ "link_id": link_id, "claim": b64(claim), "device_id": PHONE, "device_name": "Pixel 9" }),
        None,
    )
    .await
}

#[tokio::test]
async fn a_claim_mints_a_session_for_the_creating_account_once() {
    let (_db, state) = fresh_state().await;
    let secret = open_link(&state, "link-a");

    let (bad, _) = claim(&state, "link-a", &[1u8; 32]).await;
    assert_eq!(bad, StatusCode::UNAUTHORIZED, "a wrong secret is refused");

    let (ok, body) = claim(&state, "link-a", &secret).await;
    assert_eq!(ok, StatusCode::OK);
    assert_eq!(body["user_id"], USER);
    assert_eq!(body["email"], "alice@x.com");
    assert!(body["session_token"].as_str().is_some_and(|t| !t.is_empty()));

    let (again, _) = claim(&state, "link-a", &secret).await;
    assert_eq!(again, StatusCode::UNAUTHORIZED, "single use: the same secret cannot claim twice");
}

#[tokio::test]
async fn a_link_session_cannot_reach_the_otp_only_gates() {
    let (_db, state) = fresh_state().await;
    let secret = open_link(&state, "link-b");
    let (_, body) = claim(&state, "link-b", &secret).await;
    let link_session = body["session_token"].as_str().unwrap().to_string();
    let otp_session = state.sessions.mint(USER, "alice@x.com", PHONE, 600, pollis_delivery::util::now_unix());

    for path in [
        "/v1/account/rotate-identity",
        "/v1/account/reset-recover",
        "/v1/auth/establish-identity",
        "/v1/read/account-status",
    ] {
        let (link_status, _) = send(&state, path, serde_json::json!({}), Some(&link_session)).await;
        assert_eq!(link_status, StatusCode::UNAUTHORIZED, "{path}: a device-link session must be refused at the gate");
        let (otp_status, _) = send(&state, path, serde_json::json!({}), Some(&otp_session)).await;
        assert_ne!(otp_status, StatusCode::UNAUTHORIZED, "{path}: an OTP session passes the gate");
    }
}

#[tokio::test]
async fn a_link_session_registers_its_device_and_files_a_tagged_request() {
    let (_db, state) = fresh_state().await;
    let secret = open_link(&state, "link-c");
    let (_, body) = claim(&state, "link-c", &secret).await;
    let session = body["session_token"].as_str().unwrap().to_string();

    let (reg, _) = send(
        &state,
        "/v1/auth/register-device",
        serde_json::json!({ "device_id": PHONE, "device_name": "Pixel 9" }),
        Some(&session),
    )
    .await;
    assert_eq!(reg, StatusCode::OK, "a linked device registers with its link session");

    let now = chrono::Utc::now();
    let request = |tag: Option<&str>| {
        let mut b = serde_json::json!({
            "request_id": "req-1",
            "new_device_ephemeral_pub": b64(&[9u8; 32]),
            "verification_code": "ABCDEFGH",
            "created_at": now.to_rfc3339(),
            "expires_at": (now + chrono::Duration::minutes(10)).to_rfc3339(),
        });
        if let Some(t) = tag {
            b["link_tag"] = serde_json::json!(t);
        }
        b
    };

    let (untagged, _) = send(&state, "/v1/auth/enrollment-request", request(None), Some(&session)).await;
    assert_eq!(untagged, StatusCode::BAD_REQUEST, "a device-link session must carry a link tag");

    let (tagged, _) = send(&state, "/v1/auth/enrollment-request", request(Some("dGFn")), Some(&session)).await;
    assert_eq!(tagged, StatusCode::OK);

    let st = state.links.status("link-c", USER, pollis_delivery::util::now_unix());
    assert_eq!(st.state, pollis_delivery::links::LinkState::Requested);
    assert_eq!(st.request_id.as_deref(), Some("req-1"));
    assert_eq!(st.link_tag.as_deref(), Some("dGFn"));
    assert_eq!(st.device_name.as_deref(), Some("Pixel 9"));
}

// v2: the new device shows the QR (#1361)

const DESKTOP: &str = "desktop-device-1";

async fn offer(state: &AppState, link_id: &str, poll_secret: &[u8; 32]) -> StatusCode {
    let verifier: [u8; 32] = Sha256::digest(poll_secret).into();
    let (status, _) = send(
        state,
        "/v1/link/offer",
        serde_json::json!({ "link_id": link_id, "poll_verifier": b64(&verifier), "device_id": DESKTOP, "device_name": "Mac 10.0.0.1" }),
        None,
    )
    .await;
    status
}

async fn poll(state: &AppState, link_id: &str, poll_secret: &[u8]) -> (StatusCode, serde_json::Value) {
    send(
        state,
        "/v1/link/poll-offer",
        serde_json::json!({ "link_id": link_id, "poll_secret": b64(poll_secret) }),
        None,
    )
    .await
}

#[tokio::test]
async fn an_offer_yields_one_enrollment_only_session_after_it_is_accepted() {
    let (_db, state) = fresh_state().await;
    let secret = [3u8; 32];
    assert_eq!(offer(&state, "offer-a", &secret).await, StatusCode::OK);

    let (waiting, body) = poll(&state, "offer-a", &secret).await;
    assert_eq!(waiting, StatusCode::OK);
    assert!(body["session"].is_null());
    let (guess, _) = poll(&state, "offer-a", &[4u8; 32]).await;
    assert_eq!(guess, StatusCode::UNAUTHORIZED);

    assert!(matches!(
        state.links.accept("offer-a", USER, pollis_delivery::util::now_unix()),
        pollis_delivery::links::AcceptOutcome::Accepted { .. }
    ));
    // The device name had its IP-shaped text redacted before anyone saw it.
    let st = state.links.status("offer-a", USER, pollis_delivery::util::now_unix());
    assert!(!st.device_name.as_deref().unwrap_or_default().contains("10.0.0.1"), "{:?}", st.device_name);

    let (ok, body) = poll(&state, "offer-a", &secret).await;
    assert_eq!(ok, StatusCode::OK);
    assert_eq!(body["session"]["user_id"], USER);
    let session = body["session"]["session_token"].as_str().unwrap().to_string();
    let (again, _) = poll(&state, "offer-a", &secret).await;
    assert_eq!(again, StatusCode::UNAUTHORIZED);

    for path in [
        "/v1/account/rotate-identity",
        "/v1/account/reset-recover",
        "/v1/auth/establish-identity",
        "/v1/read/account-status",
    ] {
        let (status, _) = send(&state, path, serde_json::json!({}), Some(&session)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: a device-offer session must be refused at the gate");
    }
    let (reg, _) = send(
        &state,
        "/v1/auth/register-device",
        serde_json::json!({ "device_id": DESKTOP, "device_name": "Mac" }),
        Some(&session),
    )
    .await;
    assert_eq!(reg, StatusCode::OK);
    let now = chrono::Utc::now();
    let request = |tag: Option<&str>| {
        let mut b = serde_json::json!({
            "request_id": "req-v2",
            "new_device_ephemeral_pub": b64(&[9u8; 32]),
            "verification_code": "ABCDEFGH",
            "created_at": now.to_rfc3339(),
            "expires_at": (now + chrono::Duration::minutes(10)).to_rfc3339(),
        });
        if let Some(t) = tag {
            b["link_tag"] = serde_json::json!(t);
        }
        b
    };
    let (tagged, _) = send(&state, "/v1/auth/enrollment-request", request(Some("dGFn")), Some(&session)).await;
    assert_eq!(tagged, StatusCode::BAD_REQUEST, "a device-offer request carries no tag");
    let (untagged, _) = send(&state, "/v1/auth/enrollment-request", request(None), Some(&session)).await;
    assert_eq!(untagged, StatusCode::OK);

    let st = state.links.status("offer-a", USER, pollis_delivery::util::now_unix());
    assert_eq!(st.state, pollis_delivery::links::LinkState::Requested);
    assert_eq!(st.request_id.as_deref(), Some("req-v2"));
    assert_eq!(st.link_tag, None);
    assert!(state.links.bound_request_ids(USER, pollis_delivery::util::now_unix()).contains("req-v2"));
}

#[tokio::test]
async fn an_offer_id_cannot_be_reused_while_live() {
    let (_db, state) = fresh_state().await;
    assert_eq!(offer(&state, "offer-b", &[1u8; 32]).await, StatusCode::OK);
    assert_eq!(offer(&state, "offer-b", &[2u8; 32]).await, StatusCode::TOO_MANY_REQUESTS);
    let (st, _) = poll(&state, "offer-b", &[2u8; 32]).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_otp_session_cannot_smuggle_a_link_tag() {
    let (_db, state) = fresh_state().await;
    let otp_session = state.sessions.mint(USER, "alice@x.com", PHONE, 600, pollis_delivery::util::now_unix());
    let now = chrono::Utc::now();
    let (status, _) = send(
        &state,
        "/v1/auth/enrollment-request",
        serde_json::json!({
            "request_id": "req-2",
            "new_device_ephemeral_pub": b64(&[9u8; 32]),
            "verification_code": "ABCDEFGH",
            "created_at": now.to_rfc3339(),
            "expires_at": (now + chrono::Duration::minutes(10)).to_rfc3339(),
            "link_tag": "dGFn",
        }),
        Some(&otp_session),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
