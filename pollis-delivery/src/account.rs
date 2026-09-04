//! Domains E + G — account lifecycle, identity rotation, recovery, and the
//! security-audit log.
//!
//! This is the riskiest write surface in #419: it touches the **account-key
//! transparency log** (`account_key_log`, fed to `verifiable-log-builder`'s
//! account tenant → verify.pollis.com), the destructive **account-deletion**
//! and **identity-reset** bulk wipes, the **device-enrollment** approval rows,
//! and the **security-event** audit trail.
//!
//! Every endpoint follows the domain-A..D convention ([`crate::messages`],
//! [`crate::devices`]): a thin axum handler `gate`s the request, parses the
//! body, calls a pure `apply_*(conn, authed, body)` that embeds BOTH
//! authorization and the write, and maps the result to a response. The handler
//! and the integration harness share the `apply_*` fn, so the test suite
//! exercises the exact authz + atomicity the production handler runs.
//!
//! ## Where the writes land
//!
//! Every table here — `users`, `account_key_log`, `account_recovery`,
//! `security_event`, `device_enrollment_request`, `group_member`,
//! `dm_channel_member`, `mls_key_package`, `message_envelope`, `groups`,
//! `user_device` — lives in the **MAIN DB** (`state.db`). None of them is on the
//! commit-log DB, so all `apply_*` fns run on the main connection. (The MLS
//! Welcome purge that identity-reset also performs is a SEPARATE log-DB write
//! routed through [`crate::writes::welcomes_purge`].)
//!
//! ## Authorization — every op is SELF-scoped
//!
//! A user rotates / deletes / recovers / audits only THEIR OWN account.
//! [`resolve_actor`] binds the target user to the authenticated signer (a body
//! `user_id` that differs from the signer is `Forbidden`), and every statement
//! is scoped `WHERE … user_id = actor` (or, for the enrollment rows, the request
//! must belong to the actor). Nothing is re-derived from client-supplied ids.
//!
//! **Credential**: device signature, except `rotate-identity` and
//! `reset-recover`, which take the signature OR a verified-OTP session
//! (`gate_or_session`) — the soft reset runs from a PRE-ENROLLMENT device on
//! the login gate, which has no signing key; its authorization has always been
//! the email OTP (see `tests/reset_session.rs` for the properties).
//!
//! ## The two hard requirements
//!
//! 1. **`account_key_log` CAS.** `account_key_log` is an append-only, seq-ordered
//!    transparency log keyed `UNIQUE (user_id, identity_version)`. A fork or gap
//!    corrupts the published account-key transparency tree. So
//!    [`apply_rotate_identity`] models the commit-log CAS ([`crate::commit`]):
//!    the `account_key_log` append is a single conditional `INSERT … SELECT …
//!    WHERE <based_on == current head> … ON CONFLICT DO NOTHING`, and the
//!    `users.identity_version` bump + the `account_recovery` rewrap ride in the
//!    SAME transaction, gated by that one CAS. Two concurrent rotations at the
//!    same head → SQLite serializes the writers; exactly one append lands and
//!    advances the head, the other sees the new head and inserts nothing →
//!    `Conflict`. No fork, no gap.
//!
//! 2. **`delete_account` / `reset_identity_and_recover` are each ONE
//!    transaction.** Their bulk deletes (and the ownership-handoff promotions)
//!    all commit together or not at all — a half-deleted account is a corrupt
//!    state. [`apply_delete_account`] and [`apply_reset_recover`] run every
//!    statement inside a single libsql transaction.
//!
//! ## What stays DIRECT (bootstrap — deliberately NOT here)
//!
//! A write that ESTABLISHES the signing credential cannot be authenticated by
//! that credential. These remain on the client's direct Turso path:
//!
//!   - **Account creation** (`auth.rs` verify_otp INSERT `users`) — no device
//!     key exists yet.
//!   - **First account-identity establishment** (`account_identity.rs`
//!     `/v1/auth/establish-identity`, version 1) — runs at signup BEFORE
//!     `register_device` / `ensure_device_cert`, so no device signing key is
//!     enrolled and the local DB isn't even open. Single-device, single-shot, no
//!     concurrency: the `UNIQUE (user_id, identity_version)` index is its only
//!     (sufficient) guard. Rotations (version ≥ 2) DO route here, CAS-guarded.
//!   - **Device registration / first cert publish** (`register_device`,
//!     `ensure_device_cert`) — domain-D bootstrap (see [`crate::devices`]).
//!   - **Enrollment *request*** (`start_device_enrollment` INSERT
//!     `device_enrollment_request`) — the requesting device is pre-enrollment:
//!     its `user_device.mls_signature_pub` is still NULL and its local DB is
//!     closed, so it cannot produce a signature the DS would accept. Enrollment
//!     *approval* / *rejection* (run on an already-enrolled sibling) DO route.
//!   - **Secret-Key recovery security-event** (`recover_with_secret_key`) — runs
//!     pre-finalize, before this device has published a cert.
//!
//! Logout device removal USED to be on the direct path, but bucket-C C4 moved it
//! to the DEVICE-SIGNED [`logout_device`] (`POST /v1/auth/logout`): the client now
//! issues the signed DELETE BEFORE it unloads the local DB / clears the signing
//! key, so a read-only Turso token no longer breaks logout.

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
    outcome_response,
    resolve_actor,
    RawRequest,
    WriteOutcome,
};
use crate::AppState;
use crate::util::b64_decode;

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::account::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::account::*;

// ── POST /v1/account/rotate-identity ─────────────────────────────────────────

/// The outcome of an identity rotation. Distinct from [`WriteOutcome`] because a
/// rotation has a THIRD terminal state — a CAS loss — that maps to 409 (not 200
/// / 403), exactly like a commit losing its epoch.
pub enum RotateOutcome {
    /// The rotation won its version. `new_version` is the version now recorded.
    Applied { new_version: i64 },
    /// The signer may not rotate this account (`user_id` ≠ signer).
    Forbidden,
    /// A concurrent rotation already advanced the head past `based_on_version`
    /// (or `based_on_version` was stale). `head_version` is the current head; the
    /// client must re-read and retry. No fork was created.
    Conflict { head_version: i64 },
}

/// POST /v1/account/rotate-identity — rotate a user's account identity key,
/// CAS-guarded so two concurrent rotations can never fork the account-key
/// transparency log. The `account_key_log` append, the `users.identity_version`
/// bump, and the `account_recovery` rewrap are ONE atomic transaction.
pub async fn rotate_identity(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_or_session_and_parse::<RotateIdentityBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_rotate_identity(&conn, authed.as_deref(), &parsed).await?;
    // Rotation does not itself rewrite `mls_signature_pub_pq`, but it changes the
    // account key every device cert chains to and is immediately followed by
    // `/v1/devices/resign` and (for a recovering device) a fresh cert publish.
    // Evicting the user here means the auth path re-reads once and can never
    // serve a key from before the rotation (#658).
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        state.device_keys.invalidate_user(&owner);
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
/// Exactly one of two racing rotations at the same head appends (advancing the
/// head); the other's `WHERE` now sees the new head and inserts nothing →
/// `affected == 0` → `Conflict`, transaction rolled back. The `ON CONFLICT DO
/// NOTHING` is the backstop the `UNIQUE (user_id, identity_version)` index
/// enforces. No fork, no gap, append-only.
pub async fn apply_rotate_identity(
    conn: &Connection,
    authed: Option<&str>,
    body: &RotateIdentityBody,
) -> anyhow::Result<RotateOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(RotateOutcome::Forbidden),
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
        return Ok(RotateOutcome::Conflict { head_version: head });
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
        libsql::params![actor, new_version, salt, nonce, wrapped],
    )
    .await?;

    tx.commit().await?;
    Ok(RotateOutcome::Applied { new_version })
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

// ── POST /v1/security-events ─────────────────────────────────────────────────

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

/// INSERT a `security_event` with `user_id = actor`. The row is always
/// attributed to the signer, so a caller can never forge an audit entry under
/// another user.
pub async fn apply_record_security_event(
    conn: &Connection,
    authed: Option<&str>,
    body: &SecurityEventBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "INSERT INTO security_event (id, user_id, kind, device_id, metadata) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![
            ulid::Ulid::new().to_string(),
            actor,
            body.kind.clone(),
            body.device_id.clone(),
            body.metadata.clone(),
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/enrollment/approve ──────────────────────────────────────────────

/// POST /v1/enrollment/approve — flip a pending enrollment request the actor
/// OWNS to `approved`, attaching the wrapped account key. The approving device
/// has already verified the code / status / expiry client-side; this is the
/// server-bound state transition. Self-scoped: the request's `user_id` must
/// equal the signer (a sibling device of the same account).
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

/// UPDATE the request `WHERE id = ? AND user_id = actor`. The `user_id = actor`
/// bind is the authz: a signer can only approve enrollments for THEIR OWN
/// account. `affected == 0` → the request isn't theirs (or doesn't exist) →
/// `Forbidden`.
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

// ── POST /v1/enrollment/reject ───────────────────────────────────────────────

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

// ── POST /v1/devices/revoke ──────────────────────────────────────────────────

/// POST /v1/devices/revoke — drop the revoked device's unclaimed key packages
/// and tombstone its `user_device` row (`revoked_at`). Self-scoped: both writes
/// are bound `user_id = actor`, so a caller can only revoke their OWN devices.
/// The MLS reconcile that removes the revoked leaf from each tree stays
/// client-side (it commits through `/v1/commits`).
pub async fn revoke_device(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RevokeDeviceBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    // The device's name BEFORE the write (#987). Tombstoning does not clear the
    // column, but reading first keeps "does this device exist on your account?"
    // and "what was it called?" a single question with a single answer.
    let device_name = match resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        Ok(owner) => live_device_name(&conn, &owner, &parsed.device_id).await?,
        Err(_) => None,
    };
    let outcome = apply_revoke_device(&conn, authed.as_deref(), &parsed).await?;
    // Revocation must bite on the NEXT request, not when the cache TTL lapses
    // (#658). Evicted unconditionally: if the write was Forbidden nothing
    // changed and the eviction only costs one re-read.
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        state
            .device_keys
            .invalidate_device(&owner, &parsed.device_id);
        // The device's commit catch-up high-water lives on the commit-log DB and
        // so cannot join the transaction above.
        if matches!(outcome, WriteOutcome::Ok) {
            let log_conn = state.log_db.conn().await?;
            crate::teardown::purge_device_log_rows(&log_conn, &owner, &parsed.device_id).await?;
        }
    }
    match outcome {
        WriteOutcome::Forbidden => Ok(AuthRejection::Forbidden.into_response()),
        WriteOutcome::EpochBehind {
            head_generation,
            head_epoch,
        } => Ok(crate::writes::epoch_behind_response(head_generation, head_epoch)),
        WriteOutcome::Ok => Ok(crate::writes::ok_response::<RevokeDeviceBody>(
            match device_name {
                Some(name) => DeviceRevoked::Ok {
                    device_name: Some(name),
                },
                // The row was absent (or already tombstoned): the UPDATE's
                // `revoked_at IS NULL` bind made it a no-op, so say so rather
                // than report a revocation that did not happen.
                None => DeviceRevoked::NotFound,
            },
        )),
    }
}

/// The name of a LIVE (non-tombstoned) device on `user_id`'s account.
///
/// `None` covers both "no such device" and "already revoked", which the caller
/// treats identically: neither is a revocation this call performed.
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

/// DELETE the revoked device's unclaimed key packages and its conversation
/// watermarks, then tombstone its row — one transaction, all `WHERE user_id =
/// actor`.
///
/// The watermark DELETE is the "invalid states unrepresentable" half of #685: a
/// revoked device can never fetch again, so a `conversation_watermark` row for it
/// is a cursor that can never advance. The envelope-GC reads filter revoked
/// devices out at read time, but leaving the rows behind means the invalid state
/// still exists in the table and every future reader has to remember to exclude
/// it. Deleting it here removes it at the chokepoint instead.
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

// ── POST /v1/auth/logout ─────────────────────────────────────────────────────

/// POST /v1/auth/logout — DELETE the logging-out device's `user_device` row.
/// DEVICE-SIGNED and self-scoped: the signer can only remove a device on their
/// OWN account (`WHERE user_id = actor`).
///
/// Deliberately a DELETE, NOT the tombstone `/v1/devices/revoke` does: a
/// `revoked_at` tombstone permanently fails the device-signature gate, but a
/// logout-with-delete must re-register cleanly on the next sign-in. Removing the
/// row outright lets `register_device` / re-enrollment recreate it. Mirrors
/// pollis-core `auth::logout`'s direct `DELETE FROM user_device`.
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

/// DELETE the device row `WHERE device_id = ? AND user_id = actor`, together
/// with that device's per-device state. The `user_id = actor` bind is the authz:
/// a signer can only log out a device on THEIR OWN account. Idempotent —
/// removing an already-gone device is a no-op `Ok` (a re-tried logout must never
/// error).
///
/// The key-package and watermark deletes mirror [`apply_revoke_device`], which
/// has always done them. Logout did not, so a logged-out device left unclaimed
/// key packages behind — claimable by a peer, producing a Welcome for a device
/// that no longer exists — plus a fetch cursor that can never advance. Neither
/// is reachable by a cascade (`conversation_watermark` has no foreign key at
/// all), so both are explicit.
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

// ── POST /v1/account/reset-recover ───────────────────────────────────────────

/// POST /v1/account/reset-recover — the membership/device wipe that follows an
/// identity reset, as ONE transaction. Removes the actor from all groups/DMs
/// (with ownership handoff), drops their stale key packages, and orphans their
/// OTHER devices. The MLS Welcome purge is a separate log-DB call
/// (`/v1/welcomes/purge`); the identity rotation itself is
/// `/v1/account/rotate-identity`.
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
    // Account-wide device wipe → evict the whole user (#658). Cheaper to reason
    // about than enumerating which sibling devices were dropped, and the kept
    // `current_device_id` merely pays one re-read.
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        state.device_keys.invalidate_user(&owner);
    }
    // Conversations this reset emptied no longer exist; drop their MLS state
    // from the commit-log DB, after the main transaction has committed.
    if matches!(outcome, WriteOutcome::Ok) && !dead_conversations.is_empty() {
        let log_conn = state.log_db.conn().await?;
        crate::teardown::purge_conversation_log(&log_conn, &dead_conversations).await?;
    }
    outcome_response::<ResetRecoverBody>(outcome)
}

/// All of identity-reset's main-DB cleanup, in one transaction. Self-scoped: the
/// actor is the signer, and every statement is bound to `user_id = actor` (the
/// admin promotions touch related rows as a server-authorized consequence of the
/// actor leaving — exactly mirroring [`apply_delete_account`]).
///
/// This is NOT account deletion: the `users` row and the account's own records
/// (preferences, blocks, security events, recovery blob) deliberately survive,
/// because the account does. Only membership and device state is shed. Returns
/// the conversation ids this reset destroyed, for the caller's post-commit
/// commit-log purge.
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

    // Note the actor's DMs before their membership rows go — see
    // `apply_delete_account` for why this has to happen first.
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

    tx.execute(
        "DELETE FROM group_member WHERE user_id = ?1",
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

    // Orphan the actor's OTHER devices; keep the current one (it re-enrolls under
    // the new identity). `None` → drop them all.
    match &body.current_device_id {
        Some(dev) => {
            tx.execute(
                "DELETE FROM user_device WHERE user_id = ?1 AND device_id != ?2",
                libsql::params![actor.clone(), dev.clone()],
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
    match &body.current_device_id {
        Some(dev) => {
            tx.execute(
                "DELETE FROM conversation_watermark WHERE user_id = ?1 AND device_id != ?2",
                libsql::params![actor.clone(), dev.clone()],
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

    tx.commit().await?;
    Ok((WriteOutcome::Ok, dead_conversations))
}

// ── POST /v1/account/delete ──────────────────────────────────────────────────

/// POST /v1/account/delete — permanently delete the actor's account, as ONE
/// transaction. Handles group ownership (delete empty groups / promote a sole
/// admin's successor), tears down any DM left with nobody in it, and deletes
/// every main-DB row naming the actor via [`crate::teardown::purge_user_rows`].
///
/// Nothing here relies on `ON DELETE CASCADE`: production Turso runs with
/// `foreign_keys=OFF`, so the schema's cascade clauses never fire and this used
/// to leave the device roster, DM membership, invites, recovery blob and
/// security-event log behind. See [`crate::teardown`].
///
/// The commit-log DB cannot join the main transaction, so its rows (the actor's
/// Welcomes, plus the MLS state of any conversation this deletion destroyed) are
/// purged after the commit.
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
        // `user_device` is now empty for this user, so every one of their cached
        // keys is stale (#658).
        state.device_keys.invalidate_user(&owner);
        // Commit-log DB, only once the main transaction has committed — see
        // `teardown::purge_conversation_log` for why the order is load-bearing.
        if matches!(outcome, WriteOutcome::Ok) {
            let log_conn = state.log_db.conn().await?;
            crate::writes::purge_welcomes(&log_conn, &owner).await?;
            crate::teardown::purge_user_log_rows(&log_conn, &owner).await?;
            crate::teardown::purge_conversation_log(&log_conn, &dead_conversations).await?;
        }
    }
    outcome_response::<DeleteAccountBody>(outcome)
}

/// Every remote-data delete of account deletion, in one transaction. Self-scoped
/// to the signer.
///
/// Returns the ids of conversations this deletion destroyed (channels of groups
/// the actor was the last member of, and DMs left with nobody in them), so the
/// caller can purge their commit-log-DB rows once this transaction has
/// committed.
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

    // Note the actor's DMs BEFORE their membership rows go — afterwards there is
    // nothing left to identify which channels they were in, and an emptied DM
    // would keep its ciphertext and metadata forever.
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

    // Any DM the actor has just left that now has no members at all is dead:
    // tear it down rather than leaving an unreachable conversation behind.
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

    // The anchor row last.
    tx.execute(
        "DELETE FROM users WHERE id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;

    tx.commit().await?;
    Ok((WriteOutcome::Ok, dead_conversations))
}

/// Group-ownership handoff shared by [`apply_delete_account`] and
/// [`apply_reset_recover`]: for every group the actor belongs to, tear the
/// group down if they are its sole member, else promote a successor if they are
/// its sole admin. Runs inside the caller's transaction (`&Transaction` derefs
/// to `&Connection`). Mirrors the direct logic this replaces in `auth.rs` /
/// `device_enrollment.rs`.
///
/// Returns the conversation ids destroyed along the way (the channels of every
/// group torn down), for the caller's post-commit commit-log purge.
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
            // Sole member — tear the whole group down. Explicitly, table by
            // table: the cascade this used to trust does not fire on Turso.
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
                    // Hand the `groups.owner_id` pointer over too. It has no
                    // foreign key, so it was left naming a user who no longer
                    // exists — a dangling reference AND a deleted user's id
                    // retained in a table that outlives them.
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
