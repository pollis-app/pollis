//! #2005: an installation's App runs only under a verified approval, re-checked
//! before every job, from an artifact that matches it exactly. The fake core
//! answers `app_approval` with what the real core's verification would (that
//! verification and each of its refusals are in the flows suite,
//! `src-tauri/tests/flows/bot_apps.rs`); this file pins what the runtime does
//! with each answer.

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::*;
use pollis_api::bot_apps::BotAppRunState;
use pollis_app_manifest::{ActionKind, AdminGrants, ChannelGrant, EventKind, Manifest};
use pollis_bot::action::{ChannelInfo, CheckedAction};
use pollis_bot::app_approval::{read_location, ArtifactLocation};
use pollis_bot::app_handler::AppHandler;
use pollis_bot::core::{CoreError, InstallationCore, Member, StateCommit, StateValue, StateWrite};
use pollis_bot::job::{Installation, JobError, JobInput, TriggerOutcome};
use pollis_bot::trigger::{IncomingMessage, TriggerKey};
use pollis_core::commands::bots::app_approval::{
    sign_dev_build, developer_key_public, AppApproval, AppApprovalRecord, AppApprovalRefusal, DevApproval,
    VerifiedAppApproval, APP_APPROVAL_VERSION,
};
use pollis_core::commands::bots::apps::AppRunReport;

const MANIFEST: &str = r##"{
    "name": "counter", "version": "0.1.0",
    "triggers": ["messages"],
    "read": { "channels": ["#alerts"] },
    "actions": ["post", "reply"],
    "state": { "quota": 4096 }
}"##;

fn wasm() -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../pollis-app-host/fixtures/test-app.wasm")).unwrap()
}

fn grants() -> AdminGrants {
    AdminGrants {
        channels: vec![ChannelGrant { id: CHANNEL.into(), name: "alerts".into() }],
        events: BTreeSet::from([EventKind::Created]),
        actions: BTreeSet::from([ActionKind::Post, ActionKind::Reply]),
        ..Default::default()
    }
}

fn hex_app_hash(component: &[u8], manifest: &str) -> String {
    pollis_app_manifest::hex(&pollis_app_manifest::app_hash(component, manifest.as_bytes()).unwrap())
}

fn production(app_hash: String) -> VerifiedAppApproval {
    let record = AppApprovalRecord::Production(AppApproval {
        app_hash,
        approver_device_id: "ADMIN-D".into(),
        approver_user_id: "ADMIN".into(),
        auto_update: false,
        bot_user_id: BOT.into(),
        grants: grants(),
        group_id: "GROUP".into(),
        installation_id: BOT.into(),
        issued_at: 1_700_000_000,
        manifest_hash: pollis_app_manifest::hex(&pollis_app_manifest::manifest_hash(MANIFEST.as_bytes()).unwrap()),
        seq: 1,
        source: None,
        v: APP_APPROVAL_VERSION,
    });
    VerifiedAppApproval { record, manifest_json: MANIFEST.into(), manifest: Manifest::from_json(MANIFEST.as_bytes()).unwrap() }
}

/// The fake core, answering `app_approval` with a set verdict and recording reports.
struct Gated {
    inner: Arc<FakeCore>,
    verdict: Mutex<Result<Result<VerifiedAppApproval, AppApprovalRefusal>, CoreError>>,
    reports: Mutex<Vec<AppRunReport>>,
}

impl Gated {
    fn new(ds: Arc<FakeDs>) -> Arc<Gated> {
        Arc::new(Gated {
            inner: FakeCore::new("host-1", ds),
            verdict: Mutex::new(Ok(Err(AppApprovalRefusal::NotApproved))),
            reports: Mutex::new(Vec::new()),
        })
    }

    fn set(&self, v: Result<Result<VerifiedAppApproval, AppApprovalRefusal>, CoreError>) {
        *self.verdict.lock().unwrap() = v;
    }

    fn last_report(&self) -> AppRunReport {
        self.reports.lock().unwrap().last().cloned().expect("a report")
    }
}

#[async_trait]
impl InstallationCore for Gated {
    fn installation_id(&self) -> &str {
        self.inner.installation_id()
    }
    fn bot_user_id(&self) -> &str {
        self.inner.bot_user_id()
    }
    async fn open(&self) -> Result<(), CoreError> {
        self.inner.open().await
    }
    async fn close(&self) {
        self.inner.close().await
    }
    async fn host_approvals(&self, wrap_now: bool) -> Result<Option<i64>, CoreError> {
        self.inner.host_approvals(wrap_now).await
    }
    async fn catch_up(&self) -> Result<Vec<IncomingMessage>, CoreError> {
        self.inner.catch_up().await
    }
    async fn mark_handled(&self, message: &IncomingMessage) -> Result<(), CoreError> {
        self.inner.mark_handled(message).await
    }
    async fn channels(&self) -> Result<Vec<ChannelInfo>, CoreError> {
        self.inner.channels().await
    }
    async fn claim(&self, key: &TriggerKey) -> Result<bool, CoreError> {
        self.inner.claim(key).await
    }
    async fn post(&self, channel_id: &str, text: &str, message_id: &str) -> Result<(), CoreError> {
        self.inner.post(channel_id, text, message_id).await
    }
    async fn act(&self, action: &CheckedAction, message_id: &str) -> Result<(), CoreError> {
        self.inner.act(action, message_id).await
    }
    async fn state_read(&self, key: &str) -> Result<(StateValue, u64), CoreError> {
        self.inner.state_read(key).await
    }
    async fn state_commit(&self, key: &TriggerKey, writes: &[StateWrite]) -> Result<StateCommit, CoreError> {
        self.inner.state_commit(key, writes).await
    }
    async fn members(&self) -> Result<Vec<Member>, CoreError> {
        self.inner.members().await
    }
    async fn app_approval(&self) -> Result<Result<VerifiedAppApproval, AppApprovalRefusal>, CoreError> {
        self.verdict.lock().unwrap().clone()
    }
    async fn app_report(&self, report: &AppRunReport) -> Result<(), CoreError> {
        self.reports.lock().unwrap().push(report.clone());
        Ok(())
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    component: std::path::PathBuf,
    ds: Arc<FakeDs>,
    core: Arc<Gated>,
    inst: Installation,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let component = dir.path().join("app.wasm");
    let manifest = dir.path().join("pollis-app.json");
    std::fs::write(&component, wasm()).unwrap();
    std::fs::write(&manifest, MANIFEST).unwrap();
    let ds = Arc::new(FakeDs::default());
    let core = Gated::new(ds.clone());
    let handler = Arc::new(AppHandler::approved(component.clone().into(), manifest.into()));
    let inst = Installation::new(core.clone(), handler);
    Setup { _dir: dir, component, ds, core, inst }
}

fn posts(ds: &FakeDs) -> usize {
    ds.posts().len()
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_approval_nothing_runs_and_admins_see_the_artifact_to_approve() {
    let s = setup();
    s.ds.deliver("01MSG1", "alice", "ack thanks");
    let r = s.inst.run_job(JobInput::Wake).await.unwrap();
    assert!(matches!(&r.triggers[0].result, Ok(TriggerOutcome::HandlerFailed { error }) if error.contains("not running the App")));
    assert_eq!(posts(&s.ds), 0);
    let report = s.core.last_report();
    assert_eq!(report.state, BotAppRunState::AwaitingApproval);
    assert_eq!(report.reason, "not_approved");
    assert_eq!(report.app_hash, Some(hex_app_hash(&wasm(), MANIFEST)), "the hash an admin approves");
    assert_eq!(report.manifest.as_deref(), Some(MANIFEST));
    assert_eq!(report.source, Some(s.component.display().to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_exact_approved_artifact_runs_with_the_approved_grants() {
    let s = setup();
    s.core.set(Ok(Ok(production(hex_app_hash(&wasm(), MANIFEST)))));
    s.ds.deliver("01MSG1", "alice", "ack thanks");
    let r = s.inst.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(r.triggers[0].result, Ok(TriggerOutcome::Applied { posted: 1, refused: vec![] }));
    assert_eq!(s.core.last_report().state, BotAppRunState::Running);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_artifact_stops_running_until_it_is_approved() {
    let s = setup();
    s.core.set(Ok(Ok(production(hex_app_hash(&wasm(), MANIFEST)))));
    // The file at the location changes after approval (an update, or a swap).
    let mut other = wasm();
    other.extend_from_slice(b"\0");
    std::fs::write(&s.component, &other).unwrap();
    s.ds.deliver("01MSG1", "alice", "ack thanks");
    let r = s.inst.run_job(JobInput::Wake).await.unwrap();
    assert!(matches!(&r.triggers[0].result, Ok(TriggerOutcome::HandlerFailed { .. })));
    assert_eq!(posts(&s.ds), 0, "a hash mismatch is never close enough");
    let report = s.core.last_report();
    assert_eq!((report.state, report.reason.as_str()), (BotAppRunState::AwaitingApproval, "hash_mismatch"));
    assert_eq!(report.app_hash, Some(hex_app_hash(&other, MANIFEST)));
}

fn auto_updating(source: &str) -> VerifiedAppApproval {
    let mut v = production(hex_app_hash(&wasm(), MANIFEST));
    if let AppApprovalRecord::Production(a) = &mut v.record {
        a.auto_update = true;
        a.source = Some(source.to_string());
    }
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_widening_update_from_the_pinned_source_runs_with_no_admin_online() {
    let s = setup();
    let manifest_path = s.component.with_file_name("pollis-app.json");
    s.core.set(Ok(Ok(auto_updating(&s.component.display().to_string()))));
    // A new version at the pinned location that asks for less (a smaller quota).
    let narrower = MANIFEST.replace("4096", "2048");
    std::fs::write(&manifest_path, &narrower).unwrap();
    s.ds.deliver("01MSG1", "alice", "ack thanks");
    let r = s.inst.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(r.triggers[0].result, Ok(TriggerOutcome::Applied { posted: 1, refused: vec![] }));
    let report = s.core.last_report();
    assert_eq!((report.state, report.reason.as_str()), (BotAppRunState::Running, "auto_updated"));
    assert_eq!(report.app_hash, Some(hex_app_hash(&wasm(), &narrower)), "admins see what runs");

    // A widening version at the same location waits for an admin.
    let wider = MANIFEST.replace("\"actions\": [\"post\", \"reply\"]", "\"actions\": [\"post\", \"reply\", \"react\"]");
    assert_ne!(wider, MANIFEST);
    std::fs::write(&manifest_path, &wider).unwrap();
    s.ds.deliver("01MSG2", "alice", "ack again");
    s.inst.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(posts(&s.ds), 1, "a widening update never runs by itself");
    let report = s.core.last_report();
    assert_eq!((report.state, report.reason.as_str()), (BotAppRunState::AwaitingApproval, "build_widens"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_from_another_source_waits_for_an_admin() {
    let s = setup();
    s.core.set(Ok(Ok(auto_updating("https://apps.example/triage.wasm"))));
    std::fs::write(s.component.with_file_name("pollis-app.json"), MANIFEST.replace("4096", "2048")).unwrap();
    s.ds.deliver("01MSG1", "alice", "ack thanks");
    s.inst.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(posts(&s.ds), 0);
    assert_eq!(s.core.last_report().reason, "hash_mismatch");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_approval_runs_nothing_and_is_reported_as_refused() {
    let s = setup();
    for refusal in [
        AppApprovalRefusal::BadSignature,
        AppApprovalRefusal::NotAdmin("roster: member, not admin".into()),
        AppApprovalRefusal::RolledBack { accepted: 3, served: 2 },
        AppApprovalRefusal::WrongKind,
    ] {
        s.core.set(Ok(Err(refusal.clone())));
        s.ds.deliver(&format!("01MSG{}", refusal.code()), "alice", "ack thanks");
        let r = s.inst.run_job(JobInput::Wake).await.unwrap();
        assert!(matches!(&r.triggers[0].result, Ok(TriggerOutcome::HandlerFailed { .. })), "{refusal}");
        let report = s.core.last_report();
        assert_eq!((report.state, report.reason.as_str()), (BotAppRunState::Refused, refusal.code()));
    }
    assert_eq!(posts(&s.ds), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_that_cannot_be_checked_stops_the_job_without_skipping_a_message() {
    let s = setup();
    s.core.set(Err(CoreError("the Delivery Service is unreachable".into())));
    s.ds.deliver("01MSG1", "alice", "ack thanks");
    let err = s.inst.run_job(JobInput::Wake).await.unwrap_err();
    assert!(matches!(err, JobError::Approval(_)), "{err}");
    // Reachable again: the same message is handled, nothing was skipped.
    s.core.set(Ok(Ok(production(hex_app_hash(&wasm(), MANIFEST)))));
    let r = s.inst.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(r.triggers.len(), 1);
    assert_eq!(posts(&s.ds), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_development_installation_runs_builds_its_developer_signed() {
    let s = setup();
    let seed = [4u8; 32];
    let dev = AppApprovalRecord::Development(DevApproval {
        approver_device_id: "ADMIN-D".into(),
        approver_user_id: "ADMIN".into(),
        bot_user_id: BOT.into(),
        developer_key: developer_key_public(&seed),
        grants: grants(),
        group_id: "GROUP".into(),
        installation_id: BOT.into(),
        issued_at: 1_700_000_000,
        manifest_hash: pollis_app_manifest::hex(&pollis_app_manifest::manifest_hash(MANIFEST.as_bytes()).unwrap()),
        seq: 2,
        v: APP_APPROVAL_VERSION,
    });
    s.core.set(Ok(Ok(VerifiedAppApproval {
        record: dev,
        manifest_json: MANIFEST.into(),
        manifest: Manifest::from_json(MANIFEST.as_bytes()).unwrap(),
    })));

    // Unsigned: refused, and admins see the build's developer key is missing.
    s.ds.deliver("01MSG1", "alice", "ack one");
    s.inst.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(posts(&s.ds), 0);
    assert_eq!(s.core.last_report().reason, "developer_key_mismatch");

    // Signed by the approved developer key: runs.
    let hash = pollis_app_manifest::app_hash(&wasm(), MANIFEST.as_bytes()).unwrap();
    let sig = sign_dev_build(&seed, &hash);
    let sig_path = format!("{}.sig.json", s.component.display());
    std::fs::write(&sig_path, serde_json::to_vec(&sig).unwrap()).unwrap();
    s.ds.deliver("01MSG2", "alice", "ack two");
    s.inst.run_job(JobInput::Wake).await.unwrap();
    assert_eq!(posts(&s.ds), 1);
    let report = s.core.last_report();
    assert_eq!(report.state, BotAppRunState::Running);
    assert_eq!(report.developer_key, Some(developer_key_public(&seed)));
}

#[tokio::test]
async fn an_artifact_comes_only_from_a_path_or_an_https_url() {
    let err = read_location(&ArtifactLocation("http://apps.example/app.wasm".into()), 10, false).await.unwrap_err();
    assert!(err.to_string().contains("only https://"), "{err}");
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.wasm");
    std::fs::write(&big, vec![0u8; 11]).unwrap();
    assert!(read_location(&big.clone().into(), 10, false).await.is_err(), "over the cap");
    assert!(read_location(&dir.path().join("absent.sig.json").into(), 10, true).await.unwrap().is_none());
}
