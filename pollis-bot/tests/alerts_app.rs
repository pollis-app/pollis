//! #2007: the published alerts App, through the real webhook router, the
//! runtime, the approval check and the real App sandbox. Only the Delivery
//! Service side is fake (`FakeCore`).

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use pollis_bot::app_approval::ArtifactLocation;
use pollis_bot::installations::InstalledApp;
use pollis_bot::job::Installation;
use pollis_bot::ratelimit::TokenBucket;
use pollis_bot::runtime::Runtime;
use pollis_bot::status::StatusBoard;
use pollis_bot::webhook::{router, HookEntry, JobSubmitter, WebhookState};
use tower::ServiceExt;
use zeroize::Zeroizing;

const SECRET: &str = "pbw_alerts-test";

/// One host running the alerts App the way `pollis-bot run` builds it.
fn host(ds: Arc<FakeDs>, approved: bool) -> axum::Router {
    let core = FakeCore::new("01HOSTA", ds);
    if approved {
        *core.approval.lock().unwrap() = Some(alerts_approval());
    }
    let app = InstalledApp {
        component: ArtifactLocation(format!("{ALERTS_APP_DIR}/app.wasm")),
        manifest: ArtifactLocation(format!("{ALERTS_APP_DIR}/pollis-app.json")),
    };
    let installation = Arc::new(Installation::new(core, app.handler()));
    let runtime = Runtime::new(vec![installation], 8, Arc::new(StatusBoard::new(None, None))).unwrap();
    let mut hooks = HashMap::new();
    hooks.insert(
        BOT.to_string(),
        HookEntry::new(Zeroizing::new(SECRET.as_bytes().to_vec()), TokenBucket::new(600, 100, Instant::now())),
    );
    let submitter: Arc<dyn JobSubmitter> = Arc::new(runtime);
    router(Arc::new(WebhookState::new(hooks, submitter)))
}

async fn send(app: &axum::Router, body: &'static [u8]) -> (StatusCode, serde_json::Value) {
    let req = Request::post(format!("/hooks/{BOT}"))
        .header("authorization", format!("Bearer {SECRET}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn the_fixture_is_the_published_build() {
    let published = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../apps/alerts/APP_HASH")).unwrap();
    let hash = pollis_app_manifest::app_hash(&alerts_app_component(), &alerts_app_manifest()).unwrap();
    assert_eq!(
        pollis_app_manifest::hex(&hash),
        published.trim(),
        "rebuild apps/alerts and copy dist/ to pollis-bot/tests/fixtures/alerts-app and its hash to apps/alerts/APP_HASH"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_valid_body_posts_one_message_whichever_way_it_names_the_channel() {
    let ds = Arc::new(FakeDs::default());
    let app = host(ds.clone(), true);
    let bodies: [&[u8]; 3] = [
        br#"{"channel":"alerts","text":"one"}"#,
        br##"{"channel":"#ALERTS","text":"two"}"##,
        br#"{"channel":"01ALERTSCHANNEL","text":"three"}"#,
    ];
    for body in bodies {
        let (status, answer) = send(&app, body).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["status"], "posted");
        assert_eq!(answer["posted"], 1);
    }
    let posts: Vec<(String, String)> = ds.posts().into_iter().map(|(_, c, t)| (c, t)).collect();
    let expected = ["one", "two", "three"].map(|t| (CHANNEL.to_string(), t.to_string()));
    assert_eq!(posts, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_body_posts_nothing_and_the_sender_is_told_what_to_send() {
    let ds = Arc::new(FakeDs::default());
    let app = host(ds.clone(), true);
    let bodies: [&[u8]; 4] = [b"not json", b"{}", br#"{"text":"no channel"}"#, br#"{"channel":"alerts","text":1}"#];
    for body in bodies {
        let (status, answer) = send(&app, body).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{answer}");
        assert_eq!(answer["status"], "failed");
        let error = answer["error"].as_str().unwrap();
        assert!(error.contains(r#"bad_request: send a JSON body {"channel": "...", "text": "..."}"#), "{error}");
    }
    assert!(ds.posts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_the_admin_did_not_grant_posts_nothing() {
    let ds = Arc::new(FakeDs::default());
    let app = host(ds.clone(), true);
    let (status, answer) = send(&app, br#"{"channel":"payroll","text":"leak"}"#).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{answer}");
    assert!(answer["error"].as_str().unwrap().contains("`post` to `payroll` refused: not in `read.channels`"), "{answer}");
    assert!(ds.posts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_text_is_refused_by_the_loop_as_before() {
    let ds = Arc::new(FakeDs::default());
    let app = host(ds.clone(), true);
    let (status, answer) = send(&app, br#"{"channel":"alerts","text":"   "}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
    assert_eq!(answer["status"], "refused");
    assert!(ds.posts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_party_gets_no_bypass_without_an_admins_approval_nothing_posts() {
    let ds = Arc::new(FakeDs::default());
    let app = host(ds.clone(), false);
    let (status, answer) = send(&app, br#"{"channel":"alerts","text":"unapproved"}"#).await;
    assert_ne!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["status"], "failed");
    assert!(ds.posts().is_empty());
}
