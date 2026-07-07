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
//!     `generate_account_identity`, version 1) — runs at signup BEFORE
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
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use libsql::Connection;
use serde::Deserialize;

use crate::error::{AppError, AuthRejection};
use crate::writes::{bad_request, gate, gate_or_session, ok_json, outcome_response, resolve_actor, WriteOutcome};
use crate::AppState;

fn b64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.decode(s)?)
}

// ── POST /v1/account/rotate-identity ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct RotateIdentityBody {
    /// The `identity_version` the client read BEFORE rotating — i.e. the head of
    /// `account_key_log` it believes it is appending onto. The new version is
    /// `based_on_version + 1`. This is the CAS expectation, the exact analogue of
    /// a commit's `based_on_epoch`.
    pub based_on_version: i64,
    /// New account identity public key, base64 (STANDARD).
    pub account_id_pub: String,
    /// New `account_recovery` blob, all base64 (STANDARD).
    pub salt: String,
    pub nonce: String,
    pub wrapped_key: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

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
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate_or_session(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: RotateIdentityBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    rotate_outcome_response(apply_rotate_identity(&conn, authed.as_deref(), &parsed).await?)
}

/// Map a [`RotateOutcome`] to its HTTP response (200 / 403 / 409).
pub(crate) fn rotate_outcome_response(outcome: RotateOutcome) -> Result<Response, AppError> {
    Ok(match outcome {
        RotateOutcome::Applied { new_version } => {
            ok_json(serde_json::json!({ "status": "ok", "identity_version": new_version }))
        }
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

#[derive(Deserialize)]
pub struct SecurityEventBody {
    pub kind: String,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub metadata: Option<String>,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/security-events — append a row to the actor's own security-audit
/// log (`security_event`). Self-scoped: the row's `user_id` is the signer.
pub async fn record_security_event(
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
    let parsed: SecurityEventBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_record_security_event(&conn, authed.as_deref(), &parsed).await?)
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

#[derive(Deserialize)]
pub struct ApproveEnrollmentBody {
    pub request_id: String,
    /// base64 (STANDARD) of `approver_pub || nonce || ciphertext`.
    pub wrapped_account_key: String,
    pub approved_by_device_id: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/enrollment/approve — flip a pending enrollment request the actor
/// OWNS to `approved`, attaching the wrapped account key. The approving device
/// has already verified the code / status / expiry client-side; this is the
/// server-bound state transition. Self-scoped: the request's `user_id` must
/// equal the signer (a sibling device of the same account).
pub async fn approve_enrollment(
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
    let parsed: ApproveEnrollmentBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_approve_enrollment(&conn, authed.as_deref(), &parsed).await?)
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

#[derive(Deserialize)]
pub struct RejectEnrollmentBody {
    pub request_id: String,
    pub approved_by_device_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/enrollment/reject — flip a pending enrollment request the actor OWNS
/// to `rejected`. Self-scoped like [`approve_enrollment`].
pub async fn reject_enrollment(
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
    let parsed: RejectEnrollmentBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_reject_enrollment(&conn, authed.as_deref(), &parsed).await?)
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

#[derive(Deserialize)]
pub struct RevokeDeviceBody {
    /// The device being revoked (one of the actor's OWN devices, not the caller).
    pub device_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/devices/revoke — drop the revoked device's unclaimed key packages
/// and tombstone its `user_device` row (`revoked_at`). Self-scoped: both writes
/// are bound `user_id = actor`, so a caller can only revoke their OWN devices.
/// The MLS reconcile that removes the revoked leaf from each tree stays
/// client-side (it commits through `/v1/commits`).
pub async fn revoke_device(
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
    let parsed: RevokeDeviceBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_revoke_device(&conn, authed.as_deref(), &parsed).await?)
}

/// DELETE the revoked device's unclaimed key packages, then tombstone its row —
/// one transaction, both `WHERE user_id = actor`.
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
        "UPDATE user_device SET revoked_at = datetime('now') \
         WHERE device_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
        libsql::params![body.device_id.clone(), actor],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/auth/logout ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LogoutDeviceBody {
    /// The device logging out — in practice the signing device itself. Bound
    /// `WHERE user_id = actor`, so a caller can only remove a device on THEIR OWN
    /// account.
    pub device_id: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

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
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: LogoutDeviceBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_logout_device(&conn, authed.as_deref(), &parsed).await?)
}

/// DELETE the device row `WHERE device_id = ? AND user_id = actor`. The
/// `user_id = actor` bind is the authz: a signer can only log out a device on
/// THEIR OWN account. Idempotent — removing an already-gone device is a no-op
/// `Ok` (a re-tried logout must never error).
pub async fn apply_logout_device(
    conn: &Connection,
    authed: Option<&str>,
    body: &LogoutDeviceBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "DELETE FROM user_device WHERE device_id = ?1 AND user_id = ?2",
        libsql::params![body.device_id.clone(), actor],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/account/reset-recover ───────────────────────────────────────────

#[derive(Deserialize)]
pub struct ResetRecoverBody {
    /// The device performing the reset — its `user_device` row is KEPT (it stays
    /// enrolled under the new identity); every OTHER device of the actor is
    /// dropped. `None` → drop ALL of the actor's devices.
    #[serde(default)]
    pub current_device_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/account/reset-recover — the membership/device wipe that follows an
/// identity reset, as ONE transaction. Removes the actor from all groups/DMs
/// (with ownership handoff), drops their stale key packages, and orphans their
/// OTHER devices. The MLS Welcome purge is a separate log-DB call
/// (`/v1/welcomes/purge`); the identity rotation itself is
/// `/v1/account/rotate-identity`.
pub async fn reset_recover(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate_or_session(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: ResetRecoverBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_reset_recover(&conn, authed.as_deref(), &parsed).await?)
}

/// All of identity-reset's main-DB cleanup, in one transaction. Self-scoped: the
/// actor is the signer, and every statement is bound to `user_id = actor` (the
/// admin promotions touch related rows as a server-authorized consequence of the
/// actor leaving — exactly mirroring [`apply_delete_account`]).
pub async fn apply_reset_recover(
    conn: &Connection,
    authed: Option<&str>,
    body: &ResetRecoverBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let tx = conn.transaction().await?;
    handoff_group_ownership(&tx, &actor).await?;

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

    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/account/delete ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DeleteAccountBody {
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/account/delete — permanently delete the actor's account, as ONE
/// transaction. Handles group ownership (delete empty groups / promote a sole
/// admin's successor), removes sent envelopes, key packages, and memberships,
/// then deletes the `users` row (cascading `dm_channel_member`, `group_invite`,
/// `user_device`, `account_recovery`, `security_event`, …).
pub async fn delete_account(
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
    // An empty body is valid when signed (actor from auth).
    let parsed: DeleteAccountBody = if body.is_empty() {
        DeleteAccountBody { user_id: None }
    } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return Ok(bad_request("invalid body")),
        }
    };
    let conn = state.db.conn()?;
    outcome_response(apply_delete_account(&conn, authed.as_deref(), &parsed).await?)
}

/// Every remote-data delete of account deletion, in one transaction. Self-scoped
/// to the signer.
pub async fn apply_delete_account(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteAccountBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let tx = conn.transaction().await?;
    handoff_group_ownership(&tx, &actor).await?;

    tx.execute(
        "DELETE FROM message_envelope WHERE sender_id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM mls_key_package WHERE user_id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM group_member WHERE user_id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;
    // The user row last — FK ON DELETE CASCADE clears dm_channel_member,
    // group_invite, user_device, account_recovery, security_event, …
    tx.execute(
        "DELETE FROM users WHERE id = ?1",
        libsql::params![actor.clone()],
    )
    .await?;

    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// Group-ownership handoff shared by [`apply_delete_account`] and
/// [`apply_reset_recover`]: for every group the actor belongs to, delete the
/// group if they are its sole member, else promote a successor if they are its
/// sole admin. Runs inside the caller's transaction (`&Transaction` derefs to
/// `&Connection`). Mirrors the direct logic this replaces in `auth.rs` /
/// `device_enrollment.rs`.
async fn handoff_group_ownership(conn: &Connection, actor: &str) -> anyhow::Result<()> {
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
            // Sole member — delete the entire group (cascades channels, invites…).
            conn.execute(
                "DELETE FROM groups WHERE id = ?1",
                libsql::params![gid.clone()],
            )
            .await?;
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
                        libsql::params![gid.clone(), new_admin],
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}
