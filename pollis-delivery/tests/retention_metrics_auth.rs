//! `GET /v1/retention/metrics` access control (#720, review round 2), driven
//! through the real axum router with `tower::oneshot`.
//!
//! The growth snapshot is identity-free but it is still a pollable aggregate
//! activity meter (envelope totals, conversation counts, growth over time) that a
//! privacy-first messenger must not publish openly. So the endpoint is
//! **default-closed**: unset `POLLIS_DS_METRICS_TOKEN` (modelled by
//! `metrics_token = None`) means the route is not served at all, and when a token
//! IS configured a caller must present it as `Authorization: Bearer <token>`.
//!
//! Every test below fails against the pre-round-2 code, where the handler answered
//! `200 {computed…}` regardless of any token: the "unset → not served" and
//! "wrong/absent token → rejected" assertions all flip.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::db::Db;
use pollis_delivery::messages::RetentionSnapshot;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

/// A bare local DB — the metrics handler reads only the in-memory snapshot cell,
/// never a table, so no schema is needed.
async fn fresh_db() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("delivery.db");
    std::mem::forget(dir);
    Arc::new(Db::connect_local(path.to_str().unwrap()).await.expect("local db"))
}

/// A `GET /v1/retention/metrics` request with an optional bearer token.
fn metrics_request(token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri("/v1/retention/metrics");
    if let Some(t) = token {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

/// Drive one request through a fresh router built on `state` and return
/// `(status, body_bytes)`.
async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, body)
}

/// Seed the shared snapshot cell so a 200 can prove it returns the real numbers.
fn seed_snapshot(state: &AppState) {
    *state.retention_metrics.lock().unwrap() = Some(RetentionSnapshot {
        total_envelopes: 42,
        conversations_with_envelopes: 3,
        oldest_sent_at: Some("2026-01-01T00:00:00+00:00".into()),
        largest_conversation_envelopes: 30,
        largest_conversation_oldest_sent_at: Some("2026-01-02T00:00:00+00:00".into()),
    });
}

/// **Fail-closed on misconfiguration.** No token configured → the route is not
/// served: 404, and NOT the snapshot — even though a snapshot exists to leak.
#[tokio::test(flavor = "multi_thread")]
async fn unset_token_does_not_serve_the_endpoint() {
    let db = fresh_db().await;
    let state = AppState::new(Arc::clone(&db), false).with_metrics_token(None);
    // A snapshot is present; the gate — not the absence of data — must withhold it.
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

/// Configured but NO `Authorization` header → 401, snapshot withheld.
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

/// Configured and the WRONG token → 401, snapshot withheld.
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

/// The correct token → 200 with the real snapshot. This is the owner's `curl`
/// access path, preserved.
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

/// Before the first sweep the cell is empty; a correctly-authorized caller still
/// gets 200, with `computed: false`, so the owner can tell "not yet computed" from
/// "not authorized".
#[tokio::test(flavor = "multi_thread")]
async fn correct_token_before_first_sweep_is_computed_false() {
    let db = fresh_db().await;
    let state = AppState::new(Arc::clone(&db), false).with_metrics_token(Some("s3cret".into()));
    // No snapshot seeded — the sweep has not run.

    let (status, body) = call(&state, metrics_request(Some("s3cret"))).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["computed"], false, "authorized, but no snapshot yet");
    assert!(json["metrics"].is_null());
}

// ── GET /v1/config (#760) ────────────────────────────────────────────────────
//
// Same operator gate as the metrics route, and the same fail-closed shape. It
// exists so deployed config is verifiable from OUTSIDE the container: #720's two
// config values were inert in production because the Worker never forwarded them,
// and nothing could observe that — the DS logs its config at startup, but
// `wrangler tail` shows Worker logs, not container stdout.

fn config_request(token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri("/v1/config");
    if let Some(t) = token {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

/// Unset token → the route is not served at all. Identical posture to the metrics
/// endpoint: an unconfigured deploy must not even advertise that it exists.
#[tokio::test]
async fn config_is_not_served_without_a_token() {
    let state = AppState::new(fresh_db().await, true);
    let (status, body) = call(&state, config_request(None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty(), "an unconfigured route must not describe itself");
}

/// Configured but unauthenticated → 401, NOT 404. This distinction is the entire
/// diagnostic value of the endpoint: 404 means "config never arrived", 401 means
/// "config arrived and auth rejected". Conflating them is what hid #758 for a
/// full deploy cycle.
#[tokio::test]
async fn config_rejects_a_missing_or_wrong_token_with_401() {
    let state = AppState::new(fresh_db().await, true).with_metrics_token(Some("right".into()));
    assert_eq!(call(&state, config_request(None)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        call(&state, config_request(Some("wrong"))).await.0,
        StatusCode::UNAUTHORIZED
    );
}

/// Correct token → the EFFECTIVE config, and never a secret's value.
#[tokio::test]
async fn config_reports_effective_values_and_no_secrets() {
    let state = AppState::new(fresh_db().await, true).with_metrics_token(Some("right".into()));
    let (status, body) = call(&state, config_request(Some("right"))).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // The staleness window is reported as the modifier the GC actually binds, so
    // a var that never reached the process shows its fallback rather than the
    // value someone believes they configured.
    let stale = v["watermark_stale_modifier"].as_str().expect("modifier reported");
    assert!(stale.starts_with('-') && stale.contains("months") || stale.contains("years"),
        "expected a SQLite datetime modifier, got {stale:?}");
    assert_eq!(v["require_auth"], serde_json::json!(true));
    assert_eq!(v["metrics_token_set"], serde_json::json!(true));

    // The token's VALUE must never appear anywhere in the response.
    let raw = String::from_utf8_lossy(&body);
    assert!(!raw.contains("right"), "the endpoint leaked the operator token");
}
