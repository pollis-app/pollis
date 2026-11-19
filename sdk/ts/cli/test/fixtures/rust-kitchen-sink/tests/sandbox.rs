//! `pollis_app` in the real sandbox, through `pollis_app::testing`.

use pollis_app::testing::{Action, Grants, HttpFixture, JobOptions, MessageInput, Post, TestApp};
use serde_json::json;

const TICKETS: &str = "https://api.example.com/tickets";

#[test]
fn state_round_trips_and_is_written_by_a_job_that_succeeds() {
    let app = TestApp::new();
    let first = app.command("count", "");
    assert_eq!(first.replies, ["count is 1"]);
    assert_eq!(first.state["count"].as_deref(), Some("1"));
    let later = app.with(JobOptions::new().state("count", "41")).command("count", "");
    assert_eq!(later.replies, ["count is 42"]);
}

#[test]
fn fetch_is_answered_from_fixtures() {
    let fixture = HttpFixture::new(TICKETS)
        .method("POST")
        .status(201)
        .header("X-Kind", "bug")
        .json(&json!({ "id": "T-7" }));
    let result = TestApp::new()
        .with(JobOptions::new().http(fixture))
        .command("ticket", "Printer on fire");
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.replies, ["Filed T-7 (201, bug)"]);
    assert_eq!(result.fetches[0].method, "POST");
    assert_eq!(result.fetches[0].body.as_deref(), Some(r#"{"title":"Printer on fire"}"#));
}

#[test]
fn a_host_not_in_egress_is_refused_with_the_runtimes_own_message() {
    let result = TestApp::new().command("leak", "");
    assert_eq!(result.replies, ["refused: `http-fetch` to `evil.example.net` refused: not in `egress`"]);
    assert!(result.fetches.is_empty());
}

#[test]
fn an_action_not_in_the_manifest_fails_the_job_and_nothing_is_sent() {
    let result = TestApp::new().command("edit", "");
    assert!(!result.ok);
    let error = result.error.expect("the job's error");
    assert_eq!(error.kind, "handler_failed");
    assert!(error.message.contains("`edit-own` refused: not in `actions`"), "{}", error.message);
    assert!(result.actions.is_empty());
}

#[test]
fn a_returned_error_fails_the_job_with_its_message() {
    let result = TestApp::new().command("throw", "");
    assert!(!result.ok);
    assert!(result.error.expect("the job's error").message.contains("something broke"));
}

#[test]
fn a_panic_fails_the_job() {
    let result = TestApp::new().command("panic", "");
    assert!(!result.ok);
    assert_eq!(result.error.expect("the job's error").kind, "trap");
    assert!(result.actions.is_empty());
}

#[test]
fn messages_are_routed_as_the_runtime_routes_them() {
    let app = TestApp::new();
    assert_eq!(app.send("/count @test_bot").replies, ["count is 1"]);
    assert_eq!(app.send("/count @other_bot help").replies, ["Try /ticket"]);
    assert_eq!(app.send("/deploy help").replies, ["Try /ticket"]);
    assert_eq!(app.send("hey @Test_Bot help").replies, ["you rang"]);
    assert_eq!(app.send("@all help").replies, ["Try /ticket"]);
}

#[test]
fn history_comes_from_granted_channels_only() {
    let at = 1_799_996_400_000;
    let options = JobOptions::new()
        .message(MessageInput::new("first").channel("support").sent_at(at))
        .message(MessageInput::new("elsewhere").channel("random").sent_at(at))
        .message(MessageInput::new("too old").channel("support").sent_at(1_767_225_600_000));
    let result = TestApp::new().with(options).command("history", "");
    assert_eq!(result.replies, ["first"]);
}

#[test]
fn randomness_is_fresh_per_job_and_printing_reaches_the_log() {
    let app = TestApp::new();
    let (a, b) = (app.command("random", ""), app.command("random", ""));
    let line = |r: &pollis_app::testing::JobResult| {
        r.logs.iter().find(|l| l.text.starts_with("random ")).map(|l| l.text.clone())
    };
    assert!(line(&a).is_some());
    assert_ne!(line(&a), line(&b));
    assert_eq!(a.reactions, ["🎲"]);
    assert!(a.logs.iter().any(|l| l.text.contains("hello from println")), "{:?}", a.logs);
}

#[test]
fn messages_schedules_and_webhooks_reach_their_handlers() {
    let app = TestApp::new();
    assert_eq!(app.message("I need help").replies, ["Try /ticket"]);
    let scheduled = app.schedule("daily");
    let expected = "schedule daily at 1799917200000 now 1800000000000";
    assert!(scheduled.logs.iter().any(|l| l.text == expected), "{:?}", scheduled.logs);
    assert_eq!(app.schedule("weekly").error.expect("undelivered").kind, "not_delivered");
    let hook = app.webhook_json(&json!({ "text": "deploy finished", "channel": "support" }));
    let post = Post { channel: "support".to_string(), text: "deploy finished".to_string() };
    assert_eq!(hook.posts, [post]);
    assert!(matches!(hook.actions[0], Action::Post { .. }));
    assert!(hook.logs.iter().any(|l| l.text == "webhook application/json"), "{:?}", hook.logs);
}

#[test]
fn a_channel_the_app_was_not_granted_never_reaches_it() {
    let result = TestApp::new().message(MessageInput::new("help").channel("random"));
    assert_eq!(result.error.expect("undelivered").kind, "not_delivered");
}

#[test]
fn a_narrower_grant_than_the_manifest_is_enforced() {
    let grants = Grants::new().channel("support", "support").actions(["reply"]);
    let options = JobOptions::new()
        .grants(grants)
        .http(HttpFixture::new(TICKETS).json(&json!({ "id": "T-1" })));
    let result = TestApp::new().with(options).command("ticket", "x");
    assert!(!result.ok);
    assert!(result.error.expect("refused").message.contains("not in `egress`"));
}
