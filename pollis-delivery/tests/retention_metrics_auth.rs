//! `GET /v1/retention/metrics` access control (#720): default-closed, bearer-token gated.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::messages::RetentionSnapshot;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

/// The handler reads only the snapshot cell, so no schema is needed.
async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    db
}

fn metrics_request(token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri("/v1/retention/metrics");
    if let Some(t) = token {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, body)
}

fn seed_snapshot(state: &AppState) {
    *state.retention_metrics.lock().unwrap() = Some(RetentionSnapshot {
        total_envelopes: 42,
        conversations_with_envelopes: 3,
        oldest_sent_at: Some("2026-01-01T00:00:00+00:00".into()),
        largest_conversation_envelopes: 30,
        largest_conversation_oldest_sent_at: Some("2026-01-02T00:00:00+00:00".into()),
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn unset_token_does_not_serve_the_endpoint() {
    let db = fresh_db().await;
    let state = AppState::new(Arc::clone(&db), false).with_metrics_token(None);
    // The gate, not the absence of data, must withhold it.
    seed_snapshot(&state);

    let (status, body) = call(&state, metrics_request(Some("anything"))).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "unset POLLIS_DS_METRICS_TOKEN must mean 'not served', never 'served openly'"
    );
    assert!(
        !String::from_utf8_lossy(&body).contains("total_envelopes"),
        "a 404 must not carry the growth snapshot"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_but_missing_token_is_rejected() {
    let db = fresh_db().await;
    let state = AppState::new(Arc::clone(&db), false).with_metrics_token(Some("s3cret".into()));
    seed_snapshot(&state);

    let (status, body) = call(&state, metrics_request(None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no bearer token → rejected");
    assert!(
        !String::from_utf8_lossy(&body).contains("total_envelopes"),
        "a rejected request must not carry the growth snapshot"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_but_wrong_token_is_rejected() {
    let db = fresh_db().await;
    let state = AppState::new(Arc::clone(&db), false).with_metrics_token(Some("s3cret".into()));
    seed_snapshot(&state);

    let (status, body) = call(&state, metrics_request(Some("not-it"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "wrong bearer token → rejected");
    assert!(
        !String::from_utf8_lossy(&body).contains("total_envelopes"),
        "a rejected request must not carry the growth snapshot"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn correct_token_returns_the_snapshot() {
    let db = fresh_db().await;
    let state = AppState::new(Arc::clone(&db), false).with_metrics_token(Some("s3cret".into()));
    seed_snapshot(&state);

    let (status, body) = call(&state, metrics_request(Some("s3cret"))).await;
    assert_eq!(status, StatusCode::OK, "the configured token unlocks the endpoint");
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["computed"], true);
    assert_eq!(
        json["metrics"]["total_envelopes"], 42,
        "the gated response carries the real growth snapshot"
    );
    assert!(
        json["metrics"].get("conversation_id").is_none()
            && json["metrics"].get("user_id").is_none(),
        "the snapshot stays identity-free even behind the gate"
    );
}

/// Before the first sweep, an authorized caller gets 200 with `computed: false`.
#[tokio::test(flavor = "multi_thread")]
async fn correct_token_before_first_sweep_is_computed_false() {
    let db = fresh_db().await;
    let state = AppState::new(Arc::clone(&db), false).with_metrics_token(Some("s3cret".into()));

    let (status, body) = call(&state, metrics_request(Some("s3cret"))).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["computed"], false, "authorized, but no snapshot yet");
    assert!(json["metrics"].is_null());
}

// GET /v1/config (#760): same operator gate, so deployed config is verifiable from outside.

fn config_request(token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri("/v1/config");
    if let Some(t) = token {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn config_is_not_served_without_a_token() {
    let db = fresh_db().await;
    let state = AppState::new(db.arc(), true);
    let (status, body) = call(&state, config_request(None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty(), "an unconfigured route must not describe itself");
}

/// Configured but unauthenticated is 401, not 404: that distinction is what hid #758.
#[tokio::test]
async fn config_rejects_a_missing_or_wrong_token_with_401() {
    let db = fresh_db().await;
    let state = AppState::new(db.arc(), true).with_metrics_token(Some("right".into()));
    assert_eq!(call(&state, config_request(None)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        call(&state, config_request(Some("wrong"))).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn config_reports_effective_values_and_no_secrets() {
    let db = fresh_db().await;
    let state = AppState::new(db.arc(), true).with_metrics_token(Some("right".into()));
    let (status, body) = call(&state, config_request(Some("right"))).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Parenthesised: `&&` binds tighter than `||`; a positive offset would mark every device stale.
    let stale = v["watermark_stale_modifier"].as_str().expect("modifier reported");
    assert!(
        stale.starts_with('-') && (stale.contains("months") || stale.contains("years")),
        "expected a negative SQLite datetime modifier, got {stale:?}"
    );
    assert_eq!(
        v["gc_sweep_secs"],
        serde_json::json!(pollis_delivery::messages::DEFAULT_GC_SWEEP_SECS),
        "unset POLLIS_DS_GC_SWEEP_SECS must report the cadence the sweep actually runs"
    );
    assert_eq!(v["require_auth"], serde_json::json!(true));
    assert_eq!(v["metrics_token_set"], serde_json::json!(true));

    let raw = String::from_utf8_lossy(&body);
    assert!(!raw.contains("right"), "the endpoint leaked the operator token");
}
