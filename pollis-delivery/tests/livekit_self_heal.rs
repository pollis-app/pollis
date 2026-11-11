//! #1751 — the DS → LiveKit path heals itself and says when it cannot.
//!
//! Every `SendData` timed out for hours until a redeploy. These pin the parts
//! that must not need one: a call that dies on a dead connection is retried on
//! a fresh one, a sustained failure flips `/health/livekit` to 503, and the
//! operator can see the counts and probe each leg of the path.


use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::broker::{room_send_data, BrokerConfig};
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

use common::fake_livekit::livekit_with_a_dead_pooled_connection;

const TOKEN: &str = "operator-token";

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    pollis_schema::apply::single_db(&db.conn().await.expect("checkout"))
        .await
        .expect("schema");
    db
}

fn livekit_broker(url: &str) -> BrokerConfig {
    BrokerConfig {
        livekit_api_key: Some("APIkey".into()),
        livekit_api_secret: Some("secret-secret-secret-secret".into()),
        livekit_url: Some(url.to_string()),
        ..BrokerConfig::default()
    }
}

/// A port nothing listens on: every connect is refused at once.
async fn refused_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("ws://{addr}")
}

async fn get(state: &AppState, uri: &str, token: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = build_router_with_state(state.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

/// Nine hours of failures went unnoticed. A sustained failure must be visible
/// without reading logs: `/health/livekit` 503s and the operator counts say
/// which phase is failing.
#[tokio::test(flavor = "multi_thread")]
async fn a_sustained_failure_turns_the_health_signal_red() {
    let db = fresh_db().await;
    let state = AppState::new(db.arc(), false)
        .with_broker_config(livekit_broker(&refused_url().await))
        .with_metrics_token(Some(TOKEN.into()));

    let payload = serde_json::json!({ "type": "inbox_refresh" });
    for _ in 0..pollis_delivery::livekit::DEGRADED_AFTER {
        let err = room_send_data(&state, "inbox-alice", &payload).await.unwrap_err();
        assert!(err.contains("connect"), "the error names the phase: {err}");
        assert!(err.contains("2 attempts"), "and that it was retried: {err}");
    }

    assert_eq!(
        get(&state, "/health/livekit", None).await,
        (StatusCode::SERVICE_UNAVAILABLE, serde_json::json!({ "livekit": "degraded" }))
    );
    let (status, body) = get(&state, "/v1/ops/livekit", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let counts = &body["counts"];
    assert_eq!(counts["verdict"], "degraded");
    assert_eq!(counts["verdict_source"], "calls", "judged by the real calls, not a check");
    assert_eq!(counts["failures"]["connect"], pollis_delivery::livekit::DEGRADED_AFTER);
    assert_eq!(counts["consecutive_failures"], pollis_delivery::livekit::DEGRADED_AFTER);
    assert_eq!(counts["last_failure"], "SendData connect");
    assert!(counts["degraded_since"].is_u64());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_operator_view_is_closed_without_its_token() {
    let db = fresh_db().await;
    let closed = AppState::new(db.arc(), false);
    assert_eq!(get(&closed, "/v1/ops/livekit", Some(TOKEN)).await.0, StatusCode::NOT_FOUND);
    let open = closed.with_metrics_token(Some(TOKEN.into()));
    assert_eq!(get(&open, "/v1/ops/livekit", None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(get(&open, "/v1/ops/livekit", Some("wrong")).await.0, StatusCode::UNAUTHORIZED);
}

/// `?probe=1` times each leg from inside the DS, and leaks no address.
#[tokio::test(flavor = "multi_thread")]
async fn the_probe_times_each_leg_and_names_no_address() {
    let (url, _, _) = livekit_with_a_dead_pooled_connection().await;
    let db = fresh_db().await;
    let state = AppState::new(db.arc(), false)
        .with_broker_config(livekit_broker(&url))
        .with_metrics_token(Some(TOKEN.into()));

    let (status, body) = get(&state, "/v1/ops/livekit?probe=1", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let probe = &body["probe"];
    for leg in ["dns", "tcp_connect", "tcp_connect_port_80", "https_fresh", "list_rooms", "send_data_empty_room"] {
        assert!(probe[leg]["ms"].is_u64(), "{leg} is timed: {probe}");
    }
    assert_eq!(probe["dns"]["v4"], 1);
    assert_eq!(probe["tcp_connect"]["ok"], true);
    let text = body.to_string();
    assert!(!text.contains("127.0.0.1"), "the probe must not print an address: {text}");
    // The probe is not traffic: it must not mask or fake an outage.
    assert_eq!(body["counts"]["ok"], 0);
}

async fn error_rows(state: &AppState) -> Vec<serde_json::Value> {
    // The row is written off the response path; give the spawned insert a moment.
    for _ in 0..50 {
        let (status, body) = get(state, "/v1/ops/errors", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK);
        let rows = body["errors"].as_array().cloned().unwrap_or_default();
        if !rows.is_empty() {
            return rows;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Vec::new()
}

/// #1751, the second time: on dev the container could not even open a TCP
/// connection to LiveKit, yet `/health/livekit` said `idle` and the counts were
/// all zero, because nothing had called LiveKit since the container woke. With
/// no real call in the window, reading the signal must find the outage itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_quiet_outage_is_found_when_the_signal_is_read() {
    let db = fresh_db().await;
    let state = AppState::new(db.arc(), false)
        .with_broker_config(livekit_broker(&refused_url().await))
        .with_metrics_token(Some(TOKEN.into()));

    assert_eq!(
        get(&state, "/health/livekit", None).await,
        (StatusCode::SERVICE_UNAVAILABLE, serde_json::json!({ "livekit": "degraded" }))
    );
    let (_, body) = get(&state, "/v1/ops/livekit", Some(TOKEN)).await;
    let counts = &body["counts"];
    assert_eq!(counts["verdict"], "degraded");
    assert_eq!(counts["verdict_source"], "check");
    assert_eq!(counts["check"]["ok"], false);
    assert_eq!(counts["check"]["phase"], "connect");
    // A check is not traffic: the call counts stay untouched.
    assert_eq!(counts["consecutive_failures"], 0);
    assert!(counts["failures"].as_object().unwrap().is_empty());

    // And the 503 is a row in the operator's failed-request log.
    let rows = error_rows(&state).await;
    assert!(
        rows.iter().any(|r| r["route"] == "/health/livekit" && r["status"] == 503),
        "{rows:?}"
    );
}

/// A healthy LiveKit reads `ok` with no traffic at all, and however often the
/// signal is read, LiveKit sees at most one check per TTL.
#[tokio::test(flavor = "multi_thread")]
async fn a_quiet_healthy_livekit_reads_ok_and_the_check_is_cached() {
    let (url, accepted, _) = livekit_with_a_dead_pooled_connection().await;
    let db = fresh_db().await;
    let state = AppState::new(db.arc(), false)
        .with_broker_config(livekit_broker(&url))
        .with_metrics_token(Some(TOKEN.into()));

    for _ in 0..5 {
        assert_eq!(
            get(&state, "/health/livekit", None).await,
            (StatusCode::OK, serde_json::json!({ "livekit": "ok" }))
        );
    }
    let (_, body) = get(&state, "/v1/ops/livekit", Some(TOKEN)).await;
    assert_eq!(body["counts"]["verdict_source"], "check");
    assert_eq!(body["counts"]["check"]["ok"], true);
    assert_eq!(
        accepted.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "six reads, one connection: the check is cached"
    );
}

/// The nudges the DS sends on its own (enrollment approval prompts, inbox
/// refreshes, history-transfer events) run after their request already
/// answered 2xx. During #1751 their failures appeared nowhere an operator
/// looks; they must land in the failed-request log as their own rows.
#[tokio::test(flavor = "multi_thread")]
async fn a_background_nudge_failure_lands_in_the_failed_request_log() {
    let db = fresh_db().await;
    let state = AppState::new(db.arc(), false)
        .with_broker_config(livekit_broker(&refused_url().await))
        .with_metrics_token(Some(TOKEN.into()));

    let bg = state.clone();
    let err = tokio::spawn(async move {
        room_send_data(&bg, "inbox-alice", &serde_json::json!({ "type": "enrollment_requested" })).await
    })
    .await
    .unwrap()
    .unwrap_err();
    assert!(err.contains("connect"), "{err}");

    let rows = error_rows(&state).await;
    let row = rows
        .iter()
        .find(|r| r["route"] == "/twirp/livekit.RoomService/SendData")
        .unwrap_or_else(|| panic!("no background row: {rows:?}"));
    // A connect failure is a timeout-class failure: 504, as the request path answers it.
    assert_eq!(row["status"], 504);
    assert!(row["error"].as_str().unwrap().contains("2 attempts"), "{row}");
    let detail = row["detail"].as_str().unwrap();
    assert!(detail.contains("phase=connect"), "{detail}");
    let text = row.to_string();
    assert!(!text.contains("inbox-alice"), "no room name in the row: {text}");
    assert!(!text.contains("127.0.0.1"), "no address in the row: {text}");
}
