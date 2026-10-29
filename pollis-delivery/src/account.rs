//! Account lifecycle, identity rotation, recovery, enrollment approval and the
//! security-audit log. Every table here is on the main DB; the MLS Welcome purge
//! is a separate log-DB write ([`crate::writes::welcomes_purge`]).
//!
//! Every op is self-scoped: [`resolve_actor`] binds the target user to the
//! signer, and every statement is bound `user_id = actor`.
//!
//! `rotate-identity` and `reset-recover` also accept a verified-OTP session,
//! because the soft reset runs from a pre-enrollment device with no signing key.
//! A session-authenticated rotation IS the soft reset: it runs the
//! `reset-recover` wipe in the same transaction, so an OTP alone can only mint a
//! fresh identity that owns nothing (whitepaper §11.5; `tests/reset_session.rs`).
//!
//! Invariants: the `account_key_log` append is a CAS, so concurrent rotations
//! never fork or gap the transparency log; account deletion and identity reset
//! are each one transaction, since a half-deleted account is a corrupt state.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::writes::{
    bad_request,
    gate,
    gate_and_parse,
    gate_or_session_and_parse,
    gate_or_session_kind_and_parse,
    outcome_response,
    resolve_actor,
    GateCredential,
    RawRequest,
    WriteOutcome,
};
use crate::AppState;
use crate::util::b64_decode;

// Request bodies are declared once in `pollis-api`, shared with pollis-core.
pub use pollis_api::account::*;

/// The outcome of an identity rotation. Unlike [`WriteOutcome`] it has a CAS-loss
/// state, which maps to 409.
pub enum RotateOutcome {
    Applied { new_version: i64 },
    /// The signer may not rotate this account (`user_id` ≠ signer).
    Forbidden,
    /// `based_on_version` is not the current head; the client re-reads and retries.
    Conflict { head_version: i64 },
}

/// POST /v1/account/rotate-identity — CAS-guarded rotation of the account
/// identity key (see [`apply_rotate_identity`]).
pub async fn rotate_identity(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, credential, parsed) =
        match gate_or_session_kind_and_parse::<RotateIdentityBody>(&state, &req).await? {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };
    let conn = state.db.conn().await?;
    let (outcome, dead_conversations) =
        apply_rotate_identity(&conn, authed.as_deref(), &parsed, &credential).await?;
    // The account key every device cert chains to changed, and a session wipe
    // dropped other devices: never serve a cached key from before (#658).
    let owner = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()).ok();
    if let Some(owner) = owner.as_deref() {
        state.device_keys.invalidate_user(owner);
    }
    // Log-DB half of a session reset, after the main transaction committed
    // (mirrors `reset_recover` + `/v1/welcomes/purge`).
    if let (RotateOutcome::Applied { .. }, GateCredential::Session { .. }, Some(owner)) =
        (&outcome, &credential, owner.as_deref())
    {
        let log_conn = state.log_db.conn().await?;
        if !dead_conversations.is_empty() {
            crate::teardown::purge_conversation_log(&log_conn, &dead_conversations).await?;
        }
        crate::writes::purge_welcomes(&log_conn, owner).await?;
    }
    rotate_outcome_response::<RotateIdentityBody>(outcome)
}

/// Map a [`RotateOutcome`] to its HTTP response (200 / 403 / 409).
pub(crate) fn rotate_outcome_response<B>(outcome: RotateOutcome) -> Result<Response, AppError>
where
    B: pollis_api::DsRequest<Response = RotateIdentityResponse>,
{
    Ok(match outcome {
        RotateOutcome::Applied { new_version } => crate::writes::ok_response::<B>(
            RotateIdentityResponse::Ok { identity_version: new_version },
        ),
        RotateOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
        RotateOutcome::Conflict { head_version } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "status": "conflict", "head_version": head_version })),
        )
            .into_response(),
    })
}

/// Append `account_key_log` at `based_on_version + 1` IFF that version is the
/// current head, then bump `users.identity_version` and rewrap
/// `account_recovery` in the same transaction. The CAS is the conditional INSERT:
///
/// ```sql
/// INSERT INTO account_key_log (user_id, account_id_pub, identity_version)
/// SELECT ?1, ?2, ?3
/// WHERE ?4 = (SELECT COALESCE(MAX(identity_version), 0)
///             FROM account_key_log WHERE user_id = ?1)
/// ON CONFLICT(user_id, identity_version) DO NOTHING
/// ```
/// (`?3 = based_on_version + 1`, `?4 = based_on_version`.) The loser of a race
/// at the same head inserts nothing and gets `Conflict`.
///
/// Under [`GateCredential::Session`] the [`reset_recover_in_tx`] wipe runs in
/// this same transaction, so an OTP alone can never mint a key that inherits the
/// account. The `identity_rotated` security event is DS-authored, never trusted
/// to the client.
///
/// Returns the conversation ids the session wipe destroyed, for the caller's
/// post-commit commit-log purge.
pub async fn apply_rotate_identity(
    conn: &Connection,
    authed: Option<&str>,
    body: &RotateIdentityBody,
    credential: &GateCredential,
) -> anyhow::Result<(RotateOutcome, Vec<String>)> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok((RotateOutcome::Forbidden, Vec::new())),
    };
    let pub_bytes = b64_decode(&body.account_id_pub)?;
    let salt = b64_decode(&body.salt)?;
    let nonce = b64_decode(&body.nonce)?;
    let wrapped = b64_decode(&body.wrapped_key)?;
    let new_version = body.based_on_version + 1;

    let tx = conn.transaction().await?;

    let affected = tx
        .execute(
            "INSERT INTO account_key_log (user_id, account_id_pub, identity_version) \
             SELECT ?1, ?2, ?3 \
             WHERE ?4 = (SELECT COALESCE(MAX(identity_version), 0) \
                         FROM account_key_log WHERE user_id = ?1) \
             ON CONFLICT(user_id, identity_version) DO NOTHING",
            libsql::params![
                actor.clone(),
                pub_bytes.clone(),
                new_version,
                body.based_on_version
            ],
        )
        .await?;

    if affected == 0 {
        let head = current_key_log_head(&tx, &actor).await?;
        drop(tx);
        return Ok((RotateOutcome::Conflict { head_version: head }, Vec::new()));
    }

    tx.execute(
        "UPDATE users SET account_id_pub = ?1, identity_version = ?2 WHERE id = ?3",
        libsql::params![pub_bytes, new_version, actor.clone()],
    )
    .await?;

    tx.execute(
        "INSERT INTO account_recovery \
         (user_id, identity_version, salt, nonce, wrapped_key, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'), datetime('now')) \
         ON CONFLICT(user_id) DO UPDATE SET \
             identity_version = excluded.identity_version, \
             salt = excluded.salt, \
             nonce = excluded.nonce, \
             wrapped_key = excluded.wrapped_key, \
             updated_at = datetime('now')",
        libsql::params![actor.clone(), new_version, salt, nonce, wrapped],
    )
    .await?;

    let (credential_label, device_id, dead_conversations) = match credential {
        GateCredential::Session { device_id } => {
            let dead = reset_recover_in_tx(&tx, &actor, Some(device_id.as_str())).await?;
            ("session", Some(device_id.clone()), dead)
        }
        GateCredential::Signature { device_id } => {
            ("signature", Some(device_id.clone()), Vec::new())
        }
        GateCredential::None => ("none", None, Vec::new()),
    };

    tx.execute(
        "INSERT INTO security_event (id, user_id, kind, device_id, metadata) \
         VALUES (?1, ?2, 'identity_rotated', ?3, ?4)",
        libsql::params![
            ulid::Ulid::new().to_string(),
            actor,
            device_id,
            format!("credential={credential_label},new_identity_version={new_version}"),
        ],
    )
    .await?;

    tx.commit().await?;
    Ok((RotateOutcome::Applied { new_version }, dead_conversations))
}

async fn current_key_log_head(conn: &Connection, user_id: &str) -> anyhow::Result<i64> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(identity_version), 0) FROM account_key_log WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    })
}

/// POST /v1/security-events — append to the actor's own security-audit log.
pub async fn record_security_event(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<SecurityEventBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<SecurityEventBody>(apply_record_security_event(&conn, authed.as_deref(), &parsed).await?)
}

const SECURITY_EVENT_WINDOW_SECS: i64 = 3600;

/// Client-reported security events one account may append per
/// [`SECURITY_EVENT_WINDOW_SECS`].
///
/// The security-events read serves only the newest 500 rows, so without a cap a
/// device could bury its own recorded action under a flood. Sixty an hour is far
/// above real client volume and far below what shifts that window. DS-authored
/// rows never pass through here.
const SECURITY_EVENT_MAX_PER_WINDOW: i64 = 60;

/// INSERT a `security_event` attributed to the signer, rate-bounded by
/// [`SECURITY_EVENT_MAX_PER_WINDOW`].
pub async fn apply_record_security_event(
    conn: &Connection,
    authed: Option<&str>,
    body: &SecurityEventBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    // Counted from the table so every DS instance and a rolling deploy share one
    // budget (#847).
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM security_event \
             WHERE user_id = ?1 AND datetime(created_at) > datetime('now', ?2)",
            libsql::params![
                actor.clone(),
                format!("-{SECURITY_EVENT_WINDOW_SECS} seconds"),
            ],
        )
        .await?;
    let recent: i64 = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };
    drop(rows);
    if recent >= SECURITY_EVENT_MAX_PER_WINDOW {
        return Ok(WriteOutcome::Forbidden);
    }
    // Shipped clients copy the device name in here, which can embed an IP; no
    // client IP may be stored, so redact rather than refuse.
    let metadata = body.metadata.as_deref().map(crate::util::redact_ip_literals);
    conn.execute(
        "INSERT INTO security_event (id, user_id, kind, device_id, metadata) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![
            ulid::Ulid::new().to_string(),
            actor,
            body.kind.clone(),
            body.device_id.clone(),
            metadata,
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

/// POST /v1/enrollment/approve — flip the actor's own pending enrollment request
/// to `approved`, attaching the wrapped account key. Code, status and expiry are
/// verified client-side by the approving device.
pub async fn approve_enrollment(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<ApproveEnrollmentBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<ApproveEnrollmentBody>(apply_approve_enrollment(&conn, authed.as_deref(), &parsed).await?)
}

/// The `user_id = actor` bind is the authz; `affected == 0` → `Forbidden`.
pub async fn apply_approve_enrollment(
    conn: &Connection,
    authed: Option<&str>,
    body: &ApproveEnrollmentBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let wrapped = b64_decode(&body.wrapped_account_key)?;
    let affected = conn
        .execute(
            "UPDATE device_enrollment_request \
             SET wrapped_account_key = ?1, status = 'approved', approved_by_device_id = ?2 \
             WHERE id = ?3 AND user_id = ?4",
            libsql::params![
                wrapped,
                body.approved_by_device_id.clone(),
                body.request_id.clone(),
                actor,
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(WriteOutcome::Forbidden);
    }
    Ok(WriteOutcome::Ok)
}

/// POST /v1/enrollment/reject — self-scoped like [`approve_enrollment`].
pub async fn reject_enrollment(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RejectEnrollmentBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<RejectEnrollmentBody>(apply_reject_enrollment(&conn, authed.as_deref(), &parsed).await?)
}

pub async fn apply_reject_enrollment(
    conn: &Connection,
    authed: Option<&str>,
    body: &RejectEnrollmentBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let affected = conn
        .execute(
            "UPDATE device_enrollment_request \
             SET status = 'rejected', approved_by_device_id = ?1 \
             WHERE id = ?2 AND user_id = ?3",
            libsql::params![body.approved_by_device_id.clone(), body.request_id.clone(), actor],
        )
        .await?;
    if affected == 0 {
        return Ok(WriteOutcome::Forbidden);
    }
    Ok(WriteOutcome::Ok)
}

/// POST /v1/devices/revoke — tombstone one of the actor's own devices. Removing
/// its leaf from each MLS tree stays client-side (via `/v1/commits`).
pub async fn revoke_device(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RevokeDeviceBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    // Read before the write, so "does it exist?" and "what was it called?" are one
    // answer (#987).
    let device_name = match resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        Ok(owner) => live_device_name(&conn, &owner, &parsed.device_id).await?,
        Err(_) => None,
    };
    let outcome = apply_revoke_device(&conn, authed.as_deref(), &parsed).await?;
    // Revocation must bite on the next request, not at cache TTL (#658).
    // Unconditional: a Forbidden write only costs one re-read.
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        state
            .device_keys
            .invalidate_device(&owner, &parsed.device_id);
        // The commit-log DB cannot join the transaction above.
        if matches!(outcome, WriteOutcome::Ok) {
            let log_conn = state.log_db.conn().await?;
            crate::teardown::purge_device_log_rows(&log_conn, &owner, &parsed.device_id).await?;
            // The SFU checks a token only at join, so sessions this device
            // already holds would outlive the revocation. Kick just this device.
            let rooms = crate::broker::user_rooms(&conn, &owner).await?;
            crate::broker::evict_device_from_rooms(&state, &rooms, &owner, &parsed.device_id).await;
        }
    }
    match outcome {
        WriteOutcome::Forbidden => Ok(AuthRejection::Forbidden.into_response()),
        WriteOutcome::EpochBehind {
            head_generation,
            head_epoch,
        } => Ok(crate::writes::epoch_behind_response(head_generation, head_epoch)),
        WriteOutcome::Invalid(msg) => Ok(crate::writes::bad_request(msg)),
        WriteOutcome::Ok => Ok(crate::writes::ok_response::<RevokeDeviceBody>(
            match device_name {
                Some(name) => DeviceRevoked::Ok {
                    device_name: Some(name),
                },
                // Absent or already tombstoned: the UPDATE was a no-op.
                None => DeviceRevoked::NotFound,
            },
        )),
    }
}

/// The name of a live (non-tombstoned) device; `None` for absent or revoked.
async fn live_device_name(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT device_name FROM user_device \
             WHERE device_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
            libsql::params![device_id.to_string(), user_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        // A live row with a NULL name is still a real device: report the empty
        // string rather than `None`, which the caller reads as "no such device".
        Some(row) => Ok(Some(row.get::<Option<String>>(0).ok().flatten().unwrap_or_default())),
        None => Ok(None),
    }
}

/// DELETE the device's unclaimed key packages and watermarks, then tombstone its
/// row, in one transaction. A revoked device's watermark is a cursor that can
/// never advance, so it is removed here at the chokepoint (#685).
pub async fn apply_revoke_device(
    conn: &Connection,
    authed: Option<&str>,
    body: &RevokeDeviceBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM mls_key_package WHERE user_id = ?1 AND device_id = ?2",
        libsql::params![actor.clone(), body.device_id.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM conversation_watermark WHERE user_id = ?1 AND device_id = ?2",
        libsql::params![actor.clone(), body.device_id.clone()],
    )
    .await?;
    tx.execute(
        "UPDATE user_device SET revoked_at = datetime('now') \
         WHERE device_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
        libsql::params![body.device_id.clone(), actor],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// POST /v1/auth/logout — DELETE the logging-out device's `user_device` row.
///
/// A DELETE, not revoke's tombstone: a tombstone permanently fails the signature
/// gate, but a logged-out device must re-register cleanly on the next sign-in.
pub async fn logout_device(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<LogoutDeviceBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_logout_device(&conn, authed.as_deref(), &parsed).await?;
    // Or the logged-out device keeps authenticating until the cache TTL (#658).
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        state
            .device_keys
            .invalidate_device(&owner, &parsed.device_id);
        // Commit-log DB — see `revoke_device` above.
        if matches!(outcome, WriteOutcome::Ok) {
            let log_conn = state.log_db.conn().await?;
            crate::teardown::purge_device_log_rows(&log_conn, &owner, &parsed.device_id).await?;
        }
    }
    outcome_response::<LogoutDeviceBody>(outcome)
}

/// DELETE the device row and its per-device state. Idempotent, since a re-tried
/// logout must never error. Leftover key packages would be claimable by a peer
/// for a device that no longer exists, and no cascade reaches them.
pub async fn apply_logout_device(
    conn: &Connection,
    authed: Option<&str>,
    body: &LogoutDeviceBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM mls_key_package WHERE user_id = ?1 AND device_id = ?2",
        libsql::params![actor.clone(), body.device_id.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM conversation_watermark WHERE user_id = ?1 AND device_id = ?2",
        libsql::params![actor.clone(), body.device_id.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM user_device WHERE device_id = ?1 AND user_id = ?2",
        libsql::params![body.device_id.clone(), actor],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// POST /v1/account/reset-recover — the membership/device wipe that follows an
/// identity reset, as one transaction. The Welcome purge is `/v1/welcomes/purge`.
pub async fn reset_recover(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_or_session_and_parse::<ResetRecoverBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let (outcome, dead_conversations) =
        apply_reset_recover(&conn, authed.as_deref(), &parsed).await?;
    // Account-wide device wipe → evict the whole user (#658).
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        state.device_keys.invalidate_user(&owner);
    }
    if matches!(outcome, WriteOutcome::Ok) && !dead_conversations.is_empty() {
        let log_conn = state.log_db.conn().await?;
        crate::teardown::purge_conversation_log(&log_conn, &dead_conversations).await?;
    }
    outcome_response::<ResetRecoverBody>(outcome)
}

/// All of identity-reset's main-DB cleanup, in one transaction. Not account
/// deletion: the `users` row and the account's own records survive; only
/// membership and device state is shed. Returns the destroyed conversation ids.
pub async fn apply_reset_recover(
    conn: &Connection,
    authed: Option<&str>,
    body: &ResetRecoverBody,
) -> anyhow::Result<(WriteOutcome, Vec<String>)> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok((o, Vec::new())),
    };
    let tx = conn.transaction().await?;
    let dead_conversations =
        reset_recover_in_tx(&tx, &actor, body.current_device_id.as_deref()).await?;
    tx.commit().await?;
    Ok((WriteOutcome::Ok, dead_conversations))
}

/// The identity-reset wipe, on an open transaction the caller commits. Must stay
/// idempotent: clients still call `reset-recover` after a session rotation that
/// already ran it. `current_device_id` is the device to keep; `None` drops all.
async fn reset_recover_in_tx(
    tx: &Connection,
    actor: &str,
    current_device_id: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let actor = actor.to_string();

    // Before the membership rows go — see `apply_delete_account`.
    let mut dm_ids: Vec<String> = Vec::new();
    {
        let mut rows = tx
            .query(
                "SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?1",
                libsql::params![actor.clone()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            dm_ids.push(row.get(0)?);
        }
    }

    let mut dead_conversations = handoff_group_ownership(tx, &actor).await?;

    tx.execute(
        "DELETE FROM group_member WHERE user_id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;
    // Every channel mute (#1431) hung on a membership that just went.
    tx.execute(
        "DELETE FROM channel_mute WHERE user_id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM dm_channel_member WHERE user_id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM mls_key_package WHERE user_id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;

    match current_device_id {
        Some(dev) => {
            tx.execute(
                "DELETE FROM user_device WHERE user_id = ?1 AND device_id != ?2",
                libsql::params![actor.clone(), dev.to_string()],
            )
            .await?;
        }
        None => {
            tx.execute(
                "DELETE FROM user_device WHERE user_id = ?1",
                libsql::params![actor.clone()],
            )
            .await?;
        }
    }

    match current_device_id {
        Some(dev) => {
            tx.execute(
                "DELETE FROM conversation_watermark WHERE user_id = ?1 AND device_id != ?2",
                libsql::params![actor.clone(), dev.to_string()],
            )
            .await?;
        }
        None => {
            tx.execute(
                "DELETE FROM conversation_watermark WHERE user_id = ?1",
                libsql::params![actor.clone()],
            )
            .await?;
        }
    }

    for dm_id in &dm_ids {
        let remaining: i64 = {
            let mut rows = tx
                .query(
                    "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = ?1",
                    libsql::params![dm_id.clone()],
                )
                .await?;
            match rows.next().await? {
                Some(row) => row.get(0)?,
                None => 0,
            }
        };
        if remaining == 0 {
            crate::teardown::purge_dm_channel(tx, dm_id).await?;
            dead_conversations.push(dm_id.clone());
        }
    }

    Ok(dead_conversations)
}

/// POST /v1/account/delete — permanently delete the actor's account in one
/// transaction, with group-ownership handoff and teardown of emptied DMs. No
/// cascade is relied on: Turso runs `foreign_keys=OFF` (see [`crate::teardown`]).
/// Commit-log rows are purged after the commit.
pub async fn delete_account(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &req).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: DeleteAccountBody = if req.body.is_empty() {
        DeleteAccountBody { user_id: None }
    } else {
        match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(_) => return Ok(bad_request("invalid body")),
        }
    };
    let conn = state.db.conn().await?;
    let (outcome, dead_conversations) =
        apply_delete_account(&conn, authed.as_deref(), &parsed).await?;
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        // Every cached key of this user is stale (#658).
        state.device_keys.invalidate_user(&owner);
        // Only after the main commit — see `teardown::purge_conversation_log`.
        if matches!(outcome, WriteOutcome::Ok) {
            let log_conn = state.log_db.conn().await?;
            crate::writes::purge_welcomes(&log_conn, &owner).await?;
            crate::teardown::purge_user_log_rows(&log_conn, &owner).await?;
            crate::teardown::purge_conversation_log(&log_conn, &dead_conversations).await?;
        }
    }
    outcome_response::<DeleteAccountBody>(outcome)
}

/// Every main-DB delete of account deletion, in one transaction. Returns the
/// destroyed conversation ids for the caller's post-commit log purge.
pub async fn apply_delete_account(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteAccountBody,
) -> anyhow::Result<(WriteOutcome, Vec<String>)> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok((o, Vec::new())),
    };
    let tx = conn.transaction().await?;

    // Before the membership rows go: afterwards nothing identifies the actor's
    // DMs, and an emptied DM would keep its ciphertext forever.
    let mut dm_ids: Vec<String> = Vec::new();
    {
        let mut rows = tx
            .query(
                "SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?1",
                libsql::params![actor.clone()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            dm_ids.push(row.get(0)?);
        }
    }

    let mut dead_conversations = handoff_group_ownership(&tx, &actor).await?;
    crate::teardown::purge_user_rows(&tx, &actor).await?;

    for dm_id in &dm_ids {
        let remaining: i64 = {
            let mut rows = tx
                .query(
                    "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = ?1",
                    libsql::params![dm_id.clone()],
                )
                .await?;
            match rows.next().await? {
                Some(row) => row.get(0)?,
                None => 0,
            }
        };
        if remaining == 0 {
            crate::teardown::purge_dm_channel(&tx, dm_id).await?;
            dead_conversations.push(dm_id.clone());
        }
    }

    tx.execute(
        "DELETE FROM users WHERE id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;

    tx.commit().await?;
    Ok((WriteOutcome::Ok, dead_conversations))
}

/// For every group the actor is in: tear it down if they are the sole member,
/// else promote a successor if they are the sole admin. Returns the destroyed
/// conversation ids.
async fn handoff_group_ownership(conn: &Connection, actor: &str) -> anyhow::Result<Vec<String>> {
    let mut dead_conversations: Vec<String> = Vec::new();
    let mut memberships: Vec<(String, String)> = Vec::new();
    {
        let mut rows = conn
            .query(
                "SELECT group_id, role FROM group_member WHERE user_id = ?1",
                libsql::params![actor.to_string()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            memberships.push((row.get(0)?, row.get(1)?));
        }
    }

    for (gid, role) in &memberships {
        let member_count: i64 = {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM group_member WHERE group_id = ?1",
                    libsql::params![gid.clone()],
                )
                .await?;
            match rows.next().await? {
                Some(row) => row.get(0)?,
                None => 0,
            }
        };

        if member_count <= 1 {
            let channels = crate::teardown::purge_group(conn, gid).await?;
            dead_conversations.extend(channels);
            dead_conversations.push(gid.clone());
        } else if role == "admin" {
            let other_admins: i64 = {
                let mut rows = conn
                    .query(
                        "SELECT COUNT(*) FROM group_member \
                         WHERE group_id = ?1 AND role = 'admin' AND user_id != ?2",
                        libsql::params![gid.clone(), actor.to_string()],
                    )
                    .await?;
                match rows.next().await? {
                    Some(row) => row.get(0)?,
                    None => 0,
                }
            };
            if other_admins == 0 {
                let candidate: Option<String> = {
                    let mut rows = conn
                        .query(
                            "SELECT user_id FROM group_member \
                             WHERE group_id = ?1 AND user_id != ?2 LIMIT 1",
                            libsql::params![gid.clone(), actor.to_string()],
                        )
                        .await?;
                    match rows.next().await? {
                        Some(row) => Some(row.get(0)?),
                        None => None,
                    }
                };
                if let Some(new_admin) = candidate {
                    conn.execute(
                        "UPDATE group_member SET role = 'admin' \
                         WHERE group_id = ?1 AND user_id = ?2",
                        libsql::params![gid.clone(), new_admin.clone()],
                    )
                    .await?;
                    // `owner_id` has no foreign key; left alone it would keep a
                    // deleted user's id.
                    conn.execute(
                        "UPDATE groups SET owner_id = ?1 \
                         WHERE id = ?2 AND owner_id = ?3",
                        libsql::params![new_admin, gid.clone(), actor.to_string()],
                    )
                    .await?;
                }
            }
        }
    }
    Ok(dead_conversations)
}
