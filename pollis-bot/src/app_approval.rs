//! Loading an App only under a verified approval (#2005).
//!
//! Before every job of an App installation the runtime:
//!
//! 1. loads the artifact (the component, its `pollis-app.json` and, for a
//!    development build, `<component>.sig.json`) from the local path or HTTPS
//!    URL given at setup. The approval binds a hash, not a location;
//! 2. reads the installation's latest approval and verifies it from the
//!    host's own state (`InstallationCore::app_approval`: signature, the
//!    approver's certified device and current admin role in this host's MLS
//!    view and roster, manifest hash, grants, and a `seq` not lower than the
//!    highest this host already accepted);
//! 3. holds the artifact against it (`app_approval::check_artifact_from`):
//!    exactly the approved App hash; or, when the approval allows automatic
//!    updates, a newer artifact from exactly the pinned source that widens
//!    nothing (accepted by the runtime itself, no admin online, and never
//!    undone: `InstallationCore::app_accept_hash`); or a development build
//!    signed by the approved developer key that does not widen the approved
//!    manifest.
//!
//! Any failure: the installation does not run this job, the runtime logs why,
//! and the group's admins see it on the bot's page (`InstallationCore::app_report`,
//! sent only when it changes). A hash mismatch is never "close enough".

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pollis_app_manifest::{Manifest, TriggerKind};
use pollis_core::commands::bots::app_approval::{
    check_artifact_from, AppApprovalRecord, AppApprovalRefusal, Artifact, DevBuildSignature, LoadableApp, DEV_BUILD_SIGNATURE_SUFFIX,
};
use pollis_core::commands::bots::apps::{AppRunReport, ArtifactFacts};
use serde::{Deserialize, Serialize};

use crate::core::{CoreError, InstallationCore};

/// The developer key `pollis-bot app dev-key` creates, in the data dir.
pub const DEV_KEY_FILE: &str = "developer-key";

/// The largest component the runtime fetches or reads.
pub const MAX_COMPONENT_BYTES: usize = 64 * 1024 * 1024;
/// The largest manifest or build signature it reads.
pub const MAX_SIDECAR_BYTES: usize = 64 * 1024;
/// How long a fetched HTTPS artifact is reused before the next job fetches it
/// again (a path is read on every job). Jobs only: there is no timer.
pub const URL_REFETCH: Duration = Duration::from_secs(300);
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Where an artifact file is: a local path, or an `https://` URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactLocation(pub String);

impl ArtifactLocation {
    pub fn is_url(&self) -> bool {
        self.0.starts_with("https://")
    }

    /// The development build signature beside a component.
    pub fn signature(&self) -> ArtifactLocation {
        ArtifactLocation(format!("{}{DEV_BUILD_SIGNATURE_SUFFIX}", self.0))
    }
}

impl From<&Path> for ArtifactLocation {
    fn from(p: &Path) -> Self {
        ArtifactLocation(p.display().to_string())
    }
}

impl From<std::path::PathBuf> for ArtifactLocation {
    fn from(p: std::path::PathBuf) -> Self {
        ArtifactLocation(p.display().to_string())
    }
}

impl std::fmt::Display for ArtifactLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The artifact as read.
#[derive(Clone)]
pub struct FetchedArtifact {
    pub component: std::sync::Arc<Vec<u8>>,
    pub manifest_json: Vec<u8>,
    pub dev_signature: Option<DevBuildSignature>,
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .https_only(true)
        .redirect(reqwest::redirect::Policy::custom(|a| {
            if a.url().scheme() == "https" && a.previous().len() < 5 {
                a.follow()
            } else {
                a.stop()
            }
        }))
        .build()
        .expect("the artifact client builds")
}

/// Read one file of an artifact. `Ok(None)` when an optional file is absent.
pub async fn read_location(location: &ArtifactLocation, max: usize, optional: bool) -> anyhow::Result<Option<Vec<u8>>> {
    if location.0.starts_with("http://") {
        anyhow::bail!("{location}: only https:// URLs or local paths");
    }
    if location.is_url() {
        let resp = http().get(&location.0).send().await?;
        if optional && resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp.error_for_status()?;
        if resp.content_length().is_some_and(|n| n as usize > max) {
            anyhow::bail!("{location} is larger than {max} bytes");
        }
        let bytes = resp.bytes().await?;
        if bytes.len() > max {
            anyhow::bail!("{location} is larger than {max} bytes");
        }
        return Ok(Some(bytes.to_vec()));
    }
    let path = Path::new(&location.0);
    if optional && !path.exists() {
        return Ok(None);
    }
    let meta = tokio::fs::metadata(path).await.map_err(|e| anyhow::anyhow!("{location}: {e}"))?;
    if meta.len() as usize > max {
        anyhow::bail!("{location} is larger than {max} bytes");
    }
    Ok(Some(tokio::fs::read(path).await.map_err(|e| anyhow::anyhow!("{location}: {e}"))?))
}

/// Read the whole artifact.
pub async fn fetch_artifact(component: &ArtifactLocation, manifest: &ArtifactLocation) -> anyhow::Result<FetchedArtifact> {
    let bytes = read_location(component, MAX_COMPONENT_BYTES, false).await?.unwrap_or_default();
    let manifest_json = read_location(manifest, MAX_SIDECAR_BYTES, false).await?.unwrap_or_default();
    let dev_signature = match read_location(&component.signature(), MAX_SIDECAR_BYTES, true).await? {
        Some(b) => Some(
            serde_json::from_slice(&b)
                .map_err(|e| anyhow::anyhow!("{}: not a build signature: {e}", component.signature()))?,
        ),
        None => None,
    };
    Ok(FetchedArtifact { component: std::sync::Arc::new(bytes), manifest_json, dev_signature })
}

/// What a job may run: the approved App and its grants.
#[derive(Clone)]
pub struct ReadyApp {
    pub app_hash: [u8; 32],
    pub component: std::sync::Arc<Vec<u8>>,
    pub manifest: Manifest,
    pub grants: pollis_app_manifest::AdminGrants,
    pub development: bool,
    /// The effective grants deliver `mentions`.
    pub mentions: bool,
    /// The manifest's schedules, when the effective grants deliver `schedule`.
    pub schedules: Vec<pollis_app_manifest::ScheduleSpec>,
}

/// The facts about an artifact the runtime reports to admins.
pub fn facts_of(component: &ArtifactLocation, artifact: &FetchedArtifact) -> ArtifactFacts {
    ArtifactFacts {
        app_hash: pollis_app_manifest::app_hash(&artifact.component, &artifact.manifest_json)
            .ok()
            .map(|h| pollis_app_manifest::hex(&h)),
        manifest: String::from_utf8(artifact.manifest_json.clone()).ok().filter(|m| m.len() <= MAX_SIDECAR_BYTES),
        source: Some(component.0.clone()).filter(|s| s.len() <= 2048),
        developer_key: artifact.dev_signature.as_ref().map(|s| s.developer_key.clone()),
    }
}

/// One installation's approved App, re-checked at the start of every job.
pub struct ApprovedApp {
    component: ArtifactLocation,
    manifest: ArtifactLocation,
    fetched: Mutex<Option<(Instant, FetchedArtifact)>>,
    /// This job's verdict, set by [`ApprovedApp::prepare`].
    current: Mutex<Option<Result<ReadyApp, String>>>,
    /// The App hash the last job ran, so a change is logged once.
    last_running: Mutex<Option<[u8; 32]>>,
}

impl ApprovedApp {
    pub fn new(component: ArtifactLocation, manifest: ArtifactLocation) -> ApprovedApp {
        ApprovedApp { component, manifest, fetched: Mutex::new(None), current: Mutex::new(None), last_running: Mutex::new(None) }
    }

    async fn artifact(&self) -> anyhow::Result<FetchedArtifact> {
        if self.component.is_url() {
            let cached = self.fetched.lock().ok().and_then(|g| g.clone());
            if let Some((at, a)) = cached {
                if at.elapsed() < URL_REFETCH {
                    return Ok(a);
                }
            }
        }
        let a = fetch_artifact(&self.component, &self.manifest).await?;
        if let Ok(mut g) = self.fetched.lock() {
            *g = Some((Instant::now(), a.clone()));
        }
        Ok(a)
    }

    fn set(&self, verdict: Result<ReadyApp, String>) {
        if let Ok(mut g) = self.current.lock() {
            *g = Some(verdict);
        }
    }

    /// Steps 1–3 for this job. `Err` only when the approval could not be
    /// checked at all (the DS unreachable): the job stops without handling
    /// anything, so nothing is skipped. A refusal is `Ok`: [`ApprovedApp::ready`]
    /// then names it and the job's triggers end without the App running.
    pub async fn prepare(&self, core: &dyn InstallationCore) -> Result<(), CoreError> {
        self.set(Err("the App has not been checked in this job".into()));
        let scope = crate::log::scope(core.installation_id());
        let artifact = match self.artifact().await {
            Ok(a) => a,
            Err(e) => {
                let why = format!("cannot load the App artifact from {}: {e:#}", self.component);
                crate::log::failure(&scope, &why, "check the path or URL given at setup is reachable from this host");
                let report = AppRunReport {
                    state: pollis_api::bot_apps::BotAppRunState::Refused,
                    reason: "artifact_unavailable".into(),
                    detail: why.chars().take(500).collect(),
                    app_hash: None,
                    manifest: None,
                    source: Some(self.component.0.clone()),
                    developer_key: None,
                };
                if let Err(e) = core.app_report(&report).await {
                    crate::log::info(&scope, format!("could not report to the group's admins: {e}"));
                }
                return Err(CoreError(why));
            }
        };
        let facts = facts_of(&self.component, &artifact);
        let verdict = match core.app_approval().await? {
            Err(refusal) => Err(refusal),
            Ok(approved) => {
                let checked = check_artifact_from(
                    &approved,
                    &Artifact {
                        component: &artifact.component,
                        manifest_json: &artifact.manifest_json,
                        dev_signature: artifact.dev_signature.as_ref(),
                    },
                    &self.component.0,
                );
                match (checked, &approved.record) {
                    // A production App runs only at the newest hash this host ran under
                    // this approval: an automatic update is never undone.
                    (Ok(app), AppApprovalRecord::Production(a)) => {
                        let hash = pollis_app_manifest::hex(&app.app_hash);
                        match core.app_accept_hash(a.seq, &a.app_hash, &hash).await? {
                            Ok(()) => Ok(app),
                            Err(refusal) => Err(refusal),
                        }
                    }
                    (other, _) => other,
                }
            }
        };
        let report = match &verdict {
            Ok(app) => AppRunReport::running(facts, app.auto_updated),
            Err(refusal) => AppRunReport::refused(refusal, facts),
        };
        if let Err(e) = core.app_report(&report).await {
            crate::log::info(&scope, format!("could not report to the group's admins: {e}"));
        }
        match verdict {
            Ok(app) => {
                let changed = self.last_running.lock().map(|mut g| g.replace(app.app_hash) != Some(app.app_hash)).unwrap_or(true);
                if changed {
                    let how = if app.auto_updated { " (updated automatically from the approved source)" } else { "" };
                    crate::log::info(&scope, format!("running App {}{how}", pollis_app_manifest::hex(&app.app_hash)));
                }
                self.set(Ok(ready(app, artifact)));
            }
            Err(refusal) => {
                log_refusal(&scope, &refusal);
                self.set(Err(refusal.to_string()));
            }
        }
        Ok(())
    }

    /// The App this job may run, or why it may not.
    pub fn ready(&self) -> Result<ReadyApp, String> {
        self.current
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_else(|| Err("the App has not been checked in this job".into()))
    }
}

fn ready(app: LoadableApp, artifact: FetchedArtifact) -> ReadyApp {
    let triggers = pollis_app_manifest::effective_grants(&app.manifest, &app.grants).triggers;
    let mentions = triggers.contains(&TriggerKind::Mentions);
    let schedules = if triggers.contains(&TriggerKind::Schedule) { app.manifest.schedules.clone() } else { Vec::new() };
    ReadyApp {
        schedules,
        app_hash: app.app_hash,
        component: artifact.component,
        manifest: app.manifest,
        grants: app.grants,
        development: app.development,
        mentions,
    }
}

fn log_refusal(scope: &str, refusal: &AppApprovalRefusal) {
    let fix = if refusal.awaits_approval() {
        "a group admin approves this App on the bot's page in Pollis (Bots → the bot); it runs at the next message after that"
    } else {
        "a group admin opens the bot's page in Pollis and approves the App again; until then it does not run"
    };
    crate::log::failure(scope, format!("not running the App: {refusal}"), fix);
}
