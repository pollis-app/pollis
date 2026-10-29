//! #1305 — the DS's failed-request log (`error_log.rs`, migration 000035).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use http_body_util::BodyExt as _;
use pollis_delivery::error_log::{self, FailedRequest};
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

async fn schema_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db
}

fn row(at: u64) -> FailedRequest {
    FailedRequest {
        at,
        request_id: None,
        method: "POST".into(),
        route: "/v1/x".into(),
        status: 400,
        error: None,
        detail: None,
    }
}

async fn count(db: &common::TempDb) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn.query("SELECT COUNT(*) FROM ds_error_log", ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// The insert is spawned off the response path; wait for it to land.
async fn wait_for_rows(db: &common::TempDb, n: i64) {
    for _ in 0..200 {
        if count(db).await >= n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("expected {n} ds_error_log rows, found {}", count(db).await);
}

async fn get_errors(state: &AppState, token: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method("GET").uri("/v1/ops/errors");
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

#[tokio::test(flavor = "multi_thread")]
async fn the_table_keeps_at_most_500_rows() {
    let db = schema_db().await;
    for i in 0..520 {
        error_log::insert(&db, &row(1_000_000 + i)).await.unwrap();
    }
    assert_eq!(count(&db).await, 500);
}

#[tokio::test(flavor = "multi_thread")]
async fn rows_older_than_72_hours_are_dropped_on_insert() {
    let db = schema_db().await;
    error_log::insert(&db, &row(1_000_000)).await.unwrap();
    error_log::insert(&db, &row(1_000_000 + 72 * 3600 + 1)).await.unwrap();
    assert_eq!(count(&db).await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_table_refuses_identifying_or_unbounded_values() {
    let db = schema_db().await;
    let bad = [
        FailedRequest { request_id: Some("alice@example.com".into()), ..row(1) },
        FailedRequest { route: "v1/no-leading-slash".into(), ..row(1) },
        FailedRequest { status: 200, ..row(1) },
        FailedRequest { error: Some("x".repeat(201)), ..row(1) },
        FailedRequest { detail: Some("x".repeat(1001)), ..row(1) },
        FailedRequest { method: "CONNECT".into(), ..row(1) },
    ];
    for r in &bad {
        assert!(error_log::insert(&db, r).await.is_err(), "accepted {r:?}");
    }
    assert_eq!(count(&db).await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_request_is_recorded_and_served_behind_the_token() {
    let db = schema_db().await;
    let state = AppState::new(db.arc(), false).with_metrics_token(Some("op-token".into()));

    let resp = build_router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/verify-otp")
                .header(pollis_api::redact::REQUEST_ID_HEADER, "00ff00ff00ff00ff")
                .header("content-type", "application/json")
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // The middleware read the body for its `error`; the client must still get it.
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
        "invalid body"
    );
    wait_for_rows(&db, 1).await;

    let (status, _) = get_errors(&AppState::new(db.arc(), false), Some("op-token")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no token configured → not served");
    let (status, _) = get_errors(&state, Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, json) = get_errors(&state, Some("op-token")).await;
    assert_eq!(status, StatusCode::OK);
    // The wrong-token 401 above is itself a failed request.
    let errors = json["errors"].as_array().unwrap();
    let row = errors
        .iter()
        .find(|r| r["route"] == "/v1/auth/verify-otp")
        .expect("the verify-otp failure is listed");
    assert_eq!(row["request_id"], "00ff00ff00ff00ff");
    assert_eq!(row["status"], 400);
    assert_eq!(row["error"], "invalid body");
}

#[tokio::test]
async fn the_cause_behind_a_5xx_is_captured_and_scrubbed() {
    use tracing_subscriber::prelude::*;
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::registry().with(error_log::CaptureLayer),
    );

    let db = schema_db().await;
    let state = AppState::new(db.arc(), false);
    async fn failing() -> (StatusCode, &'static str) {
        tracing::error!("establish-identity failed for alice@example.com: disk full");
        (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
    let app = Router::new()
        .route("/v1/test/fail", post(failing))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), error_log::record))
        .with_state(state);
    let resp = app
        .oneshot(Request::builder().method("POST").uri("/v1/test/fail").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    wait_for_rows(&db, 1).await;

    let conn = db.conn().await.unwrap();
    let mut rows = conn.query("SELECT detail FROM ds_error_log", ()).await.unwrap();
    let detail: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(detail, "establish-identity failed for ***@example.com: disk full");
}
