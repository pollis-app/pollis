//! The subcommands over a fake host: pairing saves an installation and honours
//! the limit, `run` serves the webhook, and `post` goes through it.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{alerts_approval, FakeCore, FakeDs, MemorySlot, ALERTS_APP_DIR, BOT, CHANNEL};
use pollis_bot::app;
use pollis_bot::app_approval::ArtifactLocation;
use pollis_bot::config::{RuntimeConfig, WebhookConfig};
use pollis_bot::host::{BotHost, OpenedInstallation, PairOutcome, PairTarget, PairingProgress, PairingTicket, Wakes};
use pollis_bot::installations::{InstallationRecord, InstalledApp, Registry};
use pollis_bot::secrets::SecretSlot;

struct FakeHost {
    ds: Arc<FakeDs>,
    core: Arc<FakeCore>,
    secrets: Arc<MemorySlot>,
    next: Mutex<Option<PairOutcome>>,
    pair_calls: AtomicUsize,
    codes: Mutex<Vec<String>>,
    opened: Mutex<Vec<String>>,
}

impl FakeHost {
    fn new(next: Option<PairOutcome>) -> Self {
        let ds = Arc::new(FakeDs::default());
        Self {
            core: FakeCore::new("01HOSTA", ds.clone()),
            ds,
            secrets: Arc::new(MemorySlot::default()),
            next: Mutex::new(next),
            pair_calls: AtomicUsize::new(0),
            codes: Mutex::new(Vec::new()),
            opened: Mutex::new(Vec::new()),
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

    async fn open(&self, record: &InstallationRecord, _dir: &Path) -> anyhow::Result<OpenedInstallation> {
        self.opened.lock().unwrap().push(record.installation_id.clone());
        Ok(OpenedInstallation { core: self.core.clone(), secrets: self.secrets.clone() })
    }

    async fn watch(&self, _record: &InstallationRecord, _dir: &Path, _wakes: Wakes) -> anyhow::Result<()> {
        Ok(())
    }

    async fn forget(&self, record: &InstallationRecord, _dir: &Path) -> anyhow::Result<()> {
        self.secrets.0.lock().unwrap().remove(&record.installation_id);
        Ok(())
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
        app: None,
        created_at: "2026-11-18T00:00:00Z".into(),
    }
}

/// An installation that has been given the alerts App.
fn with_app(id: &str) -> InstallationRecord {
    InstallationRecord {
        app: Some(InstalledApp {
            component: ArtifactLocation(format!("{ALERTS_APP_DIR}/app.wasm")),
            manifest: ArtifactLocation(format!("{ALERTS_APP_DIR}/pollis-app.json")),
        }),
        ..record(id)
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
    let err = app::run(&host, dir.path(), &RuntimeConfig::default(), None, false).await.unwrap_err();
    assert!(err.to_string().contains("pollis-bot pair"), "{err}");

    let registry = Registry::new(dir.path());
    registry.save(&record("01A")).unwrap();
    registry.save(&record("01B")).unwrap();
    let config = RuntimeConfig { max_installations: 1, webhook: None };
    let err = app::run(&host, dir.path(), &config, None, false).await.unwrap_err();
    assert!(err.to_string().contains("max_installations"), "{err}");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_serves_the_webhook_and_post_goes_through_the_approved_alerts_app() {
    let dir = tempfile::tempdir().unwrap();
    Registry::new(dir.path()).save(&with_app(BOT)).unwrap();
    let host = Arc::new(FakeHost::new(None));
    *host.core.approval.lock().unwrap() = Some(alerts_approval());
    let webhook = WebhookConfig { bind: format!("127.0.0.1:{}", free_port()).parse().unwrap(), ..Default::default() };
    let config = RuntimeConfig { webhook: Some(webhook.clone()), ..Default::default() };

    let runner = {
        let (host, dir, config) = (host.clone(), dir.path().to_path_buf(), config.clone());
        tokio::spawn(async move { app::run(host.as_ref(), &dir, &config, Some(webhook), false).await })
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

    // A channel the admin did not grant is refused, and `post` says why.
    let err = app::post(host.as_ref(), dir.path(), &config, BOT, "01ELSEWHERE", "leak").await.unwrap_err();
    assert!(err.to_string().contains("not in `read.channels`"), "{err}");
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

#[tokio::test]
async fn forget_removes_the_installation_and_its_keys() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::new(dir.path());
    registry.save(&record(BOT)).unwrap();
    registry.save(&record("01OTHER")).unwrap();
    let host = FakeHost::new(None);
    app::rotate_secret(&host, dir.path(), BOT).await.unwrap();

    app::forget(&host, dir.path(), BOT).await.unwrap();
    let left: Vec<_> = registry.list().unwrap().into_iter().map(|r| r.installation_id).collect();
    assert_eq!(left, ["01OTHER"]);
    assert!(host.secrets.load(BOT).await.unwrap().is_none());
    assert!(app::forget(&host, dir.path(), BOT).await.is_err(), "a second forget names the missing id");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_one_pass_run_catches_up_wraps_at_once_and_exits_sealed() {
    let dir = tempfile::tempdir().unwrap();
    Registry::new(dir.path()).save(&with_app(BOT)).unwrap();
    let host = FakeHost::new(None);
    *host.core.approval.lock().unwrap() = Some(alerts_approval());
    // A peer ranks first: a long-running host would defer, a one-pass host must not.
    *host.core.approvals_retry_at.lock().unwrap() = Some(i64::MAX);
    app::run(&host, dir.path(), &RuntimeConfig::default(), None, true).await.unwrap();
    assert_eq!(*host.core.approval_calls.lock().unwrap(), vec![true], "one pass wraps now");
    assert_eq!(host.core.opens.load(Ordering::SeqCst), host.core.closes.load(Ordering::SeqCst));
    assert!(!host.core.open.load(Ordering::SeqCst), "the store is sealed when the process exits");
}

// ---------------------------------------------------------------- no App yet (#2007)

#[tokio::test]
async fn an_installation_without_an_app_does_not_run_and_says_what_to_do() {
    let dir = tempfile::tempdir().unwrap();
    Registry::new(dir.path()).save(&record(BOT)).unwrap();
    let host = FakeHost::new(None);
    let config = RuntimeConfig::default();

    let err = app::run(&host, dir.path(), &config, None, true).await.unwrap_err();
    assert!(err.to_string().contains("pollis-bot app install"), "{err}");
    assert!(host.opened.lock().unwrap().is_empty(), "never unsealed");
    assert_eq!(host.core.opens.load(Ordering::SeqCst), 0);

    let status = app::status(dir.path(), &config).unwrap();
    assert!(status.contains("no App yet: not running") && status.contains("pollis-bot app install"), "{status}");

    let err = app::post(&host, dir.path(), &config, BOT, "alerts", "hi").await.unwrap_err();
    assert!(err.to_string().contains("no App yet"), "{err}");
    assert!(host.ds.posts().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_installation_without_an_app_does_not_stop_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::new(dir.path());
    registry.save(&with_app(BOT)).unwrap();
    registry.save(&record("01WAITING")).unwrap();
    let host = FakeHost::new(None);
    *host.core.approval.lock().unwrap() = Some(alerts_approval());
    app::run(&host, dir.path(), &RuntimeConfig::default(), None, true).await.unwrap();
    assert_eq!(*host.opened.lock().unwrap(), vec![BOT.to_string()]);
}

#[test]
fn a_record_written_without_an_app_reads_as_having_none() {
    let json = r#"{"installation_id":"01A","bot_user_id":"01A","group_id":"G","device_id":"D","created_at":"t"}"#;
    let record: InstallationRecord = serde_json::from_str(json).unwrap();
    assert_eq!(record.app, None);
}

// ---------------------------------------------------------------- app install (#2004, #2005)

#[tokio::test]
async fn app_install_points_the_installation_at_a_checked_app_and_prints_its_hash() {
    let data = tempfile::tempdir().unwrap();
    let registry = Registry::new(data.path());
    registry
        .save(&InstallationRecord {
            installation_id: "01BOT".into(),
            bot_user_id: "01BOT".into(),
            group_id: "01GROUP".into(),
            device_id: "01DEVICE".into(),
            app: None,
            created_at: "2026-11-18T00:00:00Z".into(),
        })
        .unwrap();
    let artifact = tempfile::tempdir().unwrap();
    let wasm = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../pollis-app-host/fixtures/test-app.wasm")).unwrap();
    std::fs::write(artifact.path().join("pager.wasm"), &wasm).unwrap();
    let manifest = br##"{"name":"pager","version":"0.1.0","triggers":["webhook"],"read":{"channels":["*"]},"actions":["post"]}"##;
    std::fs::write(artifact.path().join("pollis-app.json"), manifest).unwrap();
    let dir = artifact.path().display().to_string();

    let (record, hash) = pollis_bot::app_cli::app_install(data.path(), "01BOT", &dir).await.unwrap();
    assert_eq!(hash, pollis_app_manifest::hex(&pollis_app_manifest::app_hash(&wasm, manifest).unwrap()));
    let installed = record.app.as_ref().expect("the installation has its App");
    let component = &installed.component;
    // Not copied: the runtime reads it where it is, and checks it against the approval each job.
    assert!(component.0.starts_with(&artifact.path().canonicalize().unwrap().display().to_string()));
    assert_eq!(registry.get("01BOT").unwrap().unwrap().app, record.app);

    // A second component is ambiguous; nothing changes.
    std::fs::write(artifact.path().join("other.wasm"), b"x").unwrap();
    let err = pollis_bot::app_cli::app_install(data.path(), "01BOT", &dir).await.unwrap_err();
    assert!(err.to_string().contains("exactly one .wasm"), "{err}");
    // An unknown installation is refused.
    assert!(pollis_bot::app_cli::app_install(data.path(), "01NOPE", &dir).await.is_err());
}
