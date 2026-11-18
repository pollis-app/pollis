//! The `pollis-bot` job loop over the real core and the in-process DS (#1182):
//! two hosts of one bot see the same message, both run a job, and exactly one
//! reply is posted.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serial_test::serial;

use pollis_bot::action::Action;
use pollis_bot::core_host::installation_core;
use pollis_bot::handler::{Handler, HandlerError};
use pollis_bot::job::{Installation, JobInput, TriggerOutcome};
use pollis_bot::trigger::Trigger;
use pollis_core::commands::bots;

use crate::harness::{pair_new_bot, wipe, BotHostClient, TestClient};

/// Replies to every message: an action every host derives from the same trigger.
struct Ack;

#[async_trait]
impl Handler for Ack {
    fn kind(&self) -> &'static str {
        "ack"
    }

    async fn handle(&self, trigger: &Trigger) -> Result<Vec<Action>, HandlerError> {
        match trigger {
            Trigger::Message(m) => Ok(vec![Action::Post {
                channel: m.channel_id.clone(),
                text: format!("ack: {}", m.text.clone().unwrap_or_default()),
            }]),
            Trigger::Webhook(_) => Ok(Vec::new()),
        }
    }
}

fn installation(host: &BotHostClient) -> Installation {
    let core = installation_core(host.state.clone(), host.session().clone(), &host.dir);
    Installation::new(core, Arc::new(Ack))
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn two_hosts_handling_the_same_message_post_exactly_once() {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let group = alice.create_group("Ops").await;
    let general = alice.general_channel_id(&group).await;

    let (host1, _) = pair_new_bot(&alice, &group, "Ack").await;
    let bot = host1.bot_user_id().to_string();
    let mut host2 = BotHostClient::new().await;
    let handle = bots::start_add_host_pairing(&host2.state, &bot, Some("host-2".into()))
        .await
        .expect("start_add_host_pairing");
    alice
        .invoke_try("add_bot_host", serde_json::json!({ "botUserId": bot, "code": handle.code }))
        .await
        .expect("add_bot_host");
    host1.handle_approvals().await;
    host2.finish_pairing(&handle, Duration::from_secs(2)).await;

    let a = installation(&host1);
    let b = installation(&host2);
    // The first job seeds each host's cursor at "now": nothing earlier is a trigger.
    a.run_job(JobInput::Wake).await.expect("seed host 1");
    b.run_job(JobInput::Wake).await.expect("seed host 2");

    alice.send_channel_message(&general, "deploy started").await;
    let (ra, rb) = tokio::join!(a.run_job(JobInput::Wake), b.run_job(JobInput::Wake));
    let outcomes: Vec<TriggerOutcome> = [ra.expect("host 1 job"), rb.expect("host 2 job")]
        .iter()
        .flat_map(|r| r.triggers.iter().map(|t| t.result.clone().expect("no transient failure")))
        .collect();
    assert_eq!(outcomes.len(), 2, "each host saw the message once: {outcomes:?}");
    assert_eq!(outcomes.iter().filter(|o| **o == TriggerOutcome::ClaimedElsewhere).count(), 1, "{outcomes:?}");

    let acks: Vec<String> = alice
        .fetch_channel_messages(&general)
        .await
        .into_iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .filter(|t| t.starts_with("ack: "))
        .collect();
    assert_eq!(acks, vec!["ack: deploy started".to_string()], "exactly one host posts");
}
