//! The offline runner goes through the real App host: grant refusals, state,
//! HTTP fixtures and the wire format, using the host's script-driven fixture
//! App (`pollis-app-host/fixtures/test-app.wasm`).

use std::path::PathBuf;

use pollis_app_dev::{TestHost, TestRequest, WireAction};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pollis-app-host/fixtures")
        .join(name)
}

fn manifest(dir: &tempdir::Dir, json: &str) -> PathBuf {
    let path = dir.0.join("pollis-app.json");
    std::fs::write(&path, json).unwrap();
    path
}

mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Dir {
        pub fn new(tag: &str) -> Dir {
            let path =
                std::env::temp_dir().join(format!("pollis-app-dev-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Dir(path)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

const MANIFEST: &str = r##"{
  "name": "test-app", "version": "0.1.0",
  "triggers": ["commands", "messages"],
  "read": { "channels": ["#support"] },
  "actions": ["reply"],
  "state": { "quota": 64 },
  "egress": ["api.example.com"],
  "commands": [{ "name": "run", "description": "Run a test script" }]
}"##;

fn request(
    dir: &tempdir::Dir,
    trigger: serde_json::Value,
    extra: serde_json::Value,
) -> TestRequest {
    let mut v = serde_json::json!({
        "component": fixture("test-app.wasm"),
        "manifest": manifest(dir, MANIFEST),
        "trigger": trigger,
    });
    if let (Some(obj), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in extra {
            obj.insert(k.clone(), val.clone());
        }
    }
    serde_json::from_value(v).unwrap()
}

fn command(args: &str, channel: &str) -> serde_json::Value {
    serde_json::json!({ "command": {
        "name": "run", "args": args,
        "message": { "id": "m1", "channel": channel, "author": { "id": "u1", "displayName": "Ann" },
                     "text": format!("/run {args}"), "sentAt": 1_800_000_000_000u64 }
    }})
}

fn texts(result: &pollis_app_dev::TestResult) -> Vec<String> {
    result.logs.iter().map(|l| l.text.clone()).collect()
}

#[test]
fn a_granted_reply_is_queued_and_returned() {
    let dir = tempdir::Dir::new("reply");
    let host = TestHost::new().unwrap();
    let result = host.run(request(
        &dir,
        command("reply m1 hello", "support"),
        serde_json::json!({}),
    ));
    assert!(result.ok, "{result:?}");
    assert_eq!(
        result.actions,
        vec![WireAction::Reply {
            channel: "support".into(),
            message: "m1".into(),
            text: "hello".into()
        }]
    );
}

#[test]
fn refusals_are_the_hosts_own_words() {
    let dir = tempdir::Dir::new("refuse");
    let host = TestHost::new().unwrap();
    let result = host.run(request(
        &dir,
        command(
            "post support hi; fetch https://evil.example.net/",
            "support",
        ),
        serde_json::json!({}),
    ));
    assert!(result.ok, "{result:?}");
    assert!(result.actions.is_empty());
    let logs = texts(&result).join("\n");
    assert!(
        logs.contains("err post support hi: `post` refused"),
        "{logs}"
    );
    assert!(logs.contains("not in `egress`"), "{logs}");
}

#[test]
fn an_ungranted_channel_is_never_delivered() {
    let dir = tempdir::Dir::new("channel");
    let host = TestHost::new().unwrap();
    let result = host.run(request(
        &dir,
        command("reply m1 hi", "random"),
        serde_json::json!({}),
    ));
    assert!(!result.ok);
    let error = result.error.unwrap();
    assert_eq!(error.kind, "not_delivered");
    assert!(
        error.message.contains("not in `read.channels`"),
        "{}",
        error.message
    );
}

#[test]
fn state_and_http_come_from_fixtures_never_the_network() {
    let dir = tempdir::Dir::new("fixtures");
    let host = TestHost::new().unwrap();
    let result = host.run(request(
        &dir,
        command(
            "kv-get seen; kv-set seen yes; fetch https://api.example.com/v1; fetch https://api.example.com/missing",
            "support",
        ),
        serde_json::json!({
            "state": { "seen": [110, 111] },
            "http": [{ "url": "https://api.example.com/v1", "body": [111, 107] }]
        }),
    ));
    assert!(result.ok, "{result:?}");
    let logs = texts(&result).join("\n");
    assert!(logs.contains("ok kv-get seen: no"), "{logs}");
    assert!(logs.contains("200 ok"), "{logs}");
    assert!(
        logs.contains("no HTTP fixture for GET https://api.example.com/missing"),
        "{logs}"
    );
    assert_eq!(result.state_writes.len(), 1);
    assert_eq!(result.state_writes[0].value.as_deref(), Some(&b"yes"[..]));
    assert_eq!(result.fetches.len(), 2);
}

#[test]
fn a_failing_handler_returns_nothing() {
    let dir = tempdir::Dir::new("fail");
    let host = TestHost::new().unwrap();
    let result = host.run(request(
        &dir,
        command("reply m1 hi; fail nope", "support"),
        serde_json::json!({}),
    ));
    assert!(!result.ok);
    assert_eq!(result.error.as_ref().unwrap().kind, "handler_failed");
    assert!(result.actions.is_empty());
}

#[test]
fn a_component_importing_sockets_is_refused_at_check() {
    let host = TestHost::new().unwrap();
    let bytes = std::fs::read(fixture("net-app.wasm")).unwrap();
    let error = host.check(&bytes).unwrap_err();
    assert!(
        error.contains("no sockets or direct network access"),
        "{error}"
    );
    host.check(&std::fs::read(fixture("std-app.wasm")).unwrap())
        .unwrap();
}

#[test]
fn trigger_fields_are_camel_case_on_the_wire() {
    let dir = tempdir::Dir::new("webhook");
    let host = TestHost::new().unwrap();
    let manifest = dir.0.join("hook.json");
    std::fs::write(
        &manifest,
        r##"{ "name": "hook", "version": "1", "triggers": ["webhook"], "read": { "channels": ["#support"] }, "actions": ["post"] }"##,
    )
    .unwrap();
    let request: TestRequest = serde_json::from_value(serde_json::json!({
        "component": fixture("test-app.wasm"),
        "manifest": manifest,
        "trigger": { "webhook": { "deliveryId": "d1", "headers": [["x", "y"]], "body": b"post support deployed".to_vec() } },
    }))
    .unwrap();
    let result = host.run(request);
    assert!(result.ok, "{result:?}");
    assert_eq!(
        result.actions,
        vec![WireAction::Post {
            channel: "support".into(),
            text: "deployed".into()
        }]
    );
}

#[test]
fn mentions_follow_the_runtimes_rule() {
    use pollis_app_dev::mentions;
    assert!(mentions("hey @Test_Bot, look", "test_bot"));
    assert!(mentions("@test_bot", "test_bot"));
    assert!(!mentions("@all please", "test_bot"));
    assert!(!mentions("mail@test_bot", "test_bot"));
    assert!(!mentions("@test_bots", "test_bot"));
}

#[test]
fn a_sent_message_is_routed_like_the_runtime_routes_it() {
    let dir = tempdir::Dir::new("send");
    let host = TestHost::new().unwrap();
    let send = |text: &str| {
        serde_json::json!({ "send": {
            "id": "m1", "channel": "support", "author": { "id": "u1", "displayName": "Ann" },
            "text": text, "sentAt": 1_800_000_000_000u64
        }})
    };
    // A declared command, addressed to this bot: on-command with the args after the target.
    let r = host.run(request(
        &dir,
        send("/run @test_bot reply m1 hi"),
        serde_json::json!({}),
    ));
    assert!(r.ok, "{r:?}");
    assert!(
        r.logs[0].text.starts_with("trigger command run"),
        "{:?}",
        r.logs
    );
    // Addressed to another bot: not a command for this one, so an ordinary message.
    let r = host.run(request(
        &dir,
        send("/run @other_bot reply m1 hi"),
        serde_json::json!({}),
    ));
    assert!(
        r.logs[0].text.starts_with("trigger message"),
        "{:?}",
        r.logs
    );
    // An undeclared command is an ordinary message too.
    let r = host.run(request(&dir, send("/deploy now"), serde_json::json!({})));
    assert!(
        r.logs[0].text.starts_with("trigger message"),
        "{:?}",
        r.logs
    );
}
