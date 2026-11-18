//! The runtime end to end over the fake core: the installations limit, and a
//! webhook call through the real router, the runtime's job submission and the
//! job loop.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{FakeCore, FakeDs, BOT, CHANNEL};
use http_body_util::BodyExt;
use pollis_bot::handler::AlertsHandler;
use pollis_bot::job::Installation;
use pollis_bot::ratelimit::TokenBucket;
use pollis_bot::runtime::Runtime;
use pollis_bot::status::{InstallationState, StatusBoard};
use pollis_bot::webhook::{router, HookEntry, JobSubmitter, WebhookState, DELIVERY_HEADER};
use tower::ServiceExt;
use zeroize::Zeroizing;

const SECRET: &str = "pbw_runtime-test";

fn host(device: &str, ds: Arc<FakeDs>) -> (Arc<Runtime>, Arc<StatusBoard>) {
    let core = FakeCore::new(device, ds);
    let inst = Arc::new(Installation::new(core, Arc::new(AlertsHandler)));
    let status = Arc::new(StatusBoard::new(None, None));
    (Runtime::new(vec![inst], 8, status.clone()).unwrap(), status)
}

fn app(runtime: Arc<Runtime>) -> axum::Router {
    let mut hooks = HashMap::new();
    hooks.insert(
        BOT.to_string(),
        HookEntry::new(Zeroizing::new(SECRET.as_bytes().to_vec()), TokenBucket::new(30, 10, Instant::now())),
    );
    let submitter: Arc<dyn JobSubmitter> = Arc::new(runtime);
    router(Arc::new(WebhookState::new(hooks, submitter)))
}

async fn call(app: &axum::Router, delivery: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::post(format!("/hooks/{BOT}"))
        .header("authorization", format!("Bearer {SECRET}"))
        .header(DELIVERY_HEADER, delivery)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"channel":"alerts","text":"CPU on fire"}"#))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn the_runtime_refuses_more_installations_than_configured() {
    let ds = Arc::new(FakeDs::default());
    let insts: Vec<_> = (0..3)
        .map(|_| Arc::new(Installation::new(FakeCore::new("01HOST", ds.clone()), Arc::new(AlertsHandler))))
        .collect();
    let status = Arc::new(StatusBoard::new(None, None));
    let err = Runtime::new(insts, 2, status).err().expect("over the limit");
    assert_eq!((err.count, err.max), (3, 2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_webhook_sent_to_two_hosts_of_one_bot_posts_once() {
    let ds = Arc::new(FakeDs::default());
    let (a, status_a) = host("01HOSTA", ds.clone());
    let (b, _) = host("01HOSTB", ds.clone());

    let (status, body) = call(&app(a), "alert-7").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "posted");

    let (status, body) = call(&app(b), "alert-7").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "claimed_elsewhere");

    let posts = ds.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].1.as_str(), posts[0].2.as_str()), (CHANNEL, "CPU on fire"));
    assert_eq!(status_a.snapshot().installations[BOT].state, InstallationState::Connected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wake_catches_up_without_any_timer() {
    let ds = Arc::new(FakeDs::default());
    let (runtime, status) = host("01HOSTA", ds.clone());
    runtime.wake_all();
    // The wake job runs on its own task; wait for its status to land.
    let deadline = Instant::now() + Duration::from_secs(5);
    while status.snapshot().installations[BOT].state != InstallationState::Connected {
        assert!(Instant::now() < deadline, "the wake job never finished");
        tokio::task::yield_now().await;
    }
}
