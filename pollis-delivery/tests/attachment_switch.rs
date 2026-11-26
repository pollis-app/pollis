//! #2109's shared-upload switch (`POLLIS_DS_SHARED_ATTACHMENTS`), through the
//! real router: off by default, `/v1/attachments/format` reports it, and a claim
//! is refused while it is off, so no shared object is stored before the builds
//! in the stores can open one.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, String) {
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn post(path: &str, body: &str, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = bearer {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn claim() -> Request<Body> {
    post(
        "/v1/attachments/claim",
        &format!(r#"{{"content_hash":"v2-{}","user_id":"alice"}}"#, "ab".repeat(32)),
        None,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_uploads_are_off_unless_switched_on() {
    let db = common::TempDb::open("switch.db").await;
    let state = AppState::new(db.arc(), false);

    let (status, body) = call(&state, post("/v1/attachments/format", "{}", None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, r#"{"shared":false}"#, "off by default");
    let (status, _) = call(&state, claim()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no claim while off");

    let state = state.with_shared_attachments(true);
    let (_, body) = call(&state, post("/v1/attachments/format", "{}", None)).await;
    assert_eq!(body, r#"{"shared":true}"#);
    let (status, _) = call(&state, claim()).await;
    assert_ne!(status, StatusCode::FORBIDDEN, "past the switch once on");
}
