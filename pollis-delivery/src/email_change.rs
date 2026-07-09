//! Email-change OTP — **device-signed**, the last client-side Resend + direct
//! `UPDATE users` write to move behind the DS (Goal B #419 final piece). Mirrors
//! pollis-core `auth::{request_email_change_otp, verify_email_change}`.
//!
//! Unlike signup bootstrap (which is OTP-session-gated because it *establishes*
//! the device credential), email change happens when the user is ALREADY fully
//! authenticated, so these endpoints use the EXISTING device-signature gate
//! ([`crate::writes::gate`] → `authed`). Two independent proofs are required, and
//! they bind different facts:
//!
//!   - the **device signature** (the gate) proves the CURRENT account (`authed`);
//!   - the **OTP**, keyed by the NEW email, proves control of the new mailbox.
//!
//! Step 1 records `(authed → new_email)`; step 2 requires the verifying `authed`
//! to equal the recorded requester. So a different signed user can never consume
//! someone else's pending change, and the email is ALWAYS bound from the
//! signature — never the body. The write lands on the MAIN DB (`state.db`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::error::{AppError, AuthRejection};
use crate::otp::{normalize_email, process_request_otp, OtpConfig, OtpStore, VerifyOutcome};
use crate::writes::{bad_request, gate};
use crate::AppState;

/// The email-change machinery: a dedicated OTP store plus the requester binding.
///
/// The OTP store is a SEPARATE instance from signup's (`state.otp`) so an
/// email-change to address X can never collide with a concurrent signup
/// `request-otp` for X (both key by email). Shallow-`Clone` (shared `Arc`s) so it
/// rides on the `Clone` `AppState`.
#[derive(Clone, Default)]
pub struct EmailChangeStore {
    /// Reuses the signup OTP machinery (salted hash + constant-time + lockout +
    /// single-use) verbatim — a private instance keyed by `new_email`.
    otp: OtpStore,
    /// normalized `new_email` → the device-signed `user_id` that requested it.
    /// Keyed identically to the OTP store so request/verify always agree.
    requesters: Arc<Mutex<HashMap<String, String>>>,
}

impl EmailChangeStore {
    /// Record `requester` as the one asking to change to `new_email`, then
    /// prepare + send the OTP (reusing [`process_request_otp`] — DEV_OTP / Resend
    /// / throttle all honored). The binding is overwritten on each request so the
    /// latest requester wins.
    pub async fn request(&self, cfg: &OtpConfig, requester: &str, new_email: &str) {
        {
            let mut g = self
                .requesters
                .lock()
                .expect("email-change requesters mutex poisoned");
            g.insert(normalize_email(new_email), requester.to_string());
        }
        process_request_otp(&self.otp, cfg, new_email).await;
    }

    /// The recorded requester for `new_email`, if any.
    fn requester_of(&self, new_email: &str) -> Option<String> {
        self.requesters
            .lock()
            .expect("email-change requesters mutex poisoned")
            .get(&normalize_email(new_email))
            .cloned()
    }

    /// Drop the requester binding for `new_email` (after the OTP is consumed or
    /// the change is finalized/refused).
    fn clear(&self, new_email: &str) {
        self.requesters
            .lock()
            .expect("email-change requesters mutex poisoned")
            .remove(&normalize_email(new_email));
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn ok_status() -> Response {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

fn internal(e: anyhow::Error) -> Response {
    tracing::error!("email-change internal error: {e:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "internal error" })),
    )
        .into_response()
}

// ── POST /v1/auth/request-email-change-otp ───────────────────────────────────

#[derive(Deserialize)]
pub struct RequestEmailChangeBody {
    pub new_email: String,
}

/// POST /v1/auth/request-email-change-otp — DEVICE-SIGNED. Record the
/// authenticated requester for `new_email`, generate + store + email an OTP keyed
/// by `new_email`. **Always 200** (anti-enumeration; mirrors `request-otp`) — the
/// uniqueness check is deferred to verify time, which can't leak account
/// existence to an unauthenticated probe. The requester is bound from the
/// signature, NEVER the body.
pub async fn request_email_change_otp(
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
    // Email change presupposes a credentialed device — there is no signed
    // identity to bind the change to on the no-auth path, so reject it.
    let requester = match authed {
        Some(u) => u,
        None => return Ok(AuthRejection::Unauthorized.into_response()),
    };

    let parsed: RequestEmailChangeBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let new_email = parsed.new_email.trim().to_string();
    if !new_email.is_empty() {
        state
            .email_change
            .request(&state.otp_config, &requester, &new_email)
            .await;
    }
    Ok(ok_status())
}

// ── POST /v1/auth/verify-email-change ────────────────────────────────────────

#[derive(Deserialize)]
pub struct VerifyEmailChangeBody {
    pub new_email: String,
    pub code: String,
}

/// The outcome of [`apply_verify_email_change`] — the handler maps it to the wire
/// response (the in-process harness maps it the same way).
#[derive(Debug, PartialEq, Eq)]
pub enum EmailChangeOutcome {
    /// `users.email` swapped to the new address.
    Updated,
    /// Wrong / expired / unknown code → 401.
    InvalidCode,
    /// Past the attempt limit → 429.
    LockedOut,
    /// The device-signed caller is not the one who requested this change → 403.
    Mismatch,
    /// The new email already belongs to another account → 409.
    EmailTaken,
}

/// POST /v1/auth/verify-email-change — DEVICE-SIGNED. Validate the OTP for
/// `new_email` and atomically swap `users.email` for the authenticated caller.
pub async fn verify_email_change(
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
    let authed = match authed {
        Some(u) => u,
        None => return Ok(AuthRejection::Unauthorized.into_response()),
    };

    let parsed: VerifyEmailChangeBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };

    let conn = state.db.conn()?;
    match apply_verify_email_change(
        &conn,
        &state.email_change,
        &state.otp_config,
        &authed,
        &parsed.new_email,
        &parsed.code,
    )
    .await
    {
        Ok(outcome) => Ok(email_change_response(outcome)),
        Err(e) => Ok(internal(e)),
    }
}

/// Validate the OTP + requester binding, then swap `users.email` — all the
/// store/DB work behind verify-email-change, extracted from the handler so the
/// in-process integration harness drives the identical logic against the shared
/// main DB + email-change store. `authed` is the device-signed caller, never a
/// body field.
pub async fn apply_verify_email_change(
    conn: &libsql::Connection,
    store: &EmailChangeStore,
    cfg: &OtpConfig,
    authed: &str,
    new_email: &str,
    code: &str,
) -> anyhow::Result<EmailChangeOutcome> {
    let trimmed = new_email.trim();

    // Binding gate FIRST — the device-signed caller MUST be the one who requested
    // this change. Checked before the OTP so a different user can't even burn the
    // attempt counter (or learn anything) against someone else's pending change.
    match store.requester_of(trimmed) {
        Some(r) if r == authed => {}
        _ => return Ok(EmailChangeOutcome::Mismatch),
    }

    // Validate WITHOUT consuming (see `OtpStore::check`): the code stays valid until
    // the `users.email` write below succeeds, so a transient/config DB failure
    // returns a clean 5xx and the same code still works on retry instead of being
    // burned and disguised as "invalid code" (#518). Wrong-guess accounting stands.
    match store.otp.check(trimmed, code, cfg.max_attempts, now_unix()) {
        VerifyOutcome::Ok => {}
        VerifyOutcome::LockedOut => {
            // check() already deleted the code on lockout; drop the binding too so a
            // retry must re-request.
            store.clear(trimmed);
            return Ok(EmailChangeOutcome::LockedOut);
        }
        VerifyOutcome::Invalid | VerifyOutcome::Expired | VerifyOutcome::NotFound => {
            // Wrong-but-not-locked: keep the code + binding so the caller can retry.
            return Ok(EmailChangeOutcome::InvalidCode);
        }
    }

    // Race-close: between request and verify, someone else could have claimed this
    // email. Reject if so — mirrors pollis-core `verify_email_change`. A DB error on
    // this read `?`-propagates to a 5xx WITHOUT consuming the code.
    let mut rows = conn
        .query(
            "SELECT 1 FROM users WHERE email = ?1 AND id != ?2",
            libsql::params![trimmed.to_string(), authed.to_string()],
        )
        .await?;
    if rows.next().await?.is_some() {
        // Correct code, but the target email is taken. Consume it (the binding is
        // dropped, so a retry must re-request anyway) and report the conflict.
        store.otp.consume(trimmed);
        store.clear(trimmed);
        return Ok(EmailChangeOutcome::EmailTaken);
    }

    conn.execute(
        "UPDATE users SET email = ?1 WHERE id = ?2",
        libsql::params![trimmed.to_string(), authed.to_string()],
    )
    .await?;
    // Applied: consume the code (single-use) now that the write has succeeded.
    store.otp.consume(trimmed);
    store.clear(trimmed);
    Ok(EmailChangeOutcome::Updated)
}

/// Map an [`EmailChangeOutcome`] to the wire response. Shared by the production
/// handler and the in-process harness so both speak the same status codes.
pub fn email_change_response(outcome: EmailChangeOutcome) -> Response {
    match outcome {
        EmailChangeOutcome::Updated => ok_status(),
        EmailChangeOutcome::InvalidCode => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid code" })),
        )
            .into_response(),
        EmailChangeOutcome::LockedOut => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "too many attempts" })),
        )
            .into_response(),
        EmailChangeOutcome::Mismatch => AuthRejection::Forbidden.into_response(),
        EmailChangeOutcome::EmailTaken => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "email already in use" })),
        )
            .into_response(),
    }
}
