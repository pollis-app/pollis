//! #875 / #918 — every declared endpoint is actually routed.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

/// The caller holds the fixture: dropping it deletes the DB's directory (#942).
async fn router() -> (axum::Router, common::TempDb) {
    let db = common::TempDb::open("db.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    // Auth off: with auth on every probe stops at the 401 gate, hiding missing routes.
    (build_router_with_state(AppState::new(db.arc(), false)), db)
}

/// A 400 to the `{}` probe proves a handler ran; a 404 proves none did.
#[tokio::test]
async fn every_declared_endpoint_is_routed() {
    let (app, _db) = router().await;
    let mut missing = Vec::new();

    for endpoint in pollis_api::ENDPOINTS {
        let req = Request::builder()
            .method("POST")
            .uri(endpoint.path)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .expect("request");
        let resp = app.clone().oneshot(req).await.expect("oneshot");
        if resp.status() == StatusCode::NOT_FOUND {
            missing.push(format!("{} ({})", endpoint.path, endpoint.type_name));
        }
    }

    assert!(
        missing.is_empty(),
        "declared in pollis-api but NOT routed by pollis-delivery — the client \
         would compile and 404:\n  {}",
        missing.join("\n  ")
    );
}

/// The control: the probe can actually observe a missing route.
#[tokio::test]
async fn an_unrouted_path_is_observably_404() {
    let (app, _db) = router().await;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/definitely-not-an-endpoint")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .expect("request");
    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "the probe must be able to see a missing route, or the coverage test is vacuous"
    );
}

/// The table must not shrink silently: removing a row disables every coverage test.
#[test]
fn the_endpoint_table_covers_the_whole_write_surface() {
    assert_eq!(
        pollis_api::ENDPOINTS.len(),
        120,
        "the DS write surface changed — update this count deliberately, and make \
         sure the new endpoint is routed in BOTH pollis-delivery and the flows harness"
    );
}
