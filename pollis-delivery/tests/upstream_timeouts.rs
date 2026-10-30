//! #913 — every outbound HTTP call the DS makes has a deadline.
//!
//! The failure mode is a black hole (accept, read, never answer), not a refused connection.
//! Only `ListParticipants` has a configurable URL; the table test covers the other upstreams.

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::broker::BrokerConfig;
use pollis_delivery::util::Upstream;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

/// Accepts and never replies; sockets are parked so nothing turns the hang into EOF.
async fn black_hole() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind black hole");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    format!("http://{addr}")
}

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

/// A LiveKit that never answers must still produce a response; pre-#913 this never returns.
#[tokio::test(flavor = "multi_thread")]
async fn a_hung_livekit_returns_504_instead_of_hanging_the_handler() {
    let url = black_hole().await;
    let db = fresh_db().await;
    let state = AppState::new(db.arc(), false).with_broker_config(livekit_broker(&url));

    let req = Request::builder()
        .method("POST")
        .uri("/v1/livekit/participants")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "room": "inbox-alice", "user_id": "alice" }).to_string(),
        ))
        .unwrap();

    let started = Instant::now();
    let resp = tokio::time::timeout(
        // Generously past the 5s LiveKit deadline.
        Duration::from_secs(30),
        build_router_with_state(state).oneshot(req),
    )
    .await
    .expect("the handler must give up on a hung upstream, not wait forever")
    .expect("router");
    let elapsed = started.elapsed();

    assert_eq!(
        resp.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "a hung upstream is a 504 (retryable), not a 502 (bad answer) and not a hang"
    );
    assert!(
        elapsed >= Upstream::LiveKit.timeout(),
        "the deadline must be the configured one, not something shorter that \
         would cut off a slow-but-working upstream; gave up after {elapsed:?}"
    );
    assert!(
        elapsed < Upstream::LiveKit.timeout() + Duration::from_secs(10),
        "gave up {elapsed:?} after a {:?} deadline — the timeout is not what ended the request",
        Upstream::LiveKit.timeout()
    );

    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json["retryable"],
        serde_json::json!(true),
        "a 504 says the request may succeed on a retry; got {json}"
    );
    assert!(
        json["error"].as_str().unwrap_or_default().contains("livekit"),
        "the body must name the upstream that stalled; got {json}"
    );
}

/// Every upstream variant must carry a sane deadline.
#[test]
fn every_upstream_declares_a_sane_deadline() {
    for upstream in [Upstream::LiveKit, Upstream::Resend, Upstream::ExpoPush, Upstream::ClientPolicy] {
        let t = upstream.timeout();
        assert!(
            t >= Duration::from_secs(1),
            "{upstream:?} deadline {t:?} is too tight to be anything but a source of \
             spurious failures against a working upstream"
        );
        assert!(
            t <= Duration::from_secs(30),
            "{upstream:?} deadline {t:?} is long enough that a hung upstream still \
             occupies a handler and a pooled connection for the whole window"
        );
        assert!(!upstream.name().is_empty(), "{upstream:?} must name itself for logs and 504 bodies");
    }
}

/// Only `util::http_post` / `util::http_get` may make outbound requests, so nobody may reach for `reqwest` directly.
#[test]
fn no_delivery_source_builds_its_own_http_client() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let rel = path.strip_prefix(root).expect("under manifest").to_path_buf();
            if rel == std::path::Path::new("src/util.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source");
            for (i, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if line.contains("reqwest::Client") || line.contains("ClientBuilder") {
                    offenders.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "outbound HTTP goes through `util::http_post`, which forces an `Upstream` \
         and therefore a timeout (#913). A hand-built client has no deadline:\n  {}",
        offenders.join("\n  ")
    );
}
