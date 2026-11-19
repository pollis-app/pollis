//! The alerts App in the real sandbox: run with `pollis app test`.

use pollis_app::testing::{Grants, JobOptions, Post, TestApp, TestOptions};
use serde_json::json;

const GENERAL: &str = "01GENERAL";

/// Installed as an admin would: `#general` granted, under its own id.
fn app() -> TestApp {
    let grants = Grants::new().channel("general", GENERAL).actions(["post"]);
    TestApp::with_options(TestOptions { job: JobOptions::new().grants(grants), ..TestOptions::default() })
}

fn post(channel: &str, text: &str) -> Post {
    Post { channel: channel.to_string(), text: text.to_string() }
}

#[test]
fn a_valid_body_posts_one_message() {
    let result = app().webhook_json(&json!({ "channel": "general", "text": "disk 91% on db-1" }));
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(result.posts, [post(GENERAL, "disk 91% on db-1")]);
    assert_eq!(result.actions.len(), 1);
}

#[test]
fn a_channel_is_named_by_id_or_by_name_with_or_without_a_hash_in_any_case() {
    let app = app();
    for channel in [GENERAL, "general", "#general", "GENERAL", "#General"] {
        let result = app.webhook_json(&json!({ "channel": channel, "text": "hi" }));
        assert_eq!(result.posts, [post(GENERAL, "hi")], "{channel}");
    }
}

#[test]
fn fields_it_does_not_know_are_ignored() {
    let result = app().webhook_json(&json!({ "channel": "general", "text": "hi", "severity": "high" }));
    assert_eq!(result.posts, [post(GENERAL, "hi")]);
}

#[test]
fn a_malformed_body_posts_nothing_and_says_what_to_send() {
    let app = app();
    let bodies: [&[u8]; 7] = [
        b"",
        b"not json",
        b"[]",
        br#"{"text":"no channel"}"#,
        br#"{"channel":"general"}"#,
        br#"{"channel":"general","text":1}"#,
        b"\xff\xfe",
    ];
    for body in bodies {
        let result = app.webhook(body);
        assert!(!result.ok, "{body:?}");
        assert!(result.actions.is_empty(), "{body:?}");
        let error = result.error.expect("the job's error");
        assert_eq!(error.kind, "handler_failed");
        assert!(error.message.contains(r#"send a JSON body {"channel": "...", "text": "..."}"#), "{}", error.message);
    }
}

#[test]
fn a_channel_the_admin_did_not_grant_is_refused() {
    let options = JobOptions::new().grants(Grants::new().channel("ops", "ops").actions(["post"]));
    let result = TestApp::new().with(options).webhook_json(&json!({ "channel": "general", "text": "hi" }));
    assert!(!result.ok);
    assert!(result.actions.is_empty());
    assert!(result.error.expect("refused").message.contains("not in `read.channels`"));
}

#[test]
fn it_asks_for_nothing_beyond_posting_from_its_webhook() {
    let manifest = app().manifest().clone();
    assert_eq!(manifest["triggers"], json!(["webhook"]));
    assert_eq!(manifest["actions"], json!(["post"]));
    assert_eq!(manifest["egress"], json!([]));
    assert_eq!(manifest["read"]["history"], json!(0));
    assert_eq!(manifest["state"]["quota"], json!(0));
}
