//! Slash commands and bot mentions (#2018), over the real core, the in-process
//! DS and the `pollis-bot` job loop with the fixture App. A command is an
//! ordinary message; the runtime alone decides whether it reaches an
//! installation's `on-command`.
//!
//! The fixture App's `on-command` runs the command's arguments as its script
//! and `on-mention` runs the message's text, so `post <channel> <text>` posts
//! `<text>` only if the trigger reached that handler with what was sent.

use std::sync::Arc;

use serial_test::serial;

use pollis_app_manifest::{ActionKind, AdminGrants, ChannelGrant, EventKind, Manifest};
use pollis_bot::app_handler::AppHandler;
use pollis_bot::core_host::installation_core;
use pollis_bot::handler::Handler;
use pollis_bot::job::{Installation, JobInput};
use pollis_core::commands::bots;
use pollis_core::commands::bots::app_commands::{self, ApprovedApp, BOT_REQUESTS_UNAVAILABLE};

use crate::harness::{pair_new_bot, wipe, world, BotHostClient, TestClient};

const LINEAR: &str = r##"{
    "name": "linear", "version": "1.0.0",
    "triggers": ["commands", "mentions"],
    "read": { "channels": ["#general"] },
    "actions": ["post"],
    "commands": [{ "name": "ticket", "description": "File a Linear issue", "arg_hint": "<title>" }]
}"##;

const JIRA: &str = r##"{
    "name": "jira", "version": "1.0.0",
    "triggers": ["commands", "mentions", "requests"],
    "read": { "channels": ["#general"] },
    "actions": ["post"],
    "commands": [
        { "name": "ticket", "description": "File a Jira issue", "arg_hint": "<title>" },
        { "name": "forget-me", "description": "Ask for your data", "request": true }
    ]
}"##;

fn grants(general: &str) -> AdminGrants {
    AdminGrants {
        channels: vec![ChannelGrant { id: general.to_string(), name: "general".into() }],
        events: [EventKind::Created].into(),
        actions: [ActionKind::Post].into(),
        ..Default::default()
    }
}

fn app(manifest: &str, general: &str) -> Arc<dyn Handler> {
    let wasm = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../pollis-app-host/fixtures/test-app.wasm"))
        .expect("fixture App");
    Arc::new(AppHandler::new(wasm, manifest.as_bytes(), grants(general)).expect("App handler"))
}

/// Seal `host` and return the runtime's handle on it (unseals per job).
async fn asleep(host: &BotHostClient, handler: Arc<dyn Handler>) -> Installation {
    bots::stop_unattended(&host.state).await;
    let core = installation_core(world().await.config.clone(), host.keystore.clone(), host.dir.clone(), host.session().clone());
    Installation::new(core, handler)
}

async fn contents(client: &TestClient, channel: &str) -> Vec<String> {
    client
        .fetch_channel_messages(channel)
        .await
        .into_iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

struct Setup {
    alice: TestClient,
    bob: TestClient,
    general: String,
    random: String,
    linear: Installation,
    jira: Installation,
    linear_name: String,
    jira_name: String,
    jira_id: String,
    group: String,
}

impl Setup {
    /// Both bots catch up (a job per installation, as wake-ups would run).
    async fn run_bots(&self) {
        self.linear.run_job(JobInput::Wake).await.expect("linear job");
        self.jira.run_job(JobInput::Wake).await.expect("jira job");
    }
}

/// Alice's group with #general and #random, Bob a member, and two Apps that
/// both declare `/ticket`, granted #general only and approved (through the
/// approval seam until #2005 lands).
async fn setup() -> Setup {
    wipe().await;
    app_commands::test_hooks::clear();
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let group = alice.create_group("Support").await;
    let general = alice.general_channel_id(&group).await;
    let random = alice.create_channel(&group, "random").await;
    alice.invite(&group, &bob_p.username).await;
    let invite = bob.first_pending_invite().await.expect("pending invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    alice.poll().await;

    let (linear_host, linear_bot) = pair_new_bot(&alice, &group, "Linear").await;
    let (jira_host, jira_bot) = pair_new_bot(&alice, &group, "Jira").await;
    for (bot, manifest) in [(&linear_bot, LINEAR), (&jira_bot, JIRA)] {
        let id = bot["userId"].as_str().expect("bot id").to_string();
        app_commands::test_hooks::approve(ApprovedApp {
            installation_id: id.clone(),
            bot_user_id: id,
            manifest: Manifest::from_json(manifest.as_bytes()).expect("manifest"),
            grants: grants(&general),
        });
    }
    let linear = asleep(&linear_host, app(LINEAR, &general)).await;
    let jira = asleep(&jira_host, app(JIRA, &general)).await;
    let s = Setup {
        linear_name: linear_bot["username"].as_str().expect("username").to_string(),
        jira_name: jira_bot["username"].as_str().expect("username").to_string(),
        jira_id: jira_bot["userId"].as_str().expect("bot id").to_string(),
        alice,
        bob,
        general,
        random,
        linear,
        jira,
        group,
    };
    // Seed the cursors: history is never replayed as triggers.
    s.run_bots().await;
    s
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn members_see_each_apps_commands_only_where_it_is_granted_and_shared_names_disambiguated() {
    let s = setup().await;
    let listed = s.bob.invoke_json("list_group_commands", serde_json::json!({ "groupId": s.group })).await;
    let summary: Vec<(String, String, bool, bool)> = listed
        .as_array()
        .expect("commands")
        .iter()
        .map(|c| {
            assert_eq!(c["channels"], serde_json::json!([s.general]), "{c}");
            (
                c["command"].as_str().unwrap().to_string(),
                c["appName"].as_str().unwrap().to_string(),
                c["needsTarget"].as_bool().unwrap(),
                c["request"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("forget-me".into(), "jira".into(), false, true),
            ("ticket".into(), "jira".into(), true, false),
            ("ticket".into(), "linear".into(), true, false),
        ]
    );
    let linear = listed.as_array().unwrap().iter().find(|c| c["appName"] == "linear").unwrap();
    assert_eq!(linear["botUsername"], s.linear_name.as_str());
    assert_eq!(linear["argHint"], "<title>");
    app_commands::test_hooks::clear();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_command_reaches_on_command_with_its_arguments_and_only_the_picked_app() {
    let s = setup().await;
    let g = &s.general;
    // The composer sends a shared name with the picked bot's mention.
    s.bob.send_channel_message(g, &format!("/ticket @{} post {g} linear filed it", s.linear_name)).await;
    s.bob.send_channel_message(g, &format!("/ticket @{} post {g} jira filed it", s.jira_name)).await;
    s.run_bots().await;
    let all = contents(&s.alice, g).await;
    assert_eq!(all.iter().filter(|t| *t == "linear filed it").count(), 1, "{all:?}");
    assert_eq!(all.iter().filter(|t| *t == "jira filed it").count(), 1, "{all:?}");
    // The bot's reply is an ordinary message: Bob sees it too.
    assert!(contents(&s.bob, g).await.contains(&"linear filed it".to_string()));
    app_commands::test_hooks::clear();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn undeclared_and_ungranted_commands_reach_no_app_and_mentions_route_to_the_mentioned_bot() {
    let s = setup().await;
    let (g, r) = (&s.general, &s.random);
    s.bob.send_channel_message(g, &format!("/deploy post {g} undeclared")).await;
    s.bob.send_channel_message(r, &format!("/ticket @{} post {g} ungranted", s.linear_name)).await;
    // A mention of one bot reaches its on-mention and no one else's.
    s.bob.send_channel_message(g, &format!("post {g} paged; @{}", s.jira_name)).await;
    s.run_bots().await;
    let all = contents(&s.alice, g).await;
    assert!(!all.iter().any(|t| t == "undeclared" || t == "ungranted"), "{all:?}");
    assert_eq!(all.iter().filter(|t| *t == "paged").count(), 1, "{all:?}");
    app_commands::test_hooks::clear();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_request_command_never_appears_in_the_channel() {
    let s = setup().await;
    let g = &s.general;
    let before = contents(&s.alice, g).await;
    // The composer's path for a `request: true` command: sealed to the bot,
    // never the channel. The transport is #2008's; until then it refuses.
    let err = s
        .bob
        .invoke_try(
            "send_command_request",
            serde_json::json!({
                "groupId": s.group, "channelId": g, "botUserId": s.jira_id,
                "command": "forget-me", "args": format!("post {g} leaked"),
            }),
        )
        .await
        .expect_err("no request transport yet");
    assert!(err.starts_with(BOT_REQUESTS_UNAVAILABLE), "{err}");
    s.run_bots().await;
    assert_eq!(contents(&s.alice, g).await, before, "nothing was posted");

    // An old client that posts it anyway: it is never run as a command.
    s.bob.send_channel_message(g, &format!("/forget-me post {g} typed-request")).await;
    s.run_bots().await;
    assert!(!contents(&s.alice, g).await.contains(&"typed-request".to_string()));
    app_commands::test_hooks::clear();
}
