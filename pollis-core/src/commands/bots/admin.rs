//! The admin side of bots (#1182): the six bridge commands the Bots pages call.
//!
//! The typed pairing code is checked against the code re-derived from what the
//! DS served (`code::derive_pairing_code`), never against a DS-stored copy, and
//! the UI never pre-fills it (#1096).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use pollis_api::bots::{
    ApproveBotHostBody, BotCreated, BotPairingLookupBody, BotPairingRequestWire, BotWire, CreateBotBody,
    DeleteBotBody, ListBotsBody, PairingTarget,
};

use super::approval::{HostApproval, HOST_APPROVAL_MAX_TTL_SECS, HOST_APPROVAL_VERSION};
use super::code::{code_lookup, derive_pairing_code, normalize_pairing_code, PAIRING_CODE_LEN};
use crate::error::{Error, Result};
use crate::state::AppState;

/// Error prefix: the typed code matches no request, or not the one served.
pub const PAIRING_CODE_MISMATCH: &str = "pairing_code_mismatch";
/// Error prefix: the group has the most bots it may have.
pub const BOT_CAP_REACHED: &str = "bot_cap_reached";
/// Error prefix: the request lapsed or was already used.
pub const PAIRING_REQUEST_EXPIRED: &str = "pairing_request_expired";

/// How long `create_bot` waits for the new host's key packages before it
/// returns anyway (the host then joins by itself, `host::join_bot_group`).
const KEY_PACKAGE_WAIT: Duration = Duration::from_secs(120);
const KEY_PACKAGE_BACKOFF_START_MS: u64 = 500;
const KEY_PACKAGE_BACKOFF_MAX_MS: u64 = 8_000;

/// A bot as the Bots page lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotSummary {
    pub user_id: String,
    pub group_id: String,
    pub display_name: String,
    pub username: String,
    /// Empty once the creating admin's account is gone.
    pub created_by: String,
    pub created_at: String,
    /// Hosts not revoked.
    pub host_count: u32,
}

/// One host of a bot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotHost {
    pub device_id: String,
    pub name: String,
    pub last_seen_at: Option<String>,
    pub created_at: String,
}

/// A bot with its live hosts and the group's bot cap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotDetail {
    #[serde(flatten)]
    pub summary: BotSummary,
    pub hosts: Vec<BotHost>,
    pub group_bot_cap: u32,
}

fn err(msg: impl std::fmt::Display) -> Error {
    Error::Other(anyhow::anyhow!("{msg}"))
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn summary_of(group_id: &str, bot: &BotWire) -> BotSummary {
    BotSummary {
        user_id: bot.user_id.clone(),
        group_id: group_id.to_string(),
        display_name: bot.preferred_name.clone().unwrap_or_else(|| bot.username.clone()),
        username: bot.username.clone(),
        created_by: bot.created_by.clone().unwrap_or_default(),
        created_at: bot.created_at.clone(),
        host_count: bot.hosts.iter().filter(|h| h.revoked_at.is_none()).count() as u32,
    }
}

fn detail_of(group_id: &str, bot: &BotWire, cap: u32) -> BotDetail {
    BotDetail {
        summary: summary_of(group_id, bot),
        hosts: bot
            .hosts
            .iter()
            .filter(|h| h.revoked_at.is_none())
            .map(|h| BotHost {
                device_id: h.device_id.clone(),
                name: h.device_name.clone().unwrap_or_else(|| "host".to_string()),
                last_seen_at: Some(h.last_seen.clone()),
                created_at: h.created_at.clone(),
            })
            .collect(),
        group_bot_cap: cap,
    }
}

async fn actor(state: &Arc<AppState>) -> Result<String> {
    crate::commands::mls::current_user_id(state).await
}

async fn read_bots(state: &Arc<AppState>, group_id: &str) -> Result<(Vec<BotWire>, u32)> {
    let resp = crate::commands::mls::ds_post_json(
        state,
        &ListBotsBody { group_id: group_id.to_string(), user_id: Some(actor(state).await?) },
    )
    .await?;
    Ok((resp.bots, resp.limit))
}

/// The group's bots. Admins only (the DS answers 403 otherwise).
pub async fn list_group_bots(state: &Arc<AppState>, group_id: String) -> Result<Vec<BotSummary>> {
    let (bots, _) = read_bots(state, &group_id).await?;
    Ok(bots.iter().map(|b| summary_of(&group_id, b)).collect())
}

/// Find the group a bot belongs to among the groups the caller administers.
async fn bot_and_group(state: &Arc<AppState>, bot_user_id: &str) -> Result<(String, BotWire, u32)> {
    let me = actor(state).await?;
    let groups = crate::commands::groups::list_user_groups(me, state).await?;
    for g in groups {
        // Not an admin there: the read is refused, and the bot cannot be there anyway.
        let Ok((bots, cap)) = read_bots(state, &g.id).await else {
            continue;
        };
        if let Some(bot) = bots.into_iter().find(|b| b.user_id == bot_user_id) {
            return Ok((g.id, bot, cap));
        }
    }
    Err(err("no bot with that id in a group you administer"))
}

/// One bot with its hosts.
pub async fn get_bot(state: &Arc<AppState>, bot_user_id: String) -> Result<BotDetail> {
    let (group_id, bot, cap) = bot_and_group(state, &bot_user_id).await?;
    Ok(detail_of(&group_id, &bot, cap))
}

/// Look up the request the typed code names and check the code against the
/// one derived from what the DS served. Returns the request and its decoded
/// ephemeral key.
async fn checked_request(
    state: &Arc<AppState>,
    group_id: &str,
    typed: &str,
) -> Result<(BotPairingRequestWire, Vec<u8>)> {
    let code = normalize_pairing_code(typed);
    if code.chars().count() != PAIRING_CODE_LEN {
        return Err(err(format!("{PAIRING_CODE_MISMATCH}: a pairing code has {PAIRING_CODE_LEN} characters")));
    }
    let body = BotPairingLookupBody {
        group_id: group_id.to_string(),
        code_lookup: b64().encode(code_lookup(&code)),
        user_id: Some(actor(state).await?),
    };
    let Some(request) = crate::commands::mls::ds_post_json(state, &body).await?.request else {
        return Err(err(format!("{PAIRING_CODE_MISMATCH}: no open pairing request has this code")));
    };
    let ephemeral = crate::commands::ds_reads::decode_b64("ephemeral_pub", &request.ephemeral_pub)?;
    let expected = derive_pairing_code(&ephemeral, &request.device_id, &request.target)?;
    if !pollis_device_cert::constant_time_eq(expected.as_bytes(), code.as_bytes()) {
        return Err(err(format!(
            "{PAIRING_CODE_MISMATCH}: the request the server returned does not match this code"
        )));
    }
    if request.expires_at <= chrono::Utc::now().timestamp() {
        return Err(err(format!("{PAIRING_REQUEST_EXPIRED}: the pairing request has expired")));
    }
    Ok((request, ephemeral))
}

/// Create a bot in `group_id` from the code its host printed, then add it to
/// the group's MLS tree through the ordinary reconcile once the host has
/// published its key packages.
pub async fn create_bot(
    state: &Arc<AppState>,
    group_id: String,
    code: String,
    display_name: String,
) -> Result<BotSummary> {
    let (request, ephemeral) = checked_request(state, &group_id, &code).await?;
    if !matches!(request.target, PairingTarget::NewBot { .. }) {
        return Err(err(format!("{PAIRING_CODE_MISMATCH}: this code is for adding a host, not a new bot")));
    }
    let me = actor(state).await?;
    let body = CreateBotBody {
        group_id: group_id.clone(),
        request_id: request.request_id.clone(),
        ephemeral_pub: b64().encode(&ephemeral),
        name: display_name.trim().to_string(),
        description: None,
        user_id: Some(me.clone()),
    };
    let resp = crate::commands::mls::ds_post(state, &body).await?;
    let status = resp.status();
    if status == reqwest::StatusCode::CONFLICT {
        return Err(err(format!("{PAIRING_REQUEST_EXPIRED}: the pairing request is no longer open")));
    }
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(err(format!("create bot ({status}): {txt}")));
    }
    let (bot_user_id, device_id) = match crate::commands::mls::decode_response::<CreateBotBody>(resp).await? {
        BotCreated::Ok { bot_user_id, device_id, .. } => (bot_user_id, device_id),
        BotCreated::LimitReached { limit } => {
            return Err(err(format!("{BOT_CAP_REACHED}: this group already has {limit} bots")));
        }
    };

    add_to_tree_when_ready(state, &group_id, &me, &bot_user_id, &device_id).await;

    let (bots, _) = read_bots(state, &group_id).await?;
    bots.iter()
        .find(|b| b.user_id == bot_user_id)
        .map(|b| summary_of(&group_id, b))
        .ok_or_else(|| err("the bot was created but is not listed"))
}

/// Wait (backing off, bounded) for the new host's key packages, then reconcile
/// the group so the ordinary add path admits it. Best-effort: a host that is
/// slower than the wait joins by itself.
async fn add_to_tree_when_ready(state: &Arc<AppState>, group_id: &str, me: &str, bot_user_id: &str, device_id: &str) {
    let deadline = std::time::Instant::now() + KEY_PACKAGE_WAIT;
    let mut delay = Duration::from_millis(KEY_PACKAGE_BACKOFF_START_MS);
    let want = (bot_user_id.to_string(), device_id.to_string());
    loop {
        match crate::commands::ds_reads::claimable_devices(state, &[bot_user_id.to_string()], None).await {
            Ok(pairs) if pairs.contains(&want) => break,
            Ok(_) => {}
            Err(e) => crate::diag!("[bots] reading the new bot's key packages failed: {e}"),
        }
        if std::time::Instant::now() + delay >= deadline {
            crate::diag!("[bots] bot {bot_user_id}'s host has not published key packages yet; it will join by itself");
            return;
        }
        tokio::time::sleep(delay).await;
        delay = std::cmp::min(delay * 2, Duration::from_millis(KEY_PACKAGE_BACKOFF_MAX_MS));
    }
    match reconcile_group(state, group_id, me).await {
        Ok(outcome) => crate::diag!("[bots] added bot {bot_user_id}: {:?}", outcome.added),
        Err(e) => crate::diag!("[bots] adding bot {bot_user_id} to {group_id} failed (the sweep retries): {e}"),
    }
}

/// Catch the group up, then reconcile its tree to its roster (ingest-before-advance, #440).
async fn reconcile_group(
    state: &Arc<AppState>,
    group_id: &str,
    me: &str,
) -> Result<crate::commands::mls::ReconcileOutcome> {
    if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(state, group_id, me).await {
        crate::diag!("[bots] catch-up of {group_id} before reconcile: {e}");
    }
    crate::commands::mls::reconcile_group_mls_impl(state, group_id, me).await
}

/// Approve a new host for a bot by the code it printed: sign a host approval
/// with this device's key and hand it to the DS for the bot's hosts.
pub async fn add_bot_host(state: &Arc<AppState>, bot_user_id: String, code: String) -> Result<BotHost> {
    let (group_id, _bot, _) = bot_and_group(state, &bot_user_id).await?;
    let (request, _ephemeral) = checked_request(state, &group_id, &code).await?;
    match &request.target {
        PairingTarget::ExistingBot { bot_user_id: named } if *named == bot_user_id => {}
        PairingTarget::ExistingBot { .. } => {
            return Err(err(format!("{PAIRING_CODE_MISMATCH}: this code is for another bot")));
        }
        PairingTarget::NewBot { .. } => {
            return Err(err(format!("{PAIRING_CODE_MISMATCH}: this code is for a new bot, not a host")));
        }
    }
    let me = actor(state).await?;
    let my_device = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| err("device_id not set"))?;
    let now = chrono::Utc::now().timestamp();
    let approval = HostApproval {
        approver_device_id: my_device.clone(),
        approver_user_id: me.clone(),
        bot_user_id: bot_user_id.clone(),
        expires_at: std::cmp::min(now + HOST_APPROVAL_MAX_TTL_SECS, request.expires_at),
        group_id,
        issued_at: now,
        new_host_ephemeral_pub: request.ephemeral_pub.clone(),
        request_id: request.request_id.clone(),
        v: HOST_APPROVAL_VERSION,
    };
    let signed = sign_approval(state, &me, &my_device, &approval).await?;
    crate::commands::mls::ds_post_ok(
        state,
        &ApproveBotHostBody { bot_user_id, request_id: request.request_id.clone(), host_approval: signed },
    )
    .await?;
    Ok(BotHost {
        device_id: request.device_id,
        name: request.device_name.unwrap_or_else(|| "host".to_string()),
        last_seen_at: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// Sign `approval` with this device's ML-DSA-44 key. Public for the flows
/// harness, which forges approvals from devices that should be refused.
pub async fn sign_approval(state: &Arc<AppState>, user_id: &str, device_id: &str, approval: &HostApproval) -> Result<String> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| err("not signed in"))?;
    let provider = crate::commands::mls::PollisProvider::new(db.conn());
    let (key, _) = crate::commands::mls::load_device_pq_signing_key(&provider, user_id, device_id)?;
    Ok(approval.sign(&key))
}

/// Revoke one host of a bot. Any admin of its group; the host's leaf leaves the
/// group in the reconcile that follows, so it cannot read anything sent after.
pub async fn revoke_bot_host(state: &Arc<AppState>, bot_user_id: String, device_id: String) -> Result<()> {
    let (group_id, _bot, _) = bot_and_group(state, &bot_user_id).await?;
    let body = pollis_api::account::RevokeDeviceBody { device_id, user_id: Some(bot_user_id) };
    match crate::commands::mls::ds_post_json(state, &body).await? {
        pollis_api::account::DeviceRevoked::Ok { .. } => {}
        pollis_api::account::DeviceRevoked::NotFound => return Err(err("no such host for this bot")),
    }
    let me = actor(state).await?;
    reconcile_group(state, &group_id, &me).await?;
    Ok(())
}

/// Delete a bot: its account, hosts and key packages (the DS's account-deletion
/// path), then its leaves leave the group.
pub async fn delete_bot(state: &Arc<AppState>, bot_user_id: String) -> Result<()> {
    let (group_id, _bot, _) = bot_and_group(state, &bot_user_id).await?;
    let me = actor(state).await?;
    crate::commands::mls::ds_post_ok(state, &DeleteBotBody { bot_user_id, user_id: Some(me.clone()) }).await?;
    reconcile_group(state, &group_id, &me).await?;
    Ok(())
}
