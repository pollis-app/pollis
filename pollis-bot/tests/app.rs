//! The subcommands over a fake host: pairing saves an installation and honours
//! the limit, `run` serves the webhook, and `post` goes through it.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{FakeCore, FakeDs, MemorySlot, BOT, CHANNEL};
use pollis_bot::app;
use pollis_bot::config::{RuntimeConfig, WebhookConfig};
use pollis_bot::handler::HandlerKind;
use pollis_bot::host::{BotHost, OpenedInstallation, PairOutcome, PairTarget, PairingProgress, PairingTicket, WakeFn};
use pollis_bot::installations::{InstallationRecord, Registry};
use pollis_bot::secrets::SecretSlot;

struct FakeHost {
    ds: Arc<FakeDs>,
    secrets: Arc<MemorySlot>,
    next: Mutex<Option<PairOutcome>>,
    pair_calls: AtomicUsize,
    codes: Mutex<Vec<String>>,
}

impl FakeHost {
    fn new(next: Option<PairOutcome>) -> Self {
        Self {
            ds: Arc::new(FakeDs::default()),
            secrets: Arc::new(MemorySlot::default()),
            next: Mutex::new(next),
            pair_calls: AtomicUsize::new(0),
            codes: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl BotHost for FakeHost {
    async fn pair(
        &self,
        _data_dir: &Path,
        _target: PairTarget,
        _device_name: Option<String>,
        progress: &(dyn Fn(PairingProgress) + Send + Sync),
    ) -> anyhow::Result<PairOutcome> {
        self.pair_calls.fetch_add(1, Ordering::SeqCst);
        let ticket = PairingTicket { code: "K7QP-2XMD".into(), expires_at: 1_900_000_000 };
        self.codes.lock().unwrap().push(ticket.code.clone());
        progress(PairingProgress::WaitingForApproval(ticket));
        progress(PairingProgress::JoiningGroup);
        Ok(self.next.lock().unwrap().take().unwrap_or(PairOutcome::Expired))
    }

    async fn open(&self, _record: &InstallationRecord, _dir: &Path, _wake: WakeFn) -> anyhow::Result<OpenedInstallation> {
        Ok(OpenedInstallation { core: FakeCore::new("01HOSTA", self.ds.clone()), secrets: self.secrets.clone() })
    }

    async fn secrets(&self, _record: &InstallationRecord, _dir: &Path) -> anyhow::Result<Arc<dyn SecretSlot>> {
        Ok(self.secrets.clone())
    }
}

fn record(id: &str) -> InstallationRecord {
    InstallationRecord {
        installation_id: id.into(),
        bot_user_id: id.into(),
        group_id: "01GROUP".into(),
        device_id: "01HOSTA".into(),
        handler: HandlerKind::Alerts,
        created_at: "2026-11-18T00:00:00Z".into(),
    }
}

#[tokio::test]
async fn pairing_shows_the_code_and_saves_the_installation() {
    let dir = tempfile::tempdir().unwrap();
    let host = FakeHost::new(Some(PairOutcome::Paired(record(BOT))));
    let saved = app::pair(&host, dir.path(), &RuntimeConfig::default(), PairTarget::NewBot, None).await.unwrap();
    assert_eq!(saved, record(BOT));
    assert_eq!(Registry::new(dir.path()).list().unwrap(), vec![record(BOT)]);
    assert_eq!(host.codes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_expired_code_saves_nothing_and_says_to_pair_again() {
    let dir = tempfile::tempdir().unwrap();
    let host = FakeHost::new(None);
    let err = app::pair(&host, dir.path(), &RuntimeConfig::default(), PairTarget::NewBot, None).await.unwrap_err();
    assert!(err.to_string().contains("pollis-bot pair"), "{err}");
    assert!(Registry::new(dir.path()).list().unwrap().is_empty());
}

#[tokio::test]
async fn pairing_past_the_installation_limit_is_refused_before_any_request() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::new(dir.path());
    registry.save(&record("01A")).unwrap();
    registry.save(&record("01B")).unwrap();
    let config = RuntimeConfig { max_installations: 2, webhook: None };
    let host = FakeHost::new(Some(PairOutcome::Paired(record("01C"))));
    let err = app::pair(&host, dir.path(), &config, PairTarget::NewBot, None).await.unwrap_err();
    assert!(err.to_string().contains("max_installations"), "{err}");
    assert_eq!(host.pair_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn adding_a_host_for_a_bot_this_host_already_runs_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    Registry::new(dir.path()).save(&record(BOT)).unwrap();
    let host = FakeHost::new(None);
    let target = PairTarget::ExistingBot { bot_user_id: BOT.into() };
    assert!(app::pair(&host, dir.path(), &RuntimeConfig::default(), target, None).await.is_err());
    assert_eq!(host.pair_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn run_refuses_no_installations_and_too_many() {
    let dir = tempfile::tempdir().unwrap();
    let host = FakeHost::new(None);
    let err = app::run(&host, dir.path(), &RuntimeConfig::default(), None).await.unwrap_err();
    assert!(err.to_string().contains("pollis-bot pair"), "{err}");

    let registry = Registry::new(dir.path());
    registry.save(&record("01A")).unwrap();
    registry.save(&record("01B")).unwrap();
    let config = RuntimeConfig { max_installations: 1, webhook: None };
    let err = app::run(&host, dir.path(), &config, None).await.unwrap_err();
    assert!(err.to_string().contains("max_installations"), "{err}");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_serves_the_webhook_and_post_goes_through_it() {
    let dir = tempfile::tempdir().unwrap();
    Registry::new(dir.path()).save(&record(BOT)).unwrap();
    let host = Arc::new(FakeHost::new(None));
    let webhook = WebhookConfig { bind: format!("127.0.0.1:{}", free_port()).parse().unwrap(), ..Default::default() };
    let config = RuntimeConfig { webhook: Some(webhook.clone()), ..Default::default() };

    let runner = {
        let (host, dir, config) = (host.clone(), dir.path().to_path_buf(), config.clone());
        tokio::spawn(async move { app::run(host.as_ref(), &dir, &config, Some(webhook)).await })
    };

    // The secret is created on the first start; wait for the listener.
    let mut answer = None;
    for _ in 0..100 {
        if host.secrets.load(BOT).await.unwrap().is_some() {
            if let Ok(a) = app::post(host.as_ref(), dir.path(), &config, BOT, "alerts", "deploy finished").await {
                answer = Some(a);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let answer = answer.expect("post never reached the runtime");
    assert_eq!(answer["status"], "posted", "{answer}");
    let posts = host.ds.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].1.as_str(), posts[0].2.as_str()), (CHANNEL, "deploy finished"));

    // A channel outside the group is refused, and `post` says so.
    let err = app::post(host.as_ref(), dir.path(), &config, BOT, "01ELSEWHERE", "leak").await.unwrap_err();
    assert!(err.to_string().contains("422"), "{err}");
    assert_eq!(host.ds.posts().len(), 1);

    let status = app::status(dir.path(), &config).unwrap();
    assert!(status.contains(BOT) && status.contains("connected"), "{status}");

    runner.abort();
}

#[tokio::test]
async fn rotating_replaces_the_secret() {
    let dir = tempfile::tempdir().unwrap();
    Registry::new(dir.path()).save(&record(BOT)).unwrap();
    let host = FakeHost::new(None);
    let first = app::rotate_secret(&host, dir.path(), BOT).await.unwrap();
    let second = app::rotate_secret(&host, dir.path(), BOT).await.unwrap();
    assert_ne!(first, second);
    assert_eq!(host.secrets.load(BOT).await.unwrap().unwrap().as_slice(), second.as_bytes());
    assert!(app::rotate_secret(&host, dir.path(), "01UNKNOWN").await.is_err());
}
