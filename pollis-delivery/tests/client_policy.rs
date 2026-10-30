//! The minimum-client-version policy on the DS (#1611): the 426 for a known-too-old build, what
//! passes (no headers, unknown platforms, GETs), and the mirror of the published file.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_api::client_policy::{
    ClientPlatform, ClientPolicy, UpgradeRequiredBody, CLIENT_PLATFORM_HEADER, CLIENT_VERSION_HEADER,
};
use pollis_delivery::client_policy::PolicyMirror;
use pollis_delivery::otp::OtpConfig;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

const FLOOR: &str = r#"{"v":1,"minimum":{"macos":"1.17.0","android-play":"1.0.21"},"message":"Version 1.16 loses drafts."}"#;

fn state(db: Arc<pollis_delivery::db::Db>, mirror: PolicyMirror) -> AppState {
    AppState::new(db, false)
        .with_otp_config(OtpConfig {
            resend_api_key: None,
            dev_otp: Some("123456".to_string()),
            resend_throttle_secs: 0,
            ..OtpConfig::default()
        })
        .with_client_policy(mirror)
}

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, body)
}

/// A pre-credential POST, declaring `client` (platform, version) when given.
fn otp(client: Option<(&str, &str)>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/v1/auth/request-otp")
        .header("content-type", "application/json")
        .header("CF-Connecting-IP", "203.0.113.7");
    if let Some((platform, version)) = client {
        b = b.header(CLIENT_PLATFORM_HEADER, platform).header(CLIENT_VERSION_HEADER, version);
    }
    b.body(Body::from(r#"{"email":"policy@example.com"}"#)).unwrap()
}

fn get(path: &str, client: Option<(&str, &str)>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri(path);
    if let Some((platform, version)) = client {
        b = b.header(CLIENT_PLATFORM_HEADER, platform).header(CLIENT_VERSION_HEADER, version);
    }
    b.body(Body::empty()).unwrap()
}

fn floor() -> PolicyMirror {
    PolicyMirror::fixed(ClientPolicy::parse(FLOOR).unwrap())
}

#[tokio::test]
async fn a_build_below_its_minimum_gets_a_typed_426() {
    let db = common::TempDb::open("policy-426.db").await;
    let st = state(db.arc(), floor());
    let (status, body) = call(&st, otp(Some(("macos", "1.16.2")))).await;
    assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
    let body: UpgradeRequiredBody = serde_json::from_slice(&body).unwrap();
    assert_eq!(body.error, "upgrade_required");
    assert_eq!(body.platform, ClientPlatform::Macos);
    assert_eq!(body.version, "1.16.2");
    assert_eq!(body.minimum, "1.17.0");
    assert_eq!(body.message.as_deref(), Some("Version 1.16 loses drafts."));
    // The whole policy rides along, so the client can cache it and keep blocking offline.
    assert_eq!(body.policy, ClientPolicy::parse(FLOOR).unwrap());

    let (status, _) = call(&st, otp(Some(("android-play", "1.0.20")))).await;
    assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
}

#[tokio::test]
async fn everything_else_passes() {
    let db = common::TempDb::open("policy-pass.db").await;
    let st = state(db.arc(), floor());
    for client in [
        // Up to date, and exactly at the minimum.
        Some(("macos", "1.18.0")),
        Some(("macos", "1.17.0")),
        // A platform with no minimum.
        Some(("windows", "0.0.1")),
        Some(("android-sideload", "1.0.0")),
        // Backward compatible: a build older than #1611 sends no headers.
        None,
        // A platform this DS does not know, and an unparseable version: fail open.
        Some(("visionos", "0.1.0")),
        Some(("macos", "dev")),
    ] {
        let (status, body) = call(&st, otp(client)).await;
        assert_eq!(status, StatusCode::OK, "{client:?}: {}", String::from_utf8_lossy(&body));
    }
    // Only one header is not a declaration.
    let req = Request::builder()
        .method("POST")
        .uri("/v1/auth/request-otp")
        .header("content-type", "application/json")
        .header(CLIENT_VERSION_HEADER, "0.0.1")
        .body(Body::from(r#"{"email":"half@example.com"}"#))
        .unwrap();
    assert_eq!(call(&st, req).await.0, StatusCode::OK);
}

/// Ops and health GETs are exempt, even from a blocked build; the mirror shows what is enforced.
#[tokio::test]
async fn gets_are_exempt_and_the_mirror_is_served() {
    let db = common::TempDb::open("policy-get.db").await;
    let st = state(db.arc(), floor());
    let old = Some(("macos", "1.0.0"));
    for path in ["/health", "/version"] {
        assert_eq!(call(&st, get(path, old)).await.0, StatusCode::OK, "{path}");
    }
    // The operator routes answer as they always do (404: no token configured), never 426.
    for path in ["/v1/ops/errors", "/v1/config", "/v1/retention/metrics"] {
        assert_ne!(call(&st, get(path, old)).await.0, StatusCode::UPGRADE_REQUIRED, "{path}");
    }
    let (status, body) = call(&st, get("/v1/client-policy", old)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_slice::<ClientPolicy>(&body).unwrap(), ClientPolicy::parse(FLOOR).unwrap());
}

#[tokio::test]
async fn unconfigured_it_enforces_nothing() {
    let db = common::TempDb::open("policy-off.db").await;
    let st = state(db.arc(), PolicyMirror::new(None));
    assert_eq!(call(&st, otp(Some(("macos", "0.0.1")))).await.0, StatusCode::OK);
    assert_eq!(call(&st, get("/v1/client-policy", None)).await.0, StatusCode::NOT_FOUND);
}

/// Serves whatever `body` holds at the moment of each request; `status` likewise.
async fn cdn(initial: (u16, &str)) -> (String, Arc<std::sync::Mutex<(u16, String)>>) {
    use axum::routing::get;
    let current = Arc::new(std::sync::Mutex::new((initial.0, initial.1.to_string())));
    let c = current.clone();
    let app = axum::Router::new().route(
        "/client-policy.json",
        get(move || {
            let c = c.clone();
            async move {
                let (status, body) = c.lock().unwrap().clone();
                (StatusCode::from_u16(status).unwrap(), body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://127.0.0.1:{port}/client-policy.json"), current)
}

/// The mirror follows the published file and keeps the last good copy through failures.
#[tokio::test]
async fn the_mirror_follows_the_published_file() {
    let (url, served) = cdn((200, FLOOR)).await;
    let mirror = PolicyMirror::new(Some(url));
    // Nothing fetched yet: nothing enforced.
    assert_eq!(mirror.current(), None);
    mirror.refresh().await;
    assert_eq!(mirror.current(), Some(ClientPolicy::parse(FLOOR).unwrap()));

    // Malformed, or a server error: keep the last good copy.
    *served.lock().unwrap() = (200, r#"{"v":1,"minimum":{"macos":"later"}}"#.into());
    mirror.refresh().await;
    assert_eq!(mirror.current(), Some(ClientPolicy::parse(FLOOR).unwrap()));
    *served.lock().unwrap() = (500, String::new());
    mirror.refresh().await;
    assert_eq!(mirror.current(), Some(ClientPolicy::parse(FLOOR).unwrap()));

    // A lowered minimum is picked up.
    *served.lock().unwrap() = (200, r#"{"v":1,"minimum":{"macos":"1.0.0"}}"#.into());
    mirror.refresh().await;
    assert!(!mirror.current().unwrap().evaluate(ClientPlatform::Macos, "1.16.0").is_blocked());

    // Removed from the CDN: no minimums.
    *served.lock().unwrap() = (404, String::new());
    mirror.refresh().await;
    assert_eq!(mirror.current(), Some(ClientPolicy::empty()));
}

/// End to end: the DS 426s from a mirrored file, and stops once the minimum is lowered.
#[tokio::test]
async fn a_publish_reaches_enforcement() {
    let (url, served) = cdn((200, FLOOR)).await;
    let mirror = PolicyMirror::new(Some(url));
    mirror.refresh().await;
    let db = common::TempDb::open("policy-e2e.db").await;
    let st = state(db.arc(), mirror.clone());
    assert_eq!(call(&st, otp(Some(("macos", "1.16.0")))).await.0, StatusCode::UPGRADE_REQUIRED);
    *served.lock().unwrap() = (200, r#"{"v":1,"minimum":{}}"#.into());
    mirror.refresh().await;
    assert_eq!(call(&st, otp(Some(("macos", "1.16.0")))).await.0, StatusCode::OK);
}

/// `GET /v1/config` reports the mirrored URL, so a deploy can prove the var reached the container.
#[tokio::test]
async fn the_effective_config_names_the_mirrored_url() {
    let db = common::TempDb::open("policy-config.db").await;
    let url = "https://cdn.pollis.com/dev/client-policy.json";
    for (mirror, expected) in [
        (PolicyMirror::new(Some(url.to_string())), serde_json::json!(url)),
        (PolicyMirror::new(None), serde_json::Value::Null),
    ] {
        let st = state(db.arc(), mirror).with_metrics_token(Some("t".into()));
        let req = Request::builder()
            .method("GET")
            .uri("/v1/config")
            .header("Authorization", "Bearer t")
            .body(Body::empty())
            .unwrap();
        let (status, body) = call(&st, req).await;
        assert_eq!(status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["client_policy_url"], expected);
    }
}
