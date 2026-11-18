//! The `pollis-bot` job loop over the real core and the in-process DS (#1182).
//! Every job unseals the installation at its start and seals it at its end, and
//! nothing assumes a host stayed up: hosts here are "asleep" (sealed, no
//! process state) between jobs.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serial_test::serial;

use pollis_bot::action::Action;
use pollis_bot::core_host::installation_core;
use pollis_bot::handler::{AlertsHandler, Handler, HandlerError};
use pollis_bot::job::{Installation, JobInput, TriggerOutcome};
use pollis_bot::trigger::Trigger;
use pollis_core::commands::bots;

use crate::harness::{pair_new_bot, wipe, world, writable_remote, BotHostClient, TestClient};

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

/// Put `host` to sleep (seal its harness state) and return the runtime's
/// handle on it, which unseals a fresh state for each job.
async fn asleep(host: &BotHostClient, handler: Arc<dyn Handler>) -> Installation {
    bots::stop_unattended(&host.state).await;
    let core = installation_core(
        world().await.config.clone(),
        host.keystore.clone(),
        host.dir.clone(),
        host.session().clone(),
    );
    Installation::new(core, handler)
}

async fn file_add_host(bot_user_id: &str, name: &str) -> (BotHostClient, bots::PairingHandle) {
    let host = BotHostClient::new().await;
    let handle = bots::start_add_host_pairing(&host.state, bot_user_id, Some(name.to_string()))
        .await
        .expect("start_add_host_pairing");
    (host, handle)
}

async fn set_last_seen(device_id: &str, modifier: &str) {
    let conn = writable_remote().await.conn().await.expect("remote conn");
    conn.execute(
        "UPDATE user_device SET last_seen = datetime('now', ?1) WHERE device_id = ?2",
        libsql::params![modifier.to_string(), device_id.to_string()],
    )
    .await
    .expect("set last_seen");
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
    let (mut host2, handle) = file_add_host(&bot, "host-2").await;
    alice
        .invoke_try("add_bot_host", serde_json::json!({ "botUserId": bot, "code": handle.code }))
        .await
        .expect("add_bot_host");
    host1.handle_approvals().await;
    host2.finish_pairing(&handle, Duration::from_secs(2)).await;

    let a = asleep(&host1, Arc::new(Ack)).await;
    let b = asleep(&host2, Arc::new(Ack)).await;
    // The first job seeds each host's cursor at "now": nothing earlier is a trigger.
    let seed = a.run_job(JobInput::Wake).await.expect("seed host 1");
    b.run_job(JobInput::Wake).await.expect("seed host 2");

    alice.send_channel_message(&general, "deploy started").await;
    let (ra, rb) = tokio::join!(a.run_job(JobInput::Wake), b.run_job(JobInput::Wake));
    let (ra, rb) = (ra.expect("host 1 job"), rb.expect("host 2 job"));
    let outcomes: Vec<TriggerOutcome> = [&ra, &rb]
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

    // The per-job unseal + seal cost, for the record.
    let samples = [seed.open_close, ra.open_close, rb.open_close];
    eprintln!("[bot-runtime] per-job open+close: {samples:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_only_host_asleep_wakes_on_the_nudge_and_wraps() {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let group = alice.create_group("Ops").await;

    let (host1, _) = pair_new_bot(&alice, &group, "Alerts").await;
    let bot = host1.bot_user_id().to_string();
    // Host 1 goes to sleep, and stays unseen long enough to look gone.
    let sleeper = asleep(&host1, Arc::new(AlertsHandler)).await;
    set_last_seen(host1.device_id(), "-2 hours").await;

    let (mut host2, handle) = file_add_host(&bot, "host-2").await;
    alice
        .invoke_try("add_bot_host", serde_json::json!({ "botUserId": bot, "code": handle.code }))
        .await
        .expect("add_bot_host");

    // The nudge (or a start) wakes host 1 cold: it unseals, verifies and wraps at once.
    let report = sleeper.run_job(JobInput::Approvals { wrap_now: false }).await.expect("approvals job");
    assert_eq!(report.approvals_error, None);
    assert_eq!(report.approvals_retry_at, None, "the only host never waits for anyone");
    host2.finish_pairing(&handle, Duration::from_secs(2)).await;
    assert_eq!(host2.bot_user_id(), bot);
    eprintln!("[bot-runtime] cold approvals job open+close: {:?}", report.open_close);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_asleep_peer_that_ranks_first_never_blocks_the_wrap() {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let group = alice.create_group("Ops").await;

    let (host1, _) = pair_new_bot(&alice, &group, "Alerts").await;
    let bot = host1.bot_user_id().to_string();
    let (mut host2, handle2) = file_add_host(&bot, "host-2").await;
    alice
        .invoke_try("add_bot_host", serde_json::json!({ "botUserId": bot, "code": handle2.code }))
        .await
        .expect("add_bot_host");
    host1.handle_approvals().await;
    host2.finish_pairing(&handle2, Duration::from_secs(2)).await;
    assert!(host1.device_id() < host2.device_id(), "host 1 ranks first");

    // Host 1 was seen moments ago but is asleep now; host 2 is woken.
    let _host1 = asleep(&host1, Arc::new(AlertsHandler)).await;
    let woken = asleep(&host2, Arc::new(AlertsHandler)).await;

    let (mut host3, handle3) = file_add_host(&bot, "host-3").await;
    alice
        .invoke_try("add_bot_host", serde_json::json!({ "botUserId": bot, "code": handle3.code }))
        .await
        .expect("add_bot_host");
    // Staggered, it defers by a bounded amount, never past the approval's life.
    let report = woken.run_job(JobInput::Approvals { wrap_now: false }).await.expect("approvals job");
    let retry_at = report.approvals_retry_at.expect("defers behind the first-ranked peer");
    let now = chrono::Utc::now().timestamp();
    assert!(retry_at - now <= bots::WRAP_STAGGER_SECS, "a bounded stagger: {}", retry_at - now);
    // Its retry (or a one-pass run that will not be up then) wraps without host 1.
    let report = woken.run_job(JobInput::Approvals { wrap_now: true }).await.expect("approvals job");
    assert_eq!((report.approvals_error, report.approvals_retry_at), (None, None));
    host3.finish_pairing(&handle3, Duration::from_secs(2)).await;
    assert_eq!(host3.bot_user_id(), bot);

    // Host 1 unseen for long: host 2 no longer waits for it at all.
    set_last_seen(host1.device_id(), "-2 hours").await;
    let (mut host4, handle4) = file_add_host(&bot, "host-4").await;
    alice
        .invoke_try("add_bot_host", serde_json::json!({ "botUserId": bot, "code": handle4.code }))
        .await
        .expect("add_bot_host");
    let report = woken.run_job(JobInput::Approvals { wrap_now: false }).await.expect("approvals job");
    assert_eq!((report.approvals_error, report.approvals_retry_at), (None, None), "wraps at once");
    host4.finish_pairing(&handle4, Duration::from_secs(2)).await;
    assert_eq!(host4.bot_user_id(), bot);
}
