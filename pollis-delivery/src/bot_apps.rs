//! App approvals (#2005): the DS stores each approval as an opaque record and
//! relays it; it verifies nothing in it and cannot forge one (it holds no
//! admin key). The runtime verifies every record from its own state before it
//! loads an App. Wire types: `pollis_api::bot_apps`. Schema:
//! `000054_bot_app_approval.sql`, which also refuses a skipped or repeated
//! `seq` and any change to a stored record.
//!
//! | Endpoint | Who |
//! |---|---|
//! | `/v1/bots/app/approve` | a current admin of the bot's group |
//! | `/v1/read/bot-apps` | a member of the group (people and the group's bots); reports for admins only |
//! | `/v1/bots/app/report` | one of the bot's hosts, for its own installation |

use axum::{extract::State, response::IntoResponse, response::Response};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::writes::{bad_request, gate_and_parse, ok_response, resolve_actor, RawRequest, WriteOutcome};
use crate::bots::signed_request;
use crate::AppState;

pub use pollis_api::bot_apps::*;


// ---------------------------------------------------------------- approve

/// POST /v1/bots/app/approve — device-signed by a current admin of the bot's group.
pub async fn approve_bot_app(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (parsed, actor, device) = match signed_request::<ApproveBotAppBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    if parsed.user_id.as_deref().is_some_and(|u| u != actor) {
        return Ok(AuthRejection::Forbidden.into_response());
    }
    let conn = state.db.conn().await?;
    match apply_approve_bot_app(&conn, &actor, &device, &parsed).await? {
        Ok(resp) => {
            if resp == ApproveBotAppResponse::Approved {
                // Wake the bot's hosts; they also read the approval on their next job. Best-effort.
                let event = serde_json::json!({ "type": "bot_app_approval", "seq": parsed.seq });
                if let Err(e) =
                    crate::broker::room_send_data(&state, &format!("inbox-{}", parsed.bot_user_id), &event).await
                {
                    tracing::warn!("bot app approval: inbox notify failed (non-fatal): {e}");
                }
                // Other admins' bot pages re-read it.
                crate::bots::notify_admins_bots_changed(&state, &parsed.bot_user_id).await;
                // Members' composers re-read the commands it brings.
                crate::bots::notify_group_bots_changed(&state, &parsed.bot_user_id).await;
            }
            Ok(ok_response::<ApproveBotAppBody>(resp))
        }
        Err(WriteOutcome::Invalid(why)) => Ok(bad_request(why)),
        Err(_) => Ok(AuthRejection::Forbidden.into_response()),
    }
}

/// Store the next approval for `body.bot_user_id`. `Forbidden` unless the
/// signer is a current admin of the bot's group; `Stale` when `seq` is not the
/// next one (the schema refuses it too).
pub async fn apply_approve_bot_app(
    conn: &Connection,
    actor: &str,
    actor_device: &str,
    body: &ApproveBotAppBody,
) -> anyhow::Result<Result<ApproveBotAppResponse, WriteOutcome>> {
    if body.record.is_empty() || body.record.len() > APP_APPROVAL_MAX_RECORD_LEN {
        return Ok(Err(WriteOutcome::Invalid("invalid record")));
    }
    if crate::util::b64_decode(&body.record).is_err() {
        return Ok(Err(WriteOutcome::Invalid("invalid record")));
    }
    if body.manifest.len() < 2
        || body.manifest.len() > APP_MANIFEST_MAX_LEN
        || serde_json::from_str::<serde_json::Value>(&body.manifest).is_err()
    {
        return Ok(Err(WriteOutcome::Invalid("invalid manifest")));
    }
    if body.seq < 1 {
        return Ok(Err(WriteOutcome::Invalid("invalid seq")));
    }
    let tx = crate::db::write_tx(conn).await?;
    let Some(group_id) = crate::bots::bot_group(&tx, &body.bot_user_id).await? else {
        return Ok(Err(WriteOutcome::Forbidden));
    };
    if !crate::groups::is_admin(&tx, &group_id, actor).await? {
        return Ok(Err(WriteOutcome::Forbidden));
    }
    let latest = latest_seq(&tx, &body.bot_user_id).await?;
    if body.seq != latest + 1 {
        return Ok(Ok(ApproveBotAppResponse::Stale { latest }));
    }
    tx.execute(
        "INSERT INTO bot_app_approval \
             (installation_id, seq, kind, record, manifest, approver_user_id, approver_device_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        libsql::params![
            body.bot_user_id.clone(),
            body.seq,
            body.kind.as_str(),
            body.record.clone(),
            body.manifest.clone(),
            actor.to_string(),
            actor_device.to_string(),
        ],
    )
    .await?;
    let meta = serde_json::json!({
        "bot_user_id": body.bot_user_id,
        "seq": body.seq,
        "kind": body.kind.as_str(),
        "approved_by": actor,
    });
    crate::bots::security_event(&tx, actor, "bot_app_approved", Some(actor_device), meta.clone()).await?;
    crate::bots::security_event(&tx, &body.bot_user_id, "bot_app_approved", None, meta).await?;
    tx.commit().await?;
    Ok(Ok(ApproveBotAppResponse::Approved))
}

async fn latest_seq(conn: &Connection, installation_id: &str) -> anyhow::Result<i64> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(seq), 0) FROM bot_app_approval WHERE installation_id = ?1",
            libsql::params![installation_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    })
}

// ---------------------------------------------------------------- read

/// POST /v1/read/bot-apps — device-signed by a member of the group.
pub async fn read_bot_apps(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<BotAppsReadBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    match apply_read_bot_apps(&conn, authed.as_deref(), &parsed).await? {
        Some(apps) => Ok(ok_response::<BotAppsReadBody>(BotAppsReadResponse { apps })),
        None => Ok(AuthRejection::Forbidden.into_response()),
    }
}

/// The latest approval of each bot in the group (or of one), and for an
/// admin each installation's report. `None` unless the caller is a member.
pub async fn apply_read_bot_apps(
    conn: &Connection,
    authed: Option<&str>,
    body: &BotAppsReadBody,
) -> anyhow::Result<Option<Vec<BotAppWire>>> {
    let Ok(actor) = resolve_actor(authed, body.user_id.as_deref()) else {
        return Ok(None);
    };
    let member = crate::util::query_strings(
        conn,
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![body.group_id.clone(), actor.clone()],
    )
    .await?;
    let Some(role) = member.first() else {
        return Ok(None);
    };
    let admin = role == "admin";
    let mut bots = crate::bots::bots_of_group(conn, &body.group_id).await?;
    if let Some(one) = &body.bot_user_id {
        bots.retain(|b| b == one);
    }
    let mut out = Vec::with_capacity(bots.len());
    for bot in bots {
        let mut rows = conn
            .query(
                "SELECT seq, kind, record, manifest, approver_user_id, created_at FROM bot_app_approval \
                 WHERE installation_id = ?1 ORDER BY seq DESC LIMIT 1",
                libsql::params![bot.clone()],
            )
            .await?;
        let approval = match rows.next().await? {
            Some(row) => {
                let kind: String = row.get(1)?;
                let kind = match kind.as_str() {
                    "development" => AppApprovalKind::Development,
                    _ => AppApprovalKind::Production,
                };
                Some(AppApprovalWire {
                    seq: row.get(0)?,
                    kind,
                    record: row.get(2)?,
                    manifest: row.get(3)?,
                    approver_user_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            }
            None => None,
        };
        drop(rows);
        let report = if admin { read_report(conn, &bot).await? } else { None };
        out.push(BotAppWire { bot_user_id: bot, approval, report });
    }
    Ok(Some(out))
}

async fn read_report(conn: &Connection, installation_id: &str) -> anyhow::Result<Option<BotAppReportWire>> {
    let mut rows = conn
        .query(
            "SELECT state, reason, detail, app_hash, manifest, source, developer_key, device_id, updated_at \
             FROM bot_app_report WHERE installation_id = ?1",
            libsql::params![installation_id.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let state: String = row.get(0)?;
    let state = match state.as_str() {
        "running" => BotAppRunState::Running,
        "awaiting_approval" => BotAppRunState::AwaitingApproval,
        _ => BotAppRunState::Refused,
    };
    Ok(Some(BotAppReportWire {
        state,
        reason: row.get(1)?,
        detail: row.get(2)?,
        app_hash: row.get(3)?,
        manifest: row.get(4)?,
        source: row.get(5)?,
        developer_key: row.get(6)?,
        device_id: row.get(7)?,
        updated_at: row.get(8)?,
    }))
}

// ---------------------------------------------------------------- report

/// POST /v1/bots/app/report — device-signed by one of the bot's hosts.
pub async fn report_bot_app(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (parsed, actor, device) = match signed_request::<BotAppReportBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    if parsed.installation_id != actor {
        return Ok(AuthRejection::Forbidden.into_response());
    }
    let conn = state.db.conn().await?;
    let outcome = apply_report_bot_app(&conn, &actor, &device, &parsed).await?;
    if matches!(outcome, WriteOutcome::Ok) {
        // The runtime reports only when its state changed: admins' bot pages re-read it.
        crate::bots::notify_admins_bots_changed(&state, &actor).await;
    }
    crate::writes::outcome_response::<BotAppReportBody>(outcome)
}

fn state_str(state: BotAppRunState) -> &'static str {
    match state {
        BotAppRunState::Running => "running",
        BotAppRunState::AwaitingApproval => "awaiting_approval",
        BotAppRunState::Refused => "refused",
    }
}

/// Replace the installation's report. `Forbidden` unless it is a bot.
pub async fn apply_report_bot_app(
    conn: &Connection,
    installation_id: &str,
    device_id: &str,
    body: &BotAppReportBody,
) -> anyhow::Result<WriteOutcome> {
    if body.reason.len() > 64 || body.detail.chars().count() > 500 {
        return Ok(WriteOutcome::Invalid("invalid reason or detail"));
    }
    if body
        .app_hash
        .as_ref()
        .is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Ok(WriteOutcome::Invalid("invalid app_hash"));
    }
    if body.manifest.as_ref().is_some_and(|m| {
        m.len() < 2 || m.len() > APP_MANIFEST_MAX_LEN || serde_json::from_str::<serde_json::Value>(m).is_err()
    }) {
        return Ok(WriteOutcome::Invalid("invalid manifest"));
    }
    if body.source.as_ref().is_some_and(|s| s.is_empty() || s.len() > 2048)
        || body.developer_key.as_ref().is_some_and(|k| k.is_empty() || k.len() > 4096)
    {
        return Ok(WriteOutcome::Invalid("invalid source or developer_key"));
    }
    if !crate::bots::is_bot(conn, installation_id).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    conn.execute(
        "INSERT INTO bot_app_report \
             (installation_id, state, reason, detail, app_hash, manifest, source, developer_key, device_id, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, CURRENT_TIMESTAMP) \
         ON CONFLICT (installation_id) DO UPDATE SET \
             state = excluded.state, reason = excluded.reason, detail = excluded.detail, \
             app_hash = excluded.app_hash, manifest = excluded.manifest, source = excluded.source, \
             developer_key = excluded.developer_key, device_id = excluded.device_id, \
             updated_at = excluded.updated_at",
        libsql::params![
            installation_id.to_string(),
            state_str(body.state),
            body.reason.clone(),
            body.detail.clone(),
            body.app_hash.clone(),
            body.manifest.clone(),
            body.source.clone(),
            body.developer_key.clone(),
            device_id.to_string(),
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

/// The App rows a bot's deletion removes (called from `bots::purge_bot_rows_of`).
pub async fn purge_bot_app_rows_of(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    let uid = || libsql::params![user_id.to_string()];
    conn.execute("DELETE FROM bot_app_approval WHERE installation_id = ?1", uid()).await?;
    conn.execute("DELETE FROM bot_app_report WHERE installation_id = ?1", uid()).await?;
    Ok(())
}
