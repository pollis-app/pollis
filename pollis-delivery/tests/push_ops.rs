//! #1423 — push outcomes are visible to the operator, and the tickets kept for
//! receipts go with the account.
//!
//! `GET /v1/ops/push` serves counts only, behind the operator token, with the
//! same fail-closed shape as `/v1/ops/errors`. `push_ticket` names only a token,
//! so account deletion has to reach it through the user's tokens.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::teardown::purge_user_rows;
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

async fn get_push(state: &AppState, token: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method("GET").uri("/v1/ops/push");
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
async fn the_route_is_absent_without_an_operator_token() {
    let db = schema_db().await;
    let state = AppState::new(db.arc(), false);
    assert_eq!(get_push(&state, Some("anything")).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_route_needs_the_operator_token() {
    let db = schema_db().await;
    let state = AppState::new(db.arc(), false).with_metrics_token(Some("op-secret".into()));
    assert_eq!(get_push(&state, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(get_push(&state, Some("wrong")).await.0, StatusCode::UNAUTHORIZED);

    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO push_ticket (ticket_id, token, created_at) VALUES ('t1', 'ExponentPushToken[a]', ?1)",
        libsql::params![pollis_delivery::util::now_unix() as i64],
    )
    .await
    .unwrap();

    let (status, body) = get_push(&state, Some("op-secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending_receipts"], 1);
    assert!(body["counts"].is_object(), "{body}");
    // Counts only: the pending ticket's token never appears.
    assert!(!body.to_string().contains("ExponentPushToken"), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn account_deletion_removes_the_users_pending_tickets() {
    let db = schema_db().await;
    let conn = db.conn().await.unwrap();
    for (token, user) in [("ExponentPushToken[a]", "alice"), ("ExponentPushToken[b]", "bob")] {
        conn.execute(
            "INSERT INTO push_token (token, user_id, platform, updated_at) VALUES (?1, ?2, 'ios', 'x')",
            libsql::params![token, user],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO push_ticket (ticket_id, token, created_at) VALUES (?1, ?1, ?2)",
            libsql::params![token, pollis_delivery::util::now_unix() as i64],
        )
        .await
        .unwrap();
    }

    purge_user_rows(&conn, "alice").await.unwrap();

    let mut rows = conn
        .query("SELECT token FROM push_ticket ORDER BY token", ())
        .await
        .unwrap();
    let mut left = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        left.push(r.get::<String>(0).unwrap());
    }
    assert_eq!(left, vec!["ExponentPushToken[b]"]);
}
