//! App approvals (#2005), the parts that need an open state: the runtime's
//! check before every load, its report to the group's admins, the admin's
//! approve flow, and the members' read of each installation's approved App.
//!
//! Every reader verifies a record the same way, from its own state
//! ([`verify_row`]): the record decodes as the kind its row says, names this
//! installation and row, the served manifest hashes to `manifest_hash`, the
//! grants are not wider than it, and the signer is a certified device of a
//! current admin in the reader's MLS view and roster (`signer.rs`, shared with
//! #1182 host approvals). The runtime additionally refuses a `seq` lower than
//! the highest it already accepted (kept in the installation's encrypted
//! store), so the DS cannot roll an installation back by withholding a newer
//! record.

use std::sync::Arc;

use pollis_api::bot_apps::{
    AppApprovalKind, AppApprovalWire, ApproveBotAppBody, ApproveBotAppResponse, BotAppReportBody, BotAppReportWire,
    BotAppRunState, BotAppWire, BotAppsReadBody,
};
use pollis_app_manifest::{AdminGrants, Manifest, ManifestChange};
use serde::{Deserialize, Serialize};

use super::app_approval::{
    check_record, domain_of, AppApproval, AppApprovalRecord, AppApprovalRefusal, DevApproval, VerifiedAppApproval,
    APP_APPROVAL_VERSION,
};
use super::host::{AccountKeyAudit, TransparencyLogAudit};
use super::signer;
use super::unattended::BotSession;
use crate::error::{Error, Result};
use crate::state::AppState;

/// Error prefix: another admin approved first; reload and look again.
pub const APP_APPROVAL_STALE: &str = "app_approval_stale";
/// Error prefix: what the admin saw is no longer what the runtime reports.
pub const APP_APPROVAL_CHANGED: &str = "app_approval_changed";

fn err(msg: impl std::fmt::Display) -> Error {
    Error::Other(anyhow::anyhow!("{msg}"))
}

async fn read_apps(state: &Arc<AppState>, group_id: &str, bot_user_id: Option<&str>, viewer: &str) -> Result<Vec<BotAppWire>> {
    let body = BotAppsReadBody {
        group_id: group_id.to_string(),
        bot_user_id: bot_user_id.map(str::to_string),
        user_id: Some(viewer.to_string()),
    };
    Ok(crate::commands::mls::ds_post_json(state, &body).await?.apps)
}

/// Verify one stored approval from `viewer`'s own state. `Ok(Err(_))` is a
/// refusal; `Err` a failure to check (the network, the local store).
pub async fn verify_row(
    state: &Arc<AppState>,
    group_id: &str,
    viewer_user_id: &str,
    bot_user_id: &str,
    row: &AppApprovalWire,
    audit: &dyn AccountKeyAudit,
) -> Result<std::result::Result<VerifiedAppApproval, AppApprovalRefusal>> {
    let (record, payload, signature) = match AppApprovalRecord::decode(row.kind, &row.record) {
        Ok(v) => v,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let manifest = match check_record(&record, row.seq, bot_user_id, group_id, &row.manifest) {
        Ok(m) => m,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let c = record.common();
    if let Err(refusal) = signer::verify_signer(
        state,
        group_id,
        viewer_user_id,
        c.approver_user_id,
        c.approver_device_id,
        domain_of(row.kind),
        &payload,
        &signature,
    )
    .await?
    {
        return Ok(Err(refusal.into()));
    }
    if let Err(refusal) =
        signer::verify_signer_is_admin(state, group_id, viewer_user_id, c.approver_user_id, audit).await?
    {
        return Ok(Err(refusal.into()));
    }
    Ok(Ok(VerifiedAppApproval { record, manifest_json: row.manifest.clone(), manifest }))
}

// ---------------------------------------------------------------- runtime

/// The runtime's check before every load: read this installation's latest
/// approval, verify it from the host's own state, and refuse a `seq` lower
/// than the highest already accepted (then remember this one). The caller
/// holds the artifact against the result with `app_approval::check_artifact`.
pub async fn verify_installation_approval(
    state: &Arc<AppState>,
    session: &BotSession,
    audit: &dyn AccountKeyAudit,
) -> Result<std::result::Result<VerifiedAppApproval, AppApprovalRefusal>> {
    let apps = read_apps(state, &session.group_id, Some(&session.bot_user_id), &session.bot_user_id).await?;
    let Some(row) = apps.into_iter().find(|a| a.bot_user_id == session.bot_user_id).and_then(|a| a.approval) else {
        return Ok(Err(AppApprovalRefusal::NotApproved));
    };
    let accepted = accepted_seq(state, &session.bot_user_id).await?;
    if row.seq < accepted {
        return Ok(Err(AppApprovalRefusal::RolledBack { accepted, served: row.seq }));
    }
    let verified = match verify_row(state, &session.group_id, &session.bot_user_id, &session.bot_user_id, &row, audit).await? {
        Ok(v) => v,
        Err(refusal) => return Ok(Err(refusal)),
    };
    accept_seq(state, &session.bot_user_id, verified.seq()).await?;
    Ok(Ok(verified))
}

/// The highest approval `seq` this host accepted for the installation (0: none).
pub async fn accepted_seq(state: &Arc<AppState>, installation_id: &str) -> Result<i64> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| err("bot installation is not active"))?;
    Ok(db
        .conn()
        .query_row(
            "SELECT seq FROM bot_app_accepted WHERE installation_id = ?1",
            rusqlite::params![installation_id],
            |r| r.get(0),
        )
        .unwrap_or(0))
}

async fn accept_seq(state: &Arc<AppState>, installation_id: &str, seq: i64) -> Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| err("bot installation is not active"))?;
    db.conn().execute(
        "INSERT INTO bot_app_accepted (installation_id, seq) VALUES (?1, ?2) \
         ON CONFLICT (installation_id) DO UPDATE SET seq = MAX(seq, excluded.seq)",
        rusqlite::params![installation_id, seq],
    )?;
    Ok(())
}

/// What the runtime found on its last check, for the group's admins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRunReport {
    pub state: BotAppRunState,
    pub reason: String,
    pub detail: String,
    pub app_hash: Option<String>,
    pub manifest: Option<String>,
    pub source: Option<String>,
    pub developer_key: Option<String>,
}

impl AppRunReport {
    /// The report for a refusal, with the artifact the runtime has (so an
    /// admin can approve it when the refusal awaits approval).
    pub fn refused(refusal: &AppApprovalRefusal, artifact: ArtifactFacts) -> AppRunReport {
        AppRunReport {
            state: if refusal.awaits_approval() { BotAppRunState::AwaitingApproval } else { BotAppRunState::Refused },
            reason: refusal.code().to_string(),
            detail: refusal.to_string().chars().take(500).collect(),
            app_hash: artifact.app_hash,
            manifest: artifact.manifest,
            source: artifact.source,
            developer_key: artifact.developer_key,
        }
    }

    pub fn running(artifact: ArtifactFacts) -> AppRunReport {
        AppRunReport {
            state: BotAppRunState::Running,
            reason: String::new(),
            detail: String::new(),
            app_hash: artifact.app_hash,
            manifest: artifact.manifest,
            source: artifact.source,
            developer_key: artifact.developer_key,
        }
    }
}

/// What the runtime knows about the artifact it has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactFacts {
    pub app_hash: Option<String>,
    pub manifest: Option<String>,
    pub source: Option<String>,
    pub developer_key: Option<String>,
}

/// Send the report unless it is the one this host sent last (kept in the
/// installation's encrypted store), so a steady state costs no DS write.
pub async fn report_app(state: &Arc<AppState>, session: &BotSession, report: &AppRunReport) -> Result<()> {
    let text = serde_json::to_string(report).map_err(|e| err(format!("report: {e}")))?;
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| err("bot installation is not active"))?;
        let last: Option<String> = db
            .conn()
            .query_row(
                "SELECT last_report FROM bot_app_accepted WHERE installation_id = ?1",
                rusqlite::params![session.bot_user_id],
                |r| r.get(0),
            )
            .unwrap_or(None);
        if last.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
    }
    let body = BotAppReportBody {
        installation_id: session.bot_user_id.clone(),
        state: report.state,
        reason: report.reason.clone(),
        detail: report.detail.clone(),
        app_hash: report.app_hash.clone(),
        manifest: report.manifest.clone(),
        source: report.source.clone(),
        developer_key: report.developer_key.clone(),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| err("bot installation is not active"))?;
    db.conn().execute(
        "INSERT INTO bot_app_accepted (installation_id, seq, last_report) VALUES (?1, 0, ?2) \
         ON CONFLICT (installation_id) DO UPDATE SET last_report = excluded.last_report",
        rusqlite::params![session.bot_user_id, text],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- views

/// A text channel of the group, for the grant controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelOption {
    pub id: String,
    pub name: String,
}

/// The installation's approved App, as verified on this device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovedAppView {
    pub seq: i64,
    pub development: bool,
    /// Production only, lowercase hex.
    pub app_hash: Option<String>,
    /// Development only, base64.
    pub developer_key: Option<String>,
    pub manifest_hash: String,
    /// The manifest (the `pollis-app.json` shape).
    pub manifest: Manifest,
    pub grants: AdminGrants,
    pub source: Option<String>,
    pub auto_update: bool,
    pub approver_user_id: String,
    /// Unix seconds.
    pub issued_at: i64,
    /// Effective egress is empty: "Can't send data outside this group".
    pub cannot_send_outside: bool,
}

/// What the runtime has and an admin may approve.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingAppView {
    pub app_hash: String,
    pub manifest_hash: String,
    pub manifest: Manifest,
    pub source: Option<String>,
    /// Set when the runtime runs a development build signed by this key.
    pub developer_key: Option<String>,
    /// Against the approved manifest; empty for a first approval.
    pub changes: Vec<ManifestChange>,
    /// Any change widens: never pre-accepted, always shown first.
    pub widens: bool,
    /// The grants a non-widening update is approved with (the current grants
    /// carried to the new manifest), or everything the manifest asks for on a
    /// first approval.
    pub suggested_grants: AdminGrants,
    /// A non-widening update from the pinned source of an installation set to
    /// update itself: `auto_approve_bot_app` approves it.
    pub auto_approvable: bool,
}

/// The runtime's last report.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppReportView {
    pub state: BotAppRunState,
    pub reason: String,
    pub detail: String,
    pub updated_at: String,
}

/// A refusal of the stored approval on this device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusalView {
    pub code: String,
    pub detail: String,
}

/// `get_bot_app`: everything the bot's page shows an admin about its App.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotAppView {
    pub bot_user_id: String,
    pub group_id: String,
    pub approved: Option<ApprovedAppView>,
    /// The latest stored approval did not verify here.
    pub approval_refusal: Option<RefusalView>,
    pub report: Option<AppReportView>,
    pub pending: Option<PendingAppView>,
    /// The group's text channels the manifest (pending, else approved) asks for.
    pub channels: Vec<ChannelOption>,
    pub latest_seq: i64,
}

/// One installation as members see it (`list_group_apps`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupAppView {
    pub bot_user_id: String,
    pub development: bool,
    pub manifest: Manifest,
    /// The approved manifest's JSON text, for readers that need fields this
    /// build does not know yet (slash commands, #2018).
    pub manifest_json: String,
    pub grants: AdminGrants,
    pub cannot_send_outside: bool,
}

fn approved_view(v: &VerifiedAppApproval) -> ApprovedAppView {
    let c = v.record.common();
    let effective = pollis_app_manifest::effective_grants(&v.manifest, c.grants);
    let (development, app_hash, developer_key, source, auto_update) = match &v.record {
        AppApprovalRecord::Production(a) => (false, Some(a.app_hash.clone()), None, a.source.clone(), a.auto_update),
        AppApprovalRecord::Development(d) => (true, None, Some(d.developer_key.clone()), None, false),
    };
    ApprovedAppView {
        seq: c.seq,
        development,
        app_hash,
        developer_key,
        manifest_hash: c.manifest_hash.to_string(),
        manifest: v.manifest.clone(),
        grants: c.grants.clone(),
        source,
        auto_update,
        approver_user_id: c.approver_user_id.to_string(),
        issued_at: c.issued_at,
        cannot_send_outside: effective.egress.is_empty(),
    }
}

/// Everything the manifest asks for, among the group's channels.
fn full_grants(manifest: &Manifest, channels: &[ChannelOption]) -> AdminGrants {
    AdminGrants {
        channels: channels
            .iter()
            .filter(|c| manifest.requests_channel(&c.name))
            .map(|c| pollis_app_manifest::ChannelGrant { id: c.id.clone(), name: c.name.to_lowercase() })
            .collect(),
        history: manifest.read.history,
        events: manifest.read.events.clone(),
        actions: manifest.actions.clone(),
        egress: manifest.egress.clone(),
    }
    .normalized()
}

fn pending_view(
    approved: Option<&VerifiedAppApproval>,
    report: &BotAppReportWire,
    channels: &[ChannelOption],
) -> Option<PendingAppView> {
    let app_hash = report.app_hash.clone()?;
    let manifest_json = report.manifest.clone()?;
    let manifest = Manifest::from_json(manifest_json.as_bytes()).ok()?;
    // Recomputed here: the manifest hash this device signs is the one it shows.
    let manifest_hash = pollis_app_manifest::hex(&pollis_app_manifest::manifest_hash(manifest_json.as_bytes()).ok()?);
    let current = match approved.map(|v| &v.record) {
        Some(AppApprovalRecord::Production(a)) => {
            a.app_hash == app_hash && report.developer_key.is_none()
        }
        Some(AppApprovalRecord::Development(d)) => report.developer_key.as_deref() == Some(d.developer_key.as_str()),
        None => false,
    };
    if current && report.state != BotAppRunState::AwaitingApproval {
        return None;
    }
    let (changes, suggested) = match approved {
        Some(v) => (pollis_app_manifest::diff(&v.manifest, &manifest), v.grants().carried_to(&manifest)),
        None => (Vec::new(), full_grants(&manifest, channels)),
    };
    let widens = changes.iter().any(|c| c.widening);
    let auto_approvable = match approved.map(|v| &v.record) {
        Some(AppApprovalRecord::Production(a)) => {
            a.auto_update
                && !widens
                && report.developer_key.is_none()
                && a.source.is_some()
                && a.source == report.source
                && a.app_hash != app_hash
        }
        _ => false,
    };
    Some(PendingAppView {
        app_hash,
        manifest_hash,
        manifest,
        source: report.source.clone(),
        developer_key: report.developer_key.clone(),
        changes,
        widens,
        suggested_grants: suggested.normalized(),
        auto_approvable,
    })
}

async fn text_channels(state: &Arc<AppState>, group_id: &str, me: &str) -> Result<Vec<ChannelOption>> {
    let channels = crate::commands::groups::list_group_channels(group_id.to_string(), me.to_string(), state).await?;
    Ok(channels
        .into_iter()
        .filter(|c| c.channel_type == "text")
        .map(|c| ChannelOption { id: c.id, name: c.name })
        .collect())
}

struct Loaded {
    view: BotAppView,
    verified: Option<VerifiedAppApproval>,
    report: Option<BotAppReportWire>,
}

async fn load(state: &Arc<AppState>, group_id: &str, bot_user_id: &str, audit: &dyn AccountKeyAudit) -> Result<Loaded> {
    let me = crate::commands::mls::current_user_id(state).await?;
    let apps = read_apps(state, group_id, Some(bot_user_id), &me).await?;
    let Some(app) = apps.into_iter().find(|a| a.bot_user_id == bot_user_id) else {
        return Err(err("no bot with that id in this group"));
    };
    let channels = text_channels(state, group_id, &me).await?;
    let latest_seq = app.approval.as_ref().map_or(0, |a| a.seq);
    let (verified, approval_refusal) = match &app.approval {
        None => (None, None),
        Some(row) => match verify_row(state, group_id, &me, bot_user_id, row, audit).await? {
            Ok(v) => (Some(v), None),
            Err(r) => (None, Some(RefusalView { code: r.code().to_string(), detail: r.to_string() })),
        },
    };
    let pending = app.report.as_ref().and_then(|r| pending_view(verified.as_ref(), r, &channels));
    let shown = pending.as_ref().map(|p| p.manifest.clone()).or_else(|| verified.as_ref().map(|v| v.manifest.clone()));
    let channels = match &shown {
        Some(m) => channels.into_iter().filter(|c| m.requests_channel(&c.name)).collect(),
        None => Vec::new(),
    };
    let view = BotAppView {
        bot_user_id: bot_user_id.to_string(),
        group_id: group_id.to_string(),
        approved: verified.as_ref().map(approved_view),
        approval_refusal,
        report: app.report.as_ref().map(|r| AppReportView {
            state: r.state,
            reason: r.reason.clone(),
            detail: r.detail.clone(),
            updated_at: r.updated_at.clone(),
        }),
        pending,
        channels,
        latest_seq,
    };
    Ok(Loaded { view, verified, report: app.report })
}

/// The bot's App as its page shows it to an admin: the approved App (verified
/// on this device), what the runtime reports, and what is waiting for approval
/// with its diff against the approved manifest.
pub async fn get_bot_app(state: &Arc<AppState>, group_id: String, bot_user_id: String) -> Result<BotAppView> {
    Ok(load(state, &group_id, &bot_user_id, &TransparencyLogAudit).await?.view)
}

/// What the admin chose on the approve page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproveBotAppInput {
    pub group_id: String,
    pub bot_user_id: String,
    /// Approve as a development installation (names a developer key, not a hash).
    #[serde(default)]
    pub development: bool,
    /// Production: the App hash the admin saw.
    #[serde(default)]
    pub app_hash: Option<String>,
    /// Development: the developer key the admin saw.
    #[serde(default)]
    pub developer_key: Option<String>,
    /// The manifest hash the admin saw.
    pub manifest_hash: String,
    pub grants: AdminGrants,
    /// Production: approve later non-widening updates from the same source automatically.
    #[serde(default)]
    pub auto_update: bool,
}

/// Sign and store the next approval for an installation. Refuses grants wider
/// than the manifest, and anything that is not what the admin saw (the App hash
/// or developer key, and the manifest hash, must match what the runtime
/// reported or what is approved now).
pub async fn approve_bot_app(state: &Arc<AppState>, input: ApproveBotAppInput) -> Result<BotAppView> {
    let loaded = load(state, &input.group_id, &input.bot_user_id, &TransparencyLogAudit).await?;
    submit_approval(state, &loaded, &input).await?;
    get_bot_app(state, input.group_id, input.bot_user_id).await
}

async fn submit_approval(state: &Arc<AppState>, loaded: &Loaded, input: &ApproveBotAppInput) -> Result<()> {
    let changed = || err(format!("{APP_APPROVAL_CHANGED}: the App changed since this page loaded; look again"));
    // The manifest the admin saw: the pending one, or the approved one (narrowing in place).
    let candidates = loaded
        .report
        .as_ref()
        .and_then(|r| r.manifest.clone())
        .into_iter()
        .chain(loaded.verified.as_ref().map(|v| v.manifest_json.clone()));
    let mut manifest_json = None;
    for m in candidates {
        let h = pollis_app_manifest::manifest_hash(m.as_bytes()).map_err(|e| err(e.to_string()))?;
        if pollis_app_manifest::hex(&h) == input.manifest_hash {
            manifest_json = Some(m);
            break;
        }
    }
    let manifest_json = manifest_json.ok_or_else(changed)?;
    let manifest = Manifest::from_json(manifest_json.as_bytes()).map_err(|e| err(e.to_string()))?;
    let grants = input.grants.clone().normalized();
    grants.check_not_wider(&manifest).map_err(|e| err(e.to_string()))?;

    let me = crate::commands::mls::current_user_id(state).await?;
    let my_device = state.device_id.lock().await.clone().ok_or_else(|| err("no device id"))?;
    let seq = loaded.view.latest_seq + 1;
    let issued_at = chrono::Utc::now().timestamp();
    let report = loaded.report.as_ref();
    let record = if input.development {
        let key = input.developer_key.clone().ok_or_else(changed)?;
        let known = report.and_then(|r| r.developer_key.as_ref()) == Some(&key)
            || matches!(loaded.verified.as_ref().map(|v| &v.record), Some(AppApprovalRecord::Development(d)) if d.developer_key == key);
        if !known {
            return Err(changed());
        }
        AppApprovalRecord::Development(DevApproval {
            approver_device_id: my_device.clone(),
            approver_user_id: me.clone(),
            bot_user_id: input.bot_user_id.clone(),
            developer_key: key,
            grants,
            group_id: input.group_id.clone(),
            installation_id: input.bot_user_id.clone(),
            issued_at,
            manifest_hash: input.manifest_hash.clone(),
            seq,
            v: APP_APPROVAL_VERSION,
        })
    } else {
        let hash = input.app_hash.clone().ok_or_else(changed)?;
        let source = if report.and_then(|r| r.app_hash.as_ref()) == Some(&hash) {
            report.and_then(|r| r.source.clone())
        } else {
            match loaded.verified.as_ref().map(|v| &v.record) {
                Some(AppApprovalRecord::Production(a)) if a.app_hash == hash => a.source.clone(),
                _ => return Err(changed()),
            }
        };
        AppApprovalRecord::Production(AppApproval {
            app_hash: hash,
            approver_device_id: my_device.clone(),
            approver_user_id: me.clone(),
            auto_update: input.auto_update,
            bot_user_id: input.bot_user_id.clone(),
            grants,
            group_id: input.group_id.clone(),
            installation_id: input.bot_user_id.clone(),
            issued_at,
            manifest_hash: input.manifest_hash.clone(),
            seq,
            source,
            v: APP_APPROVAL_VERSION,
        })
    };
    let signed = sign_app_approval(state, &me, &my_device, &record).await?;
    let body = ApproveBotAppBody {
        bot_user_id: input.bot_user_id.clone(),
        seq,
        kind: record.kind(),
        record: signed,
        manifest: manifest_json,
        user_id: Some(me),
    };
    match crate::commands::mls::ds_post_json(state, &body).await? {
        ApproveBotAppResponse::Approved => Ok(()),
        ApproveBotAppResponse::Stale { .. } => {
            Err(err(format!("{APP_APPROVAL_STALE}: another admin approved this bot just now; look again")))
        }
    }
}

/// Sign an App approval with this device's ML-DSA-44 key. Public for the flows
/// harness, which forges records that should be refused.
pub async fn sign_app_approval(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
    record: &AppApprovalRecord,
) -> Result<String> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| err("not signed in"))?;
    let provider = crate::commands::mls::PollisProvider::new(db.conn());
    let (key, _) = crate::commands::mls::load_device_pq_signing_key(&provider, user_id, device_id)?;
    Ok(record.sign(&key))
}

/// For an installation set to update itself: approve a waiting non-widening
/// update from the pinned source with the current grants carried over. The
/// apps call this when an admin's app shows the group's bots, so the update
/// lands the next time any admin is in the app; each still gets its own
/// signed record. `true` when it approved one.
pub async fn auto_approve_bot_app(state: &Arc<AppState>, group_id: String, bot_user_id: String) -> Result<bool> {
    let loaded = load(state, &group_id, &bot_user_id, &TransparencyLogAudit).await?;
    let Some(pending) = loaded.view.pending.clone().filter(|p| p.auto_approvable) else {
        return Ok(false);
    };
    let input = ApproveBotAppInput {
        group_id,
        bot_user_id,
        development: false,
        app_hash: Some(pending.app_hash),
        developer_key: None,
        manifest_hash: pending.manifest_hash,
        grants: pending.suggested_grants,
        auto_update: true,
    };
    submit_approval(state, &loaded, &input).await?;
    Ok(true)
}

/// Every installation in the group with an approved App that verifies on
/// this device, for members: the "Development build" mark, the "Can't send
/// data outside this group" mark, and (#2018) the approved manifest's commands.
/// An approval that does not verify here is left out.
pub async fn list_group_apps(state: &Arc<AppState>, group_id: String) -> Result<Vec<GroupAppView>> {
    let me = crate::commands::mls::current_user_id(state).await?;
    let apps = read_apps(state, &group_id, None, &me).await?;
    let mut out = Vec::new();
    for app in apps {
        let Some(row) = &app.approval else {
            continue;
        };
        match verify_row(state, &group_id, &me, &app.bot_user_id, row, &TransparencyLogAudit).await? {
            Ok(v) => {
                let grants = v.grants().clone();
                let effective = pollis_app_manifest::effective_grants(&v.manifest, &grants);
                out.push(GroupAppView {
                    bot_user_id: app.bot_user_id.clone(),
                    development: row.kind == AppApprovalKind::Development,
                    manifest: v.manifest.clone(),
                    manifest_json: v.manifest_json.clone(),
                    grants,
                    cannot_send_outside: effective.egress.is_empty(),
                });
            }
            Err(refusal) => {
                crate::diag!("[bots] App approval of {} does not verify here: {refusal}", app.bot_user_id);
            }
        }
    }
    Ok(out)
}
