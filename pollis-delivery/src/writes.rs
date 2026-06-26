//! The MLS control-plane write endpoints beyond `POST /v1/commits`.
//!
//! These cover the non-`direct_submit` client writes (W4–W8 in
//! `docs/goal-a-commit-log-sole-writer.md`) that must move behind the DS once
//! the client holds only a read-only token on the log DB:
//!
//!   - `POST /v1/group-info`     — republish GroupInfo (W4), epoch-monotone.
//!   - `POST /v1/welcomes/ack`   — mark Welcomes delivered (W5).
//!   - `POST /v1/welcomes/reset` — re-arm Welcomes for redelivery (W6/W7).
//!   - `POST /v1/welcomes/purge` — delete all of a user's Welcomes (W8).
//!
//! Every endpoint is gated by `require_auth` exactly like `/v1/commits`: when
//! enforced, the request is device-signature-verified and the authenticated
//! user must equal the actor/owner the write targets. All writes land on
//! `state.log_db`; membership/auth lookups read `state.db` (the main DB, where
//! `user_device` and group/DM membership live).

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use libsql::Connection;
use serde::Deserialize;

use crate::auth;
use crate::error::{AppError, AuthRejection};
use crate::AppState;

// ── Shared auth gate ─────────────────────────────────────────────────────────

/// The outcome of the shared auth gate: either an authenticated user (auth
/// enforced + verified), or `None` (auth disabled — the no-auth path).
type Authed = Option<String>;

/// Run the same auth gate as `submit`: when `require_auth` is on, verify the
/// device signature over the raw body and return the authenticated `user_id`;
/// when off, return `None` (the no-auth test/pre-cutover path).
///
/// Returns `Ok(Err(response))` when auth is enforced but the request is
/// rejected — the caller forwards that response unchanged.
async fn gate(
    state: &AppState,
    headers: &HeaderMap,
    method: &Method,
    uri: &Uri,
    body: &Bytes,
) -> Result<Result<Authed, Response>, AppError> {
    if !state.require_auth {
        return Ok(Ok(None));
    }
    let conn = state.db.conn()?;
    match auth::verify_request(
        &conn,
        headers,
        method.as_str(),
        uri.path(),
        body,
        auth::now_unix(),
    )
    .await
    {
        Ok(user_id) => Ok(Ok(Some(user_id))),
        Err(rej) => Ok(Err(rej.into_response())),
    }
}

/// Resolve the recipient/owner a welcome op targets.
///
///   - auth ON  → the authenticated user. If the body also carries `user_id`,
///                it must equal the authenticated user (else 403).
///   - auth OFF → the body's `user_id` (the no-auth path has no signed identity,
///                so the recipient must be supplied explicitly). Missing/empty
///                → 400.
fn resolve_recipient(authed: Authed, body_user_id: Option<String>) -> Result<String, Response> {
    match authed {
        Some(user) => {
            if let Some(b) = body_user_id {
                if b != user {
                    return Err(AuthRejection::Forbidden.into_response());
                }
            }
            Ok(user)
        }
        None => match body_user_id {
            Some(b) if !b.is_empty() => Ok(b),
            _ => Err(bad_request("user_id required when auth is disabled")),
        },
    }
}

fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

fn ok_json(value: serde_json::Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

/// Membership check against the main DB: is `user_id` a current member of the
/// MLS conversation `conversation_id`?
///
/// An MLS `conversation_id` is one of three things, depending on the surface
/// that created the group, so all three must be accepted:
///   - a **group id** — a group's text channels share one MLS group keyed by the
///     group id (membership via `group_member.group_id = conversation_id`);
///   - a **DM channel id** (membership via `dm_channel_member`);
///   - a **channel id** — e.g. a voice channel's own MLS group (membership via
///     the channel's owning group).
///
/// Takes a bare [`Connection`] (the main DB) so the same gate is reusable from
/// the integration harness, which drives a single shared connection rather than
/// the DS's `AppState`.
pub async fn is_member(
    conn: &Connection,
    conversation_id: &str,
    user_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 WHERE \
                EXISTS (SELECT 1 FROM dm_channel_member \
                        WHERE dm_channel_id = ?1 AND user_id = ?2) \
             OR EXISTS (SELECT 1 FROM group_member \
                        WHERE group_id = ?1 AND user_id = ?2) \
             OR EXISTS (SELECT 1 FROM channels c \
                        JOIN group_member gm ON gm.group_id = c.group_id \
                        WHERE c.id = ?1 AND gm.user_id = ?2) \
             LIMIT 1",
            libsql::params![conversation_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

// ── W4 — POST /v1/group-info ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct GroupInfoBody {
    pub conversation_id: String,
    pub epoch: i64,
    /// TLS-serialized MLS GroupInfo, base64 (STANDARD).
    pub group_info: String,
    pub updated_by_device_id: String,
}

/// POST /v1/group-info — republish GroupInfo for a conversation, epoch-monotone
/// (an older epoch can never clobber a newer one). When auth is enforced, the
/// authenticated user must be a current member of `conversation_id`.
pub async fn group_info(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };

    let parsed: GroupInfoBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };

    // Authz: a signed request may only republish for a conversation it belongs
    // to. Skipped on the no-auth path (mirrors submit).
    if let Some(user_id) = &authed {
        let conn = state.db.conn()?;
        if !is_member(&conn, &parsed.conversation_id, user_id).await? {
            return Ok(AuthRejection::Forbidden.into_response());
        }
    }

    let gi = match b64_decode(&parsed.group_info) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid group_info")),
    };

    let conn = state.log_db.conn()?;
    upsert_group_info(
        &conn,
        &parsed.conversation_id,
        parsed.epoch,
        &gi,
        &parsed.updated_by_device_id,
    )
    .await?;

    Ok(ok_json(serde_json::json!({ "status": "ok" })))
}

/// Decode a parsed [`GroupInfoBody`]'s base64 GroupInfo and UPSERT it (the W4
/// write minus auth/authz). Exposed so the integration harness reuses the exact
/// decode + write without re-implementing base64 handling. A decode failure is
/// an `Err` here (the axum handler maps the same case to 400 itself, before
/// calling [`upsert_group_info`], so its 400-vs-500 behavior is unchanged).
pub async fn apply_group_info(log_conn: &Connection, body: &GroupInfoBody) -> anyhow::Result<u64> {
    let gi = b64_decode(&body.group_info)?;
    upsert_group_info(
        log_conn,
        &body.conversation_id,
        body.epoch,
        &gi,
        &body.updated_by_device_id,
    )
    .await
}

/// Epoch-monotone UPSERT of a conversation's GroupInfo (W4). An older epoch can
/// never clobber a newer one. Pure conn-level write so the integration harness
/// can reuse it against its shared log connection.
pub async fn upsert_group_info(
    log_conn: &Connection,
    conversation_id: &str,
    epoch: i64,
    group_info: &[u8],
    updated_by_device_id: &str,
) -> anyhow::Result<u64> {
    let affected = log_conn
        .execute(
            "INSERT INTO mls_group_info (conversation_id, epoch, group_info, updated_by_device_id) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(conversation_id) DO UPDATE SET \
                 epoch = excluded.epoch, \
                 group_info = excluded.group_info, \
                 updated_by_device_id = excluded.updated_by_device_id, \
                 updated_at = datetime('now') \
             WHERE excluded.epoch > mls_group_info.epoch",
            libsql::params![
                conversation_id.to_string(),
                epoch,
                group_info.to_vec(),
                updated_by_device_id.to_string(),
            ],
        )
        .await?;
    Ok(affected)
}

// ── W5 — POST /v1/welcomes/ack ───────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AckBody {
    pub welcome_ids: Vec<String>,
    /// Recipient, used ONLY on the no-auth path; when auth is on it must equal
    /// the authenticated user (or be absent).
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/welcomes/ack — mark the given Welcomes delivered, scoped to the
/// authenticated recipient so a user can only ack their own Welcomes.
pub async fn welcomes_ack(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };

    let parsed: AckBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };

    let recipient = match resolve_recipient(authed, parsed.user_id) {
        Ok(r) => r,
        Err(resp) => return Ok(resp),
    };

    let conn = state.log_db.conn()?;
    let updated = ack_welcomes(&conn, &recipient, &parsed.welcome_ids).await?;

    Ok(ok_json(serde_json::json!({ "status": "ok", "updated": updated })))
}

/// Mark the given Welcomes `delivered = 1`, scoped to `recipient` so a user can
/// only ack their own Welcomes (W5). Pure conn-level write reused by the harness.
pub async fn ack_welcomes(
    log_conn: &Connection,
    recipient: &str,
    welcome_ids: &[String],
) -> anyhow::Result<u64> {
    if welcome_ids.is_empty() {
        return Ok(0);
    }

    // `id IN (?2, ?3, …)` with the recipient bound first so the filter can never
    // touch another user's Welcomes.
    let placeholders = (2..=welcome_ids.len() + 1)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "UPDATE mls_welcome SET delivered = 1 \
         WHERE recipient_id = ?1 AND id IN ({placeholders})"
    );
    let mut params: Vec<libsql::Value> = Vec::with_capacity(welcome_ids.len() + 1);
    params.push(libsql::Value::Text(recipient.to_string()));
    for id in welcome_ids {
        params.push(libsql::Value::Text(id.clone()));
    }

    Ok(log_conn.execute(&sql, libsql::params_from_iter(params)).await?)
}

// ── W6/W7 — POST /v1/welcomes/reset ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct ResetBody {
    /// `Some` → reset only this device's (and device-agnostic) Welcomes (W6);
    /// `None` → reset all of the recipient's Welcomes (W7).
    #[serde(default)]
    pub device_id: Option<String>,
    /// Recipient, used ONLY on the no-auth path (see [`resolve_recipient`]).
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/welcomes/reset — re-arm Welcomes for redelivery (set `delivered=0`),
/// scoped to the authenticated recipient.
pub async fn welcomes_reset(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };

    let parsed: ResetBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };

    let recipient = match resolve_recipient(authed, parsed.user_id) {
        Ok(r) => r,
        Err(resp) => return Ok(resp),
    };

    let conn = state.log_db.conn()?;
    let updated = reset_welcomes(&conn, &recipient, parsed.device_id.as_deref()).await?;

    Ok(ok_json(serde_json::json!({ "status": "ok", "updated": updated })))
}

/// Re-arm Welcomes for redelivery (`delivered = 0`) for `recipient` (W6/W7).
/// `device_id` `Some` → device-scoped (W6); `None` → all the recipient's
/// Welcomes (W7). Pure conn-level write reused by the harness.
pub async fn reset_welcomes(
    log_conn: &Connection,
    recipient: &str,
    device_id: Option<&str>,
) -> anyhow::Result<u64> {
    Ok(log_conn
        .execute(
            "UPDATE mls_welcome SET delivered = 0 \
             WHERE recipient_id = ?1 \
               AND (?2 IS NULL OR recipient_device_id = ?2 OR recipient_device_id IS NULL)",
            libsql::params![recipient.to_string(), device_id.map(|s| s.to_string())],
        )
        .await?)
}

// ── W8 — POST /v1/welcomes/purge ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PurgeBody {
    /// Recipient, used ONLY on the no-auth path (see [`resolve_recipient`]).
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/welcomes/purge — delete all of the authenticated user's Welcomes
/// (identity-reset cleanup). Recipient is derived from auth; the body carries an
/// explicit `user_id` only on the no-auth path.
pub async fn welcomes_purge(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };

    // An empty body is valid; tolerate it when auth is on (recipient from auth).
    let parsed: PurgeBody = if body.is_empty() {
        PurgeBody { user_id: None }
    } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return Ok(bad_request("invalid body")),
        }
    };

    let recipient = match resolve_recipient(authed, parsed.user_id) {
        Ok(r) => r,
        Err(resp) => return Ok(resp),
    };

    let conn = state.log_db.conn()?;
    let deleted = purge_welcomes(&conn, &recipient).await?;

    Ok(ok_json(serde_json::json!({ "status": "ok", "deleted": deleted })))
}

/// Delete all of `recipient`'s Welcomes (W8, identity-reset cleanup). Pure
/// conn-level write reused by the harness.
pub async fn purge_welcomes(log_conn: &Connection, recipient: &str) -> anyhow::Result<u64> {
    Ok(log_conn
        .execute(
            "DELETE FROM mls_welcome WHERE recipient_id = ?1",
            libsql::params![recipient.to_string()],
        )
        .await?)
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn b64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.decode(s)?)
}
