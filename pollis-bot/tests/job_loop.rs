//! The job loop against a fake core: lock exclusivity, handler errors, grant
//! refusals, lost claims, retries. Several "hosts" of one bot share a fake DS
//! (claims + posts), as real hosts share the Delivery Service.

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use common::{EchoHandler, FailingHandler, FakeCore, FakeDs, MixedHandler, BOT, CHANNEL};
use pollis_bot::action::Refusal;
use pollis_bot::handler::AlertsHandler;
use pollis_bot::job::{action_message_id, Installation, JobError, JobInput, TriggerOutcome};
use pollis_bot::trigger::{IncomingMessage, Trigger, WebhookCall};

fn hook(delivery: &str, channel: &str, text: &str) -> JobInput {
    JobInput::Webhook(WebhookCall { delivery_id: delivery.into(), channel: channel.into(), text: text.into() })
}

fn webhook_outcome(report: &pollis_bot::job::JobReport) -> TriggerOutcome {
    report.webhook().expect("webhook trigger").result.clone().expect("no transient error")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_job_per_installation_at_a_time() {
    let ds = Arc::new(FakeDs::default());
    let core = FakeCore::slow("01HOSTA", ds.clone());
    let inst = Arc::new(Installation::new(core.clone(), Arc::new(AlertsHandler)));
    let mut jobs = Vec::new();
    for i in 0..6 {
        let inst = inst.clone();
        jobs.push(tokio::spawn(async move { inst.run_job(hook(&format!("d{i}"), "alerts", "x")).await }));
    }
    for job in jobs {
        job.await.unwrap().unwrap();
    }
    assert_eq!(core.max_active.load(Ordering::SeqCst), 1, "two jobs of one installation overlapped");
    assert_eq!(ds.posts().len(), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn different_installations_do_not_wait_on_each_other() {
    let ds = Arc::new(FakeDs::default());
    let a = FakeCore::slow("01HOSTA", ds.clone());
    let b = FakeCore::slow("01HOSTB", ds.clone());
    let ia = Installation::new(a.clone(), Arc::new(AlertsHandler));
    let ib = Installation::new(b.clone(), Arc::new(AlertsHandler));
    let start = std::time::Instant::now();
    let (ra, rb) = tokio::join!(ia.run_job(JobInput::Wake), ib.run_job(JobInput::Wake));
    ra.unwrap();
    rb.unwrap();
    // Each catch-up sleeps 20ms; serialised they would take 40ms.
    assert!(start.elapsed() < Duration::from_millis(38), "{:?}", start.elapsed());
}

#[tokio::test]
async fn a_webhook_posts_once_through_the_claim() {
    let ds = Arc::new(FakeDs::default());
    let core = FakeCore::new("01HOSTA", ds.clone());
    let inst = Installation::new(core.clone(), Arc::new(AlertsHandler));
    let report = inst.run_job(hook("d1", "#alerts", "disk full")).await.unwrap();
    assert_eq!(webhook_outcome(&report), TriggerOutcome::Applied { posted: 1, refused: vec![] });
    let posts = ds.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].1.as_str(), posts[0].2.as_str()), (CHANNEL, "disk full"));
}

#[tokio::test]
async fn a_handler_error_discards_every_action_and_claims_nothing() {
    let ds = Arc::new(FakeDs::default());
    let core = FakeCore::new("01HOSTA", ds.clone());
    let inst = Installation::new(core.clone(), Arc::new(FailingHandler));
    let report = inst.run_job(hook("d1", "alerts", "x")).await.unwrap();
    assert_eq!(webhook_outcome(&report), TriggerOutcome::HandlerFailed { error: "boom".into() });
    assert!(ds.posts().is_empty());
    assert_eq!(core.claim_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_action_outside_the_grants_is_refused_and_never_applied() {
    let ds = Arc::new(FakeDs::default());
    let core = FakeCore::new("01HOSTA", ds.clone());

    let alerts = Installation::new(core.clone(), Arc::new(AlertsHandler));
    let report = alerts.run_job(hook("d1", "01OTHERGROUPCHANNEL", "leak")).await.unwrap();
    assert_eq!(
        webhook_outcome(&report),
        TriggerOutcome::Applied {
            posted: 0,
            refused: vec![Refusal::ChannelNotGranted { channel: "01OTHERGROUPCHANNEL".into() }],
        }
    );
    let report = alerts.run_job(hook("d2", "standup", "voice")).await.unwrap();
    assert!(matches!(webhook_outcome(&report), TriggerOutcome::Applied { posted: 0, .. }));
    assert!(ds.posts().is_empty());

    // Refusing one action does not take the granted one with it.
    let mixed = Installation::new(core.clone(), Arc::new(MixedHandler));
    let report = mixed.run_job(hook("d3", "", "")).await.unwrap();
    assert!(matches!(webhook_outcome(&report), TriggerOutcome::Applied { posted: 1, ref refused } if refused.len() == 1));
    let posts = ds.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].2, "ok");
}

#[tokio::test]
async fn two_hosts_handling_the_same_message_post_exactly_once() {
    let ds = Arc::new(FakeDs::default());
    let a = Installation::new(FakeCore::new("01HOSTA", ds.clone()), Arc::new(EchoHandler));
    let b = Installation::new(FakeCore::new("01HOSTB", ds.clone()), Arc::new(EchoHandler));
    ds.deliver("01MSG1", "01ALICE", "hi");

    let (ra, rb) = tokio::join!(a.run_job(JobInput::Wake), b.run_job(JobInput::Wake));
    let outcomes: Vec<TriggerOutcome> = [ra.unwrap(), rb.unwrap()]
        .iter()
        .flat_map(|r| r.triggers.iter().map(|t| t.result.clone().unwrap()))
        .collect();
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes.iter().filter(|o| **o == TriggerOutcome::ClaimedElsewhere).count(), 1);
    assert_eq!(ds.posts().len(), 1, "exactly one host may post");

    // Both hosts moved past the message: neither handles it again.
    for inst in [&a, &b] {
        assert!(inst.run_job(JobInput::Wake).await.unwrap().triggers.is_empty());
    }
}

#[tokio::test]
async fn the_bots_own_messages_are_not_triggers() {
    let ds = Arc::new(FakeDs::default());
    let inst = Installation::new(FakeCore::new("01HOSTA", ds.clone()), Arc::new(EchoHandler));
    ds.deliver("01MSG1", BOT, "my own post");
    let report = inst.run_job(JobInput::Wake).await.unwrap();
    assert!(report.triggers.is_empty());
    assert!(ds.posts().is_empty());
}

#[tokio::test]
async fn a_transient_claim_failure_is_retried_on_the_next_wake_without_skipping() {
    let ds = Arc::new(FakeDs::default());
    let core = FakeCore::new("01HOSTA", ds.clone());
    let inst = Installation::new(core.clone(), Arc::new(EchoHandler));
    ds.deliver("01MSG1", "01ALICE", "one");
    ds.deliver("01MSG2", "01ALICE", "two");

    core.fail_claim.store(true, Ordering::SeqCst);
    let report = inst.run_job(JobInput::Wake).await.unwrap();
    // Stopped at the first message; the second was not attempted out of order.
    assert_eq!(report.triggers.len(), 1);
    assert!(matches!(report.triggers[0].result, Err(JobError::Claim(_))));
    assert!(ds.posts().is_empty());

    core.fail_claim.store(false, Ordering::SeqCst);
    let report = inst.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(report.triggers.len(), 2);
    let texts: Vec<String> = ds.posts().into_iter().map(|p| p.2).collect();
    assert_eq!(texts, ["echo: one", "echo: two"]);
}

#[tokio::test]
async fn a_send_that_failed_after_winning_the_claim_is_retried_under_the_same_id() {
    let ds = Arc::new(FakeDs::default());
    let core = FakeCore::new("01HOSTA", ds.clone());
    let inst = Installation::new(core.clone(), Arc::new(EchoHandler));
    ds.deliver("01MSG1", "01ALICE", "hi");

    core.fail_post.store(true, Ordering::SeqCst);
    let report = inst.run_job(JobInput::Wake).await.unwrap();
    assert!(matches!(report.triggers[0].result, Err(JobError::Post(_))));

    core.fail_post.store(false, Ordering::SeqCst);
    let report = inst.run_job(JobInput::Wake).await.unwrap();
    // The winner's retry wins the claim again.
    assert_eq!(report.triggers[0].result, Ok(TriggerOutcome::Applied { posted: 1, refused: vec![] }));
    // A third run (say after a crash before the cursor moved) changes nothing.
    *core.cursor.lock().unwrap() = 0;
    inst.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(ds.posts().len(), 1);
}

#[tokio::test]
async fn wake_ups_fold_into_a_queued_job() {
    let ds = Arc::new(FakeDs::default());
    let inst = Installation::new(FakeCore::new("01HOSTA", ds), Arc::new(AlertsHandler));
    assert!(inst.queue_wake());
    assert!(!inst.queue_wake(), "a second wake-up before the job starts must not queue another");
    inst.run_job(JobInput::Wake).await.unwrap();
    assert!(inst.queue_wake(), "once the job has started catching up, a new wake-up needs a new job");
}

#[test]
fn a_triggers_message_ids_are_stable_valid_ulids() {
    let t = Trigger::Message(IncomingMessage {
        envelope_id: ulid::Ulid::new().to_string(),
        channel_id: CHANNEL.into(),
        sender_id: "u".into(),
        text: None,
        reply_to_id: None,
        thread_id: None,
        sent_at: String::new(),
    });
    let a = action_message_id(BOT, &t, 0);
    assert_eq!(a, action_message_id(BOT, &t, 0));
    assert_ne!(a, action_message_id(BOT, &t, 1));
    assert_ne!(a, action_message_id("01OTHERBOT", &t, 0));
    assert!(ulid::Ulid::from_string(&a).is_ok());
}
