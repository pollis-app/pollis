//! #2004: a sandboxed App as the installation's handler, through the job loop
//! (fake core; the real core is in the flows harness, `bot_runtime.rs`).

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use common::*;
use pollis_app_manifest::{ActionKind, AdminGrants, ChannelGrant, EventKind};
use pollis_bot::action::Action;
use pollis_bot::app_handler::AppHandler;
use pollis_bot::core::StateWrite;
use pollis_bot::handler::{Handler, HandlerContext, HandlerError, HandlerOutput};
use pollis_bot::job::{Installation, JobInput, TriggerOutcome, STATE_CONFLICT_ATTEMPTS};
use pollis_bot::trigger::Trigger;

const MANIFEST: &[u8] = br##"{
    "name": "counter", "version": "0.1.0",
    "triggers": ["messages"],
    "read": { "channels": ["#alerts"] },
    "actions": ["post", "reply"],
    "state": { "quota": 4096 }
}"##;

fn grants() -> AdminGrants {
    AdminGrants {
        channels: vec![ChannelGrant {
            id: CHANNEL.into(),
            name: "alerts".into(),
        }],
        events: [EventKind::Created].into(),
        actions: [ActionKind::Post, ActionKind::Reply].into(),
        ..Default::default()
    }
}

fn app() -> Arc<dyn Handler> {
    let wasm = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../pollis-app-host/fixtures/test-app.wasm"
    ))
    .unwrap();
    Arc::new(AppHandler::new(wasm, MANIFEST, grants()).unwrap())
}

fn texts(ds: &FakeDs) -> Vec<String> {
    ds.posts().into_iter().map(|(_, _, t)| t).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_runs_in_the_job_loop_and_two_hosts_share_its_state() {
    let ds = Arc::new(FakeDs::default());
    let host1 = Installation::new(FakeCore::new("host-1", ds.clone()), app());
    let host2 = Installation::new(FakeCore::new("host-2", ds.clone()), app());

    ds.deliver(
        "01MSG1",
        "alice",
        &format!("incr n; post-kv {CHANNEL} n; ack thanks"),
    );
    let r = host1.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(
        r.triggers[0].result,
        Ok(TriggerOutcome::Applied {
            posted: 2,
            refused: vec![]
        })
    );
    // Host 2 handles the same message too, and loses the claim.
    let r = host2.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(r.triggers[0].result, Ok(TriggerOutcome::ClaimedElsewhere));

    // Host 2 alone reads what host 1 wrote.
    ds.deliver("01MSG2", "alice", &format!("incr n; post-kv {CHANNEL} n"));
    host2.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(
        texts(&ds),
        vec![
            "kv n=1".to_string(),
            "reply to 01MSG1: thanks".into(),
            "kv n=2".into()
        ]
    );
    assert_eq!(ds.state.lock().unwrap().get("n").map(|(v, _)| *v), Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_outside_the_apps_grants_runs_nothing() {
    let ds = Arc::new(FakeDs::default());
    let host = Installation::new(FakeCore::new("host-1", ds.clone()), app());
    // The bot's own message is never a trigger; an ungranted channel is not delivered.
    ds.log
        .lock()
        .unwrap()
        .push(pollis_bot::trigger::IncomingMessage {
            envelope_id: "01ELSEWHERE".into(),
            channel_id: "01VOICE".into(),
            sender_id: "alice".into(),
            text: Some(format!("post {CHANNEL} leak")),
            reply_to_id: None,
            thread_id: None,
            sent_at: String::new(),
        });
    let r = host.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(r.triggers[0].result, Ok(TriggerOutcome::NoActions));
    assert!(ds.posts().is_empty());
    assert!(ds.claims.lock().unwrap().is_empty(), "nothing to claim");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trapped_app_applies_nothing_and_writes_no_state() {
    let ds = Arc::new(FakeDs::default());
    let host = Installation::new(FakeCore::new("host-1", ds.clone()), app());
    ds.deliver(
        "01MSG1",
        "alice",
        &format!("incr n; post {CHANNEL} queued; trap"),
    );
    let r = host.run_job(JobInput::Wake).await.unwrap();
    assert!(
        matches!(
            r.triggers[0].result,
            Ok(TriggerOutcome::HandlerFailed { .. })
        ),
        "{:?}",
        r.triggers
    );
    assert!(ds.posts().is_empty());
    assert!(ds.state.lock().unwrap().is_empty());
}

/// Reads `n` like an App would, then (first run only) lets another host move it
/// before the commit.
struct Racy {
    ds: Arc<FakeDs>,
    runs: AtomicUsize,
}

#[async_trait]
impl Handler for Racy {
    fn kind(&self) -> &'static str {
        "racy"
    }

    async fn handle(&self, _: &Trigger) -> Result<Vec<Action>, HandlerError> {
        unreachable!()
    }

    async fn handle_job(
        &self,
        _: &Trigger,
        ctx: &HandlerContext,
    ) -> Result<HandlerOutput, HandlerError> {
        let run = self.runs.fetch_add(1, Ordering::SeqCst);
        let (read, _) = ctx
            .core
            .state_read("n")
            .await
            .map_err(|e| HandlerError(e.0))?;
        if run == 0 {
            self.ds
                .state
                .lock()
                .unwrap()
                .insert("n".into(), (read.version + 1, b"moved".to_vec()));
        }
        Ok(HandlerOutput {
            actions: vec![Action::Post {
                channel: CHANNEL.into(),
                text: format!("run {run}"),
            }],
            state: vec![StateWrite {
                key: "n".into(),
                expected_version: read.version,
                value: Some(b"mine".to_vec()),
            }],
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_state_conflict_reruns_the_handler_on_the_new_state() {
    let ds = Arc::new(FakeDs::default());
    let racy = Arc::new(Racy {
        ds: ds.clone(),
        runs: AtomicUsize::new(0),
    });
    let host = Installation::new(FakeCore::new("host-1", ds.clone()), racy.clone());
    ds.deliver("01MSG1", "alice", "go");
    let r = host.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(
        r.triggers[0].result,
        Ok(TriggerOutcome::Applied {
            posted: 1,
            refused: vec![]
        })
    );
    assert_eq!(racy.runs.load(Ordering::SeqCst), 2);
    // Only the second run's action was applied, on top of the moved value.
    assert_eq!(texts(&ds), vec!["run 1".to_string()]);
    assert_eq!(
        ds.state.lock().unwrap().get("n").cloned(),
        Some((2, b"mine".to_vec()))
    );
    const { assert!(STATE_CONFLICT_ATTEMPTS >= 2) };
}

fn roster(ds: &FakeDs) {
    *ds.members.lock().unwrap() = vec![
        pollis_bot::core::Member { user_id: "alice".into(), username: Some("alice".into()), is_bot: false },
        pollis_bot::core::Member { user_id: "helper".into(), username: Some("helper_bot".into()), is_bot: true },
        pollis_bot::core::Member { user_id: BOT.into(), username: Some("counter_bot".into()), is_bot: true },
    ];
}

#[tokio::test(flavor = "multi_thread")]
async fn the_app_sees_authors_from_the_roster() {
    let ds = Arc::new(FakeDs::default());
    roster(&ds);
    let host = Installation::new(FakeCore::new("host-1", ds.clone()), app());
    ds.deliver("01MSG1", "alice", &format!("post-author {CHANNEL}"));
    ds.deliver("01MSG2", "helper", &format!("post-author {CHANNEL}"));
    ds.deliver("01MSG3", "gone", &format!("post-author {CHANNEL}"));
    host.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(
        texts(&ds),
        vec!["author alice bot=false".to_string(), "author helper_bot bot=true".into(), "author gone bot=false".into()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mention_of_the_bot_reaches_on_mention_only_when_the_app_takes_mentions() {
    let mentions_only = br##"{
        "name": "pager", "version": "0.1.0",
        "triggers": ["mentions"],
        "read": { "channels": ["#alerts"] },
        "actions": ["post"]
    }"##;
    let wasm = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../pollis-app-host/fixtures/test-app.wasm")).unwrap();
    let mut post_only = grants();
    post_only.actions = [ActionKind::Post].into();
    let handler: Arc<dyn Handler> = Arc::new(AppHandler::new(wasm, mentions_only, post_only).unwrap());
    let ds = Arc::new(FakeDs::default());
    roster(&ds);
    let host = Installation::new(FakeCore::new("host-1", ds.clone()), handler);
    ds.deliver("01MSG1", "alice", &format!("post {CHANNEL} plain"));
    ds.deliver("01MSG2", "alice", &format!("post {CHANNEL} everyone; @all"));
    ds.deliver("01MSG3", "alice", &format!("post {CHANNEL} paged; @Counter_bot"));
    let r = host.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(r.triggers[0].result, Ok(TriggerOutcome::NoActions), "a plain message is not a mention");
    assert_eq!(r.triggers[1].result, Ok(TriggerOutcome::NoActions), "@all is not a mention of the bot");
    assert_eq!(texts(&ds), vec!["paged".to_string()]);
}
