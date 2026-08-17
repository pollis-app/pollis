//! #875 / #918 — every declared endpoint is actually routed.
//!
//! `pollis-api::ENDPOINTS` is the one place the write API is written down: the
//! client posts `Body::PATH`, and this router is built from the same constant.
//! That makes a *typo* impossible, but not an *omission* — a request type can be
//! declared, used by `pollis-core`, and simply never routed. The client would
//! then compile perfectly and 404 at runtime, which is precisely how
//! `/v1/invite-links/*` reached the flows harness broken (#918).
//!
//! So: walk the table, hit every path, and require the router to know it. A
//! 404 here means the endpoint exists in the type system and nowhere else.
//!
//! The mirror of this test lives in the `flows` harness
//! (`src-tauri/tests/flows/harness.rs`), which asserts the same table against the
//! in-process DS the integration suite runs against.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

/// The router plus the fixture that owns its temp dir. The guard is returned
/// rather than dropped here: it deletes the directory the DB lives in, so the
/// caller has to hold it for as long as it drives the router (#942).
async fn router() -> (axum::Router, common::TempDb) {
    let db = common::TempDb::open("db.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    // Auth OFF: this test is about ROUTING, not authorization. With auth on
    // every probe would stop at the 401 gate, which is indistinguishable from a
    // route that exists — and would pass even for a path that does not.
    (build_router_with_state(AppState::new(db.arc(), false)), db)
}

/// The treatment: every path in the shared endpoint table resolves to a handler.
///
/// The probe body is `{}`, which nearly every handler rejects as a bad request —
/// that is fine and deliberate. A 400 proves a handler ran; a 404 proves none
/// did. The assertion is only ever about the difference between those two.
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

/// The control: the probe above can actually observe a missing route.
///
/// Without this, `every_declared_endpoint_is_routed` would pass just as happily
/// against a router that answered 200 to everything, and the test would be
/// proving nothing. A path deliberately absent from the table must come back
/// 404 through the same probe.
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

/// The table is not allowed to shrink silently.
///
/// `ENDPOINTS` drives the client, both routers and both coverage tests, so
/// deleting a row disables all of them at once with no failure anywhere. This
/// pins the count: adding or removing an endpoint is then a deliberate edit here
/// as well, not a side effect.
#[test]
fn the_endpoint_table_covers_the_whole_write_surface() {
    assert_eq!(
        pollis_api::ENDPOINTS.len(),
        72,
        "the DS write surface changed — update this count deliberately, and make \
         sure the new endpoint is routed in BOTH pollis-delivery and the flows harness"
    );
}
