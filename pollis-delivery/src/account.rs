//! Domains E + G — account lifecycle, identity rotation, recovery, and the
//! security-audit log.
//!
//! The riskiest write surface in #419: the account-key transparency log, wipes, enrollment, audit.
//!
//! ## Authorization — every op is SELF-scoped
//!
//! A user acts only on THEIR OWN account: [`resolve_actor`] binds the target; every statement is scoped.
//!
//! **Credential**: device signature, except `rotate-identity` and `reset-recover`, which take a session too.
//!
//! **A session-authenticated rotation IS the soft reset:** the `reset-recover` wipe rides in its transaction.
//!
//! ## The two hard requirements
//!
//! 1. **`account_key_log` CAS.** One conditional append gates the bump and rewrap; a racer gets `Conflict`.
//!
//! 2. **`delete_account` / `reset_identity_and_recover` are each ONE transaction.**

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

// Request bodies live in `pollis-api`, shared with pollis-core; re-exported here.
pub use pollis_api::account::*;

/// The outcome of an identity rotation; a CAS loss is a 409.
pub enum RotateOutcome {
    /// The rotation won its version. `new_version` is the version now recorded.
    Applied { new_version: i64 },
    /// The signer may not rotate this account (`user_id` ≠ signer).
    Forbidden,
    /// The head moved past `based_on_version`; the client re-reads and retries. No fork.
    Conflict { head_version: i64 },
}

/// POST /v1/account/rotate-identity — CAS-guarded, ONE transaction (see the module docs).
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
    // Evict the user (#658): every device cert chains to the rotated key.
    let owner = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()).ok();
    if let Some(owner) = owner.as_deref() {
        state.device_keys.invalidate_user(owner);
    }
    // A session rotation is the reset, so its commit-log half runs here too.
    if let (RotateOutcome::Applied { .. }, GateCredential::Session { .. }, Some(owner)) =
        (&outcome, &credential, owner.as_deref())
    {
        reset_log_half(&state, owner, &dead_conversations).await?;
    }
    rotate_outcome_response::<RotateIdentityBody>(outcome)
}

/// The commit-log-DB half of an identity reset, run once the main transaction
/// has committed: drop the MLS state of the conversations the wipe destroyed
/// and delete every pending Welcome addressed to the account. Those Welcomes
/// were made for key packages and devices the wipe just removed.
///
/// The two databases share no transaction. An error here fails the request
/// (5xx) even though the main-DB half has applied, so the reset is never
/// reported complete with Welcomes left behind. Both halves are idempotent: a
/// retried `reset-recover` reruns them, and a retried reset rotates again from
/// the account's new head and reruns them. `POST /v1/welcomes/purge` stays for
/// clients that still send it; after this it deletes nothing.
async fn reset_log_half(
    state: &AppState,
    owner: &str,
    dead_conversations: &[String],
) -> anyhow::Result<()> {
    let log_conn = state.log_db.conn().await?;
    if !dead_conversations.is_empty() {
        crate::teardown::purge_conversation_log(&log_conn, dead_conversations).await?;
    }
    crate::writes::purge_welcomes(&log_conn, owner).await?;
    Ok(())
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
/// `account_recovery` in the SAME transaction. The CAS is the conditional INSERT
/// — modeled on [`crate::commit::submit_commit`]:
///
/// ```sql
/// INSERT INTO account_key_log (user_id, account_id_pub, identity_version)
/// SELECT ?1, ?2, ?3
/// WHERE ?4 = (SELECT COALESCE(MAX(identity_version), 0)
///             FROM account_key_log WHERE user_id = ?1)
/// ON CONFLICT(user_id, identity_version) DO NOTHING
/// ```
/// (`?3 = based_on_version + 1`, `?4 = based_on_version`.)
///
/// One of two racing rotations appends; the other inserts nothing → `Conflict`.
///
/// A [`GateCredential::Session`] rotation also runs [`reset_recover_in_tx`]; all write `identity_rotated`.
///
/// Returns the conversation ids the session wipe destroyed.
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

    // The atomic CAS: append at `new_version` only if `based_on_version` is the
    // current head of THIS user's account_key_log.
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
        // Lost the race (or a stale based_on_version). Report the real head so
        // the client re-reads and retries; nothing was written.
        let head = current_key_log_head(&tx, &actor).await?;
        drop(tx);
        return Ok((RotateOutcome::Conflict { head_version: head }, Vec::new()));
    }

    // Won the version. Bump the live identity + rewrap the recovery blob, both
    // bound to `new_version` so the three rows can never disagree.
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

    // The credential decides the rest of the transaction — see the fn docs.
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

/// The current head of a user's `account_key_log` = `MAX(identity_version)` (0
/// for a user with no log rows yet). `&Transaction` derefs to `&Connection`.
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

/// POST /v1/security-events — append a row to the actor's own security-audit
/// log (`security_event`). Self-scoped: the row's `user_id` is the signer.
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

/// How far back [`SECURITY_EVENT_MAX_PER_WINDOW`] counts.
const SECURITY_EVENT_WINDOW_SECS: i64 = 3600;

/// Client-reported security events one account may append per
/// [`SECURITY_EVENT_WINDOW_SECS`].
///
/// Unbounded appends could bury events in a newest-first log; DS-authored rows bypass this cap.
const SECURITY_EVENT_MAX_PER_WINDOW: i64 = 60;

/// INSERT a `security_event` attributed to the signer, rate-bounded.
pub async fn apply_record_security_event(
    conn: &Connection,
    authed: Option<&str>,
    body: &SecurityEventBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    // Counted from the table, not memory (#847's durable-budget pattern).
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
    // Client free text can embed an IP (device names); redact rather than refuse.
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

/// POST /v1/enrollment/approve — flip the actor's OWN pending request to `approved`.
pub async fn approve_enrollment(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (identity, parsed) = match crate::writes::gate_identity_and_parse::<ApproveEnrollmentBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    // #1735: the new device reads `approved_by_device_id` (a display hint for
    // the history-copy offer); it must name the device that signed the approval.
    if let Some((_, signer)) = &identity {
        if *signer != parsed.approved_by_device_id {
            return Ok(AuthRejection::Forbidden.into_response());
        }
    }
    let authed = identity.map(|(user, _)| user);
    let conn = state.db.conn().await?;
    outcome_response::<ApproveEnrollmentBody>(apply_approve_enrollment(&conn, authed.as_deref(), &parsed).await?)
}

/// UPDATE `WHERE id = ? AND user_id = actor`; zero rows → `Forbidden`.
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

/// POST /v1/enrollment/reject — flip a pending enrollment request the actor OWNS
/// to `rejected`. Self-scoped like [`approve_enrollment`].
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

/// UPDATE the request to `rejected` `WHERE id = ? AND user_id = actor`.
/// `affected == 0` → not the actor's request → `Forbidden`.
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

/// POST /v1/devices/revoke — drop unclaimed key packages and tombstone the row (self-scoped).
pub async fn revoke_device(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RevokeDeviceBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    // The device's name BEFORE the write (#987).
    let device_name = match resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        Ok(owner) => live_device_name(&conn, &owner, &parsed.device_id).await?,
        Err(_) => None,
    };
    let outcome = apply_revoke_device(&conn, authed.as_deref(), &parsed).await?;
    // Revocation must bite on the NEXT request (#658); evicting on Forbidden costs a re-read.
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        state
            .device_keys
            .invalidate_device(&owner, &parsed.device_id);
        // The device's commit catch-up high-water lives on the commit-log DB and
        // so cannot join the transaction above.
        if matches!(outcome, WriteOutcome::Ok) {
            let log_conn = state.log_db.conn().await?;
            crate::teardown::purge_device_log_rows(&log_conn, &owner, &parsed.device_id).await?;
            // The SFU checks tokens only at JOIN, so kick this device's existing sessions.
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
                // Absent or already tombstoned: report that, not a revocation.
                None => DeviceRevoked::NotFound,
            },
        )),
    }
}

/// The name of a LIVE (non-tombstoned) device on `user_id`'s account.
///
/// `None` covers both "no such device" and "already revoked".
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

/// Delete a device's unclaimed key packages, its conversation watermarks (#685: cursors
/// that can never advance) and its history transfers (#1735: it is party to none, in
/// either role), all scoped to `actor`.
async fn purge_device_state(tx: &Connection, actor: &str, device_id: &str) -> anyhow::Result<()> {
    tx.execute(
        "DELETE FROM mls_key_package WHERE user_id = ?1 AND device_id = ?2",
        libsql::params![actor, device_id],
    )
    .await?;
    tx.execute(
        "DELETE FROM conversation_watermark WHERE user_id = ?1 AND device_id = ?2",
        libsql::params![actor, device_id],
    )
    .await?;
    crate::history_transfer::purge_device(tx, actor, device_id).await?;
    Ok(())
}

/// The DMs `actor` is a member of.
async fn dm_ids_of(tx: &Connection, actor: &str) -> anyhow::Result<Vec<String>> {
    let mut dm_ids: Vec<String> = Vec::new();
    let mut rows = tx
        .query(
            "SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?1",
            libsql::params![actor],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        dm_ids.push(row.get(0)?);
    }
    Ok(dm_ids)
}

/// Tear down each of `dm_ids` left with no members at all, recording it in `dead`.
async fn purge_emptied_dms(
    tx: &Connection,
    dm_ids: &[String],
    dead: &mut Vec<String>,
) -> anyhow::Result<()> {
    for dm_id in dm_ids {
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
            dead.push(dm_id.clone());
        }
    }
    Ok(())
}

/// DELETE the revoked device's unclaimed key packages and its conversation
/// watermarks, then tombstone its row — one transaction, all `WHERE user_id =
/// actor`.
///
/// Deleting the watermarks (#685) removes cursors that can never advance.
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
    // #1874: the device's leaves must go before anyone seals to them again.
    crate::directory::bump_membership_versions_of(&tx, &actor).await?;
    purge_device_state(&tx, &actor, &body.device_id).await?;
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
/// DEVICE-SIGNED and self-scoped: the signer can only remove a device on their
/// OWN account (`WHERE user_id = actor`).
///
/// A DELETE, NOT a tombstone, so the next sign-in re-registers cleanly.
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
    // The row is gone; the cached key must go with it (#658), or the logged-out
    // device would keep authenticating until the TTL lapsed.
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

/// DELETE the device row and its per-device state, scoped to the actor; idempotent.
///
/// Key-package and watermark deletes, as in [`apply_revoke_device`]; no cascade reaches them.
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
    // #1874: the device's leaves must go before anyone seals to them again.
    crate::directory::bump_membership_versions_of(&tx, &actor).await?;
    purge_device_state(&tx, &actor, &body.device_id).await?;
    tx.execute(
        "DELETE FROM user_device WHERE device_id = ?1 AND user_id = ?2",
        libsql::params![body.device_id.clone(), actor],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// POST /v1/account/reset-recover — the post-reset membership/device wipe, as ONE
/// transaction, then the account's pending Welcomes ([`reset_log_half`]).
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
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        // Account-wide device wipe → evict the whole user (#658).
        state.device_keys.invalidate_user(&owner);
        if matches!(outcome, WriteOutcome::Ok) {
            reset_log_half(&state, &owner, &dead_conversations).await?;
        }
    }
    outcome_response::<ResetRecoverBody>(outcome)
}

/// All of identity-reset's main-DB cleanup, in one transaction, self-scoped.
///
/// NOT account deletion: the account's own records survive; only membership and devices go.
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

/// The identity-reset wipe on an OPEN transaction; shared with [`apply_rotate_identity`]. Idempotent.
///
/// `current_device_id` is the row to KEEP; `None` drops them all.
async fn reset_recover_in_tx(
    tx: &Connection,
    actor: &str,
    current_device_id: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let actor = actor.to_string();

    // Note the actor's DMs before their membership rows go — see
    // `apply_delete_account` for why this has to happen first.
    let dm_ids = dm_ids_of(tx, &actor).await?;
    // #1874: every conversation it was in re-checks its tree before the next seal.
    crate::directory::bump_membership_versions_of(tx, &actor).await?;

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

    // #1735: the account's other devices are going, so no transfer survives.
    crate::history_transfer::purge_transfers_of_user(tx, &actor).await?;

    // Orphan the actor's OTHER devices.
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

    // Per-device cursors belong to devices that no longer exist. Keyed on the
    // surviving device so the kept one's cursor is preserved.
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

    // A DM the actor has just left with nobody else in it is dead.
    purge_emptied_dms(tx, &dm_ids, &mut dead_conversations).await?;

    Ok(dead_conversations)
}

/// POST /v1/account/delete — permanently delete the actor's account, as ONE
/// transaction. Handles group ownership (delete empty groups / promote a sole
/// admin's successor), tears down any DM left with nobody in it, and deletes
/// every main-DB row naming the actor via [`crate::teardown::purge_user_rows`].
///
/// Nothing relies on `ON DELETE CASCADE` (FKs are off in production); see [`crate::teardown`].
///
/// Commit-log rows are purged after the commit, from a record the transaction
/// writes ([`crate::teardown::record_account_log_purge`]).
pub async fn delete_account(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &req).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    // An empty body is valid when signed (actor from auth).
    let parsed: DeleteAccountBody = if req.body.is_empty() {
        DeleteAccountBody { user_id: None, withdraw: Vec::new() }
    } else {
        match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(_) => return Ok(bad_request("invalid body")),
        }
    };
    let conn = state.db.conn().await?;
    // Its conversations, for the leave wake-up sent after the commit (#1874).
    let left = match resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        Ok(owner) => crate::directory::mls_groups_of(&conn, &owner).await?,
        Err(_) => crate::directory::MlsGroupsOf::default(),
    };
    let (outcome, _dead_conversations) =
        apply_delete_account(&conn, authed.as_deref(), &parsed).await?;
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        // `user_device` is now empty for this user, so every one of their cached
        // keys is stale (#658).
        state.device_keys.invalidate_user(&owner);
        if matches!(outcome, WriteOutcome::Ok) {
            spawn_leave_wakes(&state, left);
            // The commit-log half the deletion transaction recorded. A failure
            // fails the request; the record stays, and the next deletion or DS
            // start runs it (`spawn_account_log_purge_drain`).
            let log_conn = state.log_db.conn().await?;
            crate::teardown::finish_account_log_purge(&conn, &log_conn, &owner).await?;
            spawn_account_log_purge_drain(&state);
        }
    }
    outcome_response::<DeleteAccountBody>(outcome)
}

/// In the background, run any commit-log half an earlier account deletion
/// still owes ([`crate::teardown::drain_account_log_purges`]). Called at DS start
/// and after each account deletion; there is no timer.
pub fn spawn_account_log_purge_drain(state: &AppState) {
    let db = std::sync::Arc::clone(&state.db);
    let log_db = std::sync::Arc::clone(&state.log_db);
    tokio::spawn(async move {
        let result = async {
            let conn = db.conn().await?;
            let log_conn = log_db.conn().await?;
            crate::teardown::drain_account_log_purges(&conn, &log_conn).await
        }
        .await;
        match result {
            Ok(0) => {}
            Ok(n) => tracing::info!("account deletion: ran {n} owed commit-log purge(s)"),
            Err(e) => tracing::warn!("account deletion: owed commit-log purges not run: {e:#}"),
        }
    });
}

/// Tell every conversation the deleted account was in that a member left, so
/// members online remove its devices' leaves now (#1874), whether or not the
/// deleting client's own wake-up got through. The same `membership_changed`
/// leave the clients publish; best-effort, like theirs. The membership version
/// bumped in the deletion is what makes the next seal wait for the removal.
fn spawn_leave_wakes(state: &AppState, left: crate::directory::MlsGroupsOf) {
    if state.broker.livekit_ready().is_none() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let wakes = left
            .groups
            .iter()
            .map(|gid| (gid, serde_json::json!({ "type": "membership_changed", "group_id": gid, "kind": "leave" })))
            .chain(left.dms.iter().map(|dm| {
                (dm, serde_json::json!({ "type": "membership_changed", "conversation_id": dm, "kind": "leave" }))
            }));
        for (room, payload) in wakes {
            if let Err(e) = crate::broker::room_send_data(&state, room, &payload).await {
                tracing::warn!("account deletion: leave wake-up failed: {e}");
            }
        }
    });
}

/// Every remote-data delete of account deletion, in one transaction. Self-scoped
/// to the signer.
///
/// Returns the destroyed conversation ids (also recorded for the post-commit log purge).
pub async fn apply_delete_account(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteAccountBody,
) -> anyhow::Result<(WriteOutcome, Vec<String>)> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok((o, Vec::new())),
    };
    if body.withdraw.len() > pollis_api::account::WITHDRAW_MAX {
        return Ok((WriteOutcome::Invalid("too many envelopes to withdraw"), Vec::new()));
    }
    let tx = conn.transaction().await?;

    // The actor's undelivered envelopes, by capability: sealed ones carry no
    // sender id for `purge_user_rows` to match.
    crate::messages::withdraw_envelopes(&tx, &body.withdraw).await?;

    // Note the actor's DMs BEFORE their membership rows go — afterwards there is
    // nothing left to identify which channels they were in, and an emptied DM
    // would keep its ciphertext and metadata forever.
    let dm_ids = dm_ids_of(&tx, &actor).await?;
    // #1874: every conversation it was in re-checks its tree before the next seal.
    crate::directory::bump_membership_versions_of(&tx, &actor).await?;

    // Read while `user_device` still has them, for the commit-log scrub.
    let device_ids = crate::broker::user_device_ids(&tx, &actor).await?;

    let mut dead_conversations = handoff_group_ownership(&tx, &actor).await?;
    crate::teardown::purge_user_rows(&tx, &actor).await?;

    // Any DM the actor has just left that now has no members at all is dead:
    // tear it down rather than leaving an unreachable conversation behind.
    purge_emptied_dms(&tx, &dm_ids, &mut dead_conversations).await?;

    // The commit-log half cannot join this transaction; it is owed from here on.
    crate::teardown::record_account_log_purge(&tx, &actor, &device_ids, &dead_conversations)
        .await?;

    // The anchor row last.
    tx.execute(
        "DELETE FROM users WHERE id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;

    tx.commit().await?;
    Ok((WriteOutcome::Ok, dead_conversations))
}

/// Group-ownership handoff: tear down sole-member groups, else promote a sole admin's successor.
///
/// Returns the conversation ids destroyed along the way.
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
            // Sole member: tear the whole group down explicitly.
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
                    // Hand `groups.owner_id` over too; it has no foreign key.
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
