//! #913 — every outbound HTTP call the DS makes has a deadline.
//!
//! The DS calls out to three upstreams: LiveKit (Twirp `SendData` /
//! `ListParticipants`), Resend (the sign-in OTP email) and the Turso Platform API
//! (minting a short-TTL read-only DB token). None of them had a timeout, and
//! `reqwest`'s default is *no* timeout — so an upstream that completes the TCP
//! handshake and then never sends a response byte pinned the axum handler that
//! called it for as long as it cared to hold the socket. Since #875 those calls
//! share one pooled `reqwest::Client`, so a hung upstream also sat on that pool's
//! connections; the damage was never confined to the one unlucky request.
//!
//! The failure mode is specifically a **black hole**, not a refused connection: a
//! refusal returns an error immediately and was always handled. So the double
//! below accepts the connection, reads the request, and then simply never
//! answers — the one case that used to hang forever.
//!
//! `ListParticipants` is the leg driven end-to-end here because it is the only
//! one whose upstream URL is configurable (`LIVEKIT_URL`): Resend and the Turso
//! Platform API are hardcoded hostnames. The deadline itself lives on
//! `util::Upstream` and is shared by all three, so the table test below is what
//! covers the other two.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::broker::BrokerConfig;
use pollis_delivery::db::Db;
use pollis_delivery::util::Upstream;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

/// An upstream that accepts connections and never replies. Sockets are parked in
/// a `Vec` so nothing closes them and turns the hang into a clean EOF.
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

async fn fresh_db() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("delivery.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().expect("utf-8 path"))
        .await
        .expect("local db");
    pollis_schema::apply::single_db(&db.conn().await.expect("checkout"))
        .await
        .expect("schema");
    Arc::new(db)
}

fn livekit_broker(url: &str) -> BrokerConfig {
    BrokerConfig {
        livekit_api_key: Some("APIkey".into()),
        livekit_api_secret: Some("secret-secret-secret-secret".into()),
        livekit_url: Some(url.to_string()),
        ..BrokerConfig::default()
    }
}

/// The defect, end to end: a LiveKit that never answers must produce a response
/// the client can act on, not a socket that hangs until something else gives up.
///
/// Against the pre-#913 code this test does not fail with a wrong status — it
/// **never returns**, which is exactly the complaint. The outer `timeout` is what
/// turns that hang into a reported failure.
#[tokio::test(flavor = "multi_thread")]
async fn a_hung_livekit_returns_504_instead_of_hanging_the_handler() {
    let url = black_hole().await;
    let state = AppState::new(fresh_db().await, false).with_broker_config(livekit_broker(&url));

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
        // Generously past the 5s LiveKit deadline. Blowing THIS budget is the
        // pre-fix behaviour: no deadline at all, so the handler waits forever.
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

/// The deadline is a property of the upstream, and every upstream has one. A
/// variant added later with no timeout — or with a zero/absurd one — is the way
/// this regresses without any handler changing. Guard.
#[test]
fn every_upstream_declares_a_sane_deadline() {
    for upstream in [Upstream::LiveKit, Upstream::Resend, Upstream::TursoPlatform] {
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

/// The structural half of the fix. `util::http_client` is private, so the only
/// way to make an outbound request is `util::http_post`, which cannot be called
/// without naming an [`Upstream`] and therefore a deadline. That holds only while
/// nobody reaches for `reqwest` directly — which is a grep, because no type
/// signature can express it.
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
            // `util.rs` owns the client — that is the whole point.
            if rel == std::path::Path::new("src/util.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source");
            for (i, line) in text.lines().enumerate() {
                // Comments may discuss it; code may not construct it.
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
