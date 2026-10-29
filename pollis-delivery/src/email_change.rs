//! Email-change OTP, **device-signed**. Mirrors pollis-core `auth::verify_email_change`.
//!
//! THREE independent proofs are required, and they bind different facts:
//!
//!   - the **device signature** (the gate) proves the CURRENT account (`authed`);
//!   - the **new-address OTP**, keyed by the NEW email, proves control of the
//!     new mailbox;
//!   - the **current-address OTP** (#1161), keyed by the address the account is
//!     on today, proves control of the mailbox that actually owns the account.
//!
//! The third is the one a stolen unlocked device lacks; without it, whoever holds it could move the address.
//!
//! Step 2 requires the verifying `authed` to be step 1's requester; the email binds from the signature.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use crate::error::{AppError, AuthRejection};
use crate::otp::{
    normalize_email, process_request_otp, process_request_otp_for, OtpConfig, OtpStore,
    VerifyOutcome,
};
use crate::writes::{bad_request, gate, RawRequest};
use crate::AppState;

// Request bodies live in `pollis-api`, shared with pollis-core; re-exported here.
pub use pollis_api::email_change::*;

/// The email-change machinery: a dedicated OTP store plus the requester binding.
///
/// A SEPARATE OTP store from signup's, so a change to X can't collide with a signup for X.
#[derive(Clone, Default)]
pub struct EmailChangeStore {
    /// The signup OTP machinery, privately, keyed by `new_email`.
    otp: OtpStore,
    /// The CURRENT-address challenge (#1161); its own store so one answer can't satisfy the other.
    current_otp: OtpStore,
    /// normalized `new_email` → the device-signed requester.
    requesters: Arc<Mutex<HashMap<String, String>>>,
}

impl EmailChangeStore {
    /// Record `requester` for `new_email` (latest wins), then send BOTH codes via [`process_request_otp`].
    ///
    /// `current_email` comes from the database, never the body; `None` sends only the new code.
    pub async fn request(
        &self,
        cfg: &OtpConfig,
        requester: &str,
        new_email: &str,
        current_email: Option<&str>,
    ) {
        {
            let mut g = self
                .requesters
                .lock()
                .expect("email-change requesters mutex poisoned");
            g.insert(normalize_email(new_email), requester.to_string());
        }
        process_request_otp(&self.otp, cfg, new_email).await;
        if let Some(current) = current_email {
            process_request_otp_for(
                &self.current_otp,
                cfg,
                current,
                crate::otp::OtpPurpose::EmailChangeChallenge,
            )
            .await;
        }
    }

    /// The recorded requester for `new_email`, if any.
    fn requester_of(&self, new_email: &str) -> Option<String> {
        self.requesters
            .lock()
            .expect("email-change requesters mutex poisoned")
            .get(&normalize_email(new_email))
            .cloned()
    }

    /// Drop the requester binding for `new_email`.
    fn clear(&self, new_email: &str) {
        self.requesters
            .lock()
            .expect("email-change requesters mutex poisoned")
            .remove(&normalize_email(new_email));
    }
}

/// The shared `{"status":"ok"}`, generic over the endpoint (#922).
fn ok_status<B>() -> Response
where
    B: pollis_api::DsRequest<Response = pollis_api::StatusOk>,
{
    crate::writes::ok_response::<B>(pollis_api::StatusOk::Ok)
}

fn internal(e: anyhow::Error) -> Response {
    tracing::error!("email-change internal error: {e:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "internal error" })),
    )
        .into_response()
}

/// POST /v1/auth/request-email-change-otp — DEVICE-SIGNED. **Always 200** (anti-enumeration).
pub async fn request_email_change_otp(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &req).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    // No signed identity to bind the change to on the no-auth path.
    let requester = match authed {
        Some(u) => u,
        None => return Ok(AuthRejection::Unauthorized.into_response()),
    };

    let parsed: RequestEmailChangeBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let new_email = parsed.new_email.trim().to_string();
    if !new_email.is_empty() {
        // The address to challenge comes from the account row, never the body.
        let conn = state.db.conn().await?;
        let current_email = match email_of(&conn, &requester).await {
            Ok(e) => e,
            Err(e) => return Ok(internal(e)),
        };
        state
            .email_change
            .request(
                &state.otp_config,
                &requester,
                &new_email,
                current_email.as_deref(),
            )
            .await;
    }
    Ok(ok_status::<RequestEmailChangeBody>())
}

/// The outcome of [`apply_verify_email_change`].
#[derive(Debug, PartialEq, Eq)]
pub enum EmailChangeOutcome {
    /// `users.email` swapped; carries the username (#987).
    Updated { username: Option<String> },
    /// Wrong / expired / unknown code for the NEW address → 401.
    InvalidCode,
    /// Missing / wrong / expired code for the CURRENT address (#1161) → 401, with its own message.
    InvalidCurrentCode,
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
    req: RawRequest,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &req).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let authed = match authed {
        Some(u) => u,
        None => return Ok(AuthRejection::Unauthorized.into_response()),
    };

    let parsed: VerifyEmailChangeBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };

    let conn = state.db.conn().await?;
    match apply_verify_email_change(
        &conn,
        &state.email_change,
        &state.otp_config,
        &authed,
        &parsed.new_email,
        &parsed.code,
        parsed.current_code.as_deref(),
    )
    .await
    {
        Ok(outcome) => Ok(email_change_response(outcome)),
        Err(e) => Ok(internal(e)),
    }
}

/// Validate the OTP + requester binding, then swap `users.email`; shared with the harness.
pub async fn apply_verify_email_change(
    conn: &libsql::Connection,
    store: &EmailChangeStore,
    cfg: &OtpConfig,
    authed: &str,
    new_email: &str,
    code: &str,
    current_code: Option<&str>,
) -> anyhow::Result<EmailChangeOutcome> {
    // Canonical form, as the OTP store and `users.email` (#1088) hold it.
    let canonical = normalize_email(new_email);
    let trimmed = canonical.as_str();

    // Binding gate FIRST, so another user can't even burn the attempt counter.
    match store.requester_of(trimmed) {
        Some(r) if r == authed => {}
        _ => return Ok(EmailChangeOutcome::Mismatch),
    }

    // Re-prove the CURRENT address (#1161) before anything is consumed; only a row with no address skips it.
    let old_email = email_of(conn, authed).await?;
    if let Some(current) = old_email.as_deref() {
        let current_key = normalize_email(current);
        let Some(current_code) = current_code.filter(|c| !c.trim().is_empty()) else {
            return Ok(EmailChangeOutcome::InvalidCurrentCode);
        };
        match store
            .current_otp
            .check(&current_key, current_code.trim(), cfg, crate::util::now_unix())
        {
            VerifyOutcome::Ok => {}
            VerifyOutcome::LockedOut => {
                store.clear(trimmed);
                return Ok(EmailChangeOutcome::LockedOut);
            }
            VerifyOutcome::Invalid | VerifyOutcome::Expired | VerifyOutcome::NotFound => {
                return Ok(EmailChangeOutcome::InvalidCurrentCode);
            }
        }
    }

    // Validate WITHOUT consuming, so a DB failure below leaves the code valid for a retry (#518).
    match store.otp.check(trimmed, code, cfg, crate::util::now_unix()) {
        VerifyOutcome::Ok => {}
        VerifyOutcome::LockedOut => {
            // check() already deleted the code on lockout; drop the binding too.
            store.clear(trimmed);
            return Ok(EmailChangeOutcome::LockedOut);
        }
        VerifyOutcome::Invalid | VerifyOutcome::Expired | VerifyOutcome::NotFound => {
            // Wrong-but-not-locked: keep the code + binding so the caller can retry.
            return Ok(EmailChangeOutcome::InvalidCode);
        }
    }

    // Race-close: someone else may have claimed this email since the request.
    let mut rows = conn
        .query(
            "SELECT 1 FROM users WHERE email = ?1 AND id != ?2",
            libsql::params![trimmed.to_string(), authed.to_string()],
        )
        .await?;
    if rows.next().await?.is_some() {
        // Correct code, but the email is taken: consume it and report the conflict.
        store.otp.consume(trimmed);
        store.clear(trimmed);
        return Ok(EmailChangeOutcome::EmailTaken);
    }

    conn.execute(
        "UPDATE users SET email = ?1 WHERE id = ?2",
        libsql::params![trimmed.to_string(), authed.to_string()],
    )
    .await?;
    // Applied: consume BOTH codes (single-use) now that the write has succeeded.
    store.otp.consume(trimmed);
    if let Some(current) = old_email.as_deref() {
        store.current_otp.consume(&normalize_email(current));
    }
    store.clear(trimmed);

    // Tell the account (L3): a DS-authored `security_event` and a mail to the OLD address; neither fails the change.
    if let Some(old) = old_email.as_deref() {
        if let Err(e) = conn
            .execute(
                "INSERT INTO security_event (id, user_id, kind, metadata) \
                 VALUES (?1, ?2, 'email_changed', ?3)",
                libsql::params![
                    ulid::Ulid::new().to_string(),
                    authed.to_string(),
                    serde_json::json!({ "from": old, "to": trimmed }).to_string(),
                ],
            )
            .await
        {
            tracing::warn!(error = %e, "email change: failed to record the security event");
        }
        if let Some(key) = &cfg.resend_api_key {
            if let Err(e) = crate::otp::send_email_change_notice(key, old, trimmed).await {
                tracing::warn!(error = %e, "email change: failed to notify the old address");
            }
        }
    }

    Ok(EmailChangeOutcome::Updated {
        username: username_of(conn, authed).await?,
    })
}

/// The account's current email, read before it is replaced.
async fn email_of(conn: &libsql::Connection, user_id: &str) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT email FROM users WHERE id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<Option<String>>(0)?),
        None => Ok(None),
    }
}

/// The caller's username, for the client's accounts-index mirror.
async fn username_of(
    conn: &libsql::Connection,
    user_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT username FROM users WHERE id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<Option<String>>(0)?),
        None => Ok(None),
    }
}

/// Map an [`EmailChangeOutcome`] to the wire response (handler and harness).
pub fn email_change_response(outcome: EmailChangeOutcome) -> Response {
    match outcome {
        EmailChangeOutcome::Updated { username } => crate::writes::ok_response::<
            VerifyEmailChangeBody,
        >(pollis_api::email_change::EmailChanged::Ok { username }),
        EmailChangeOutcome::InvalidCode => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid code" })),
        )
            .into_response(),
        EmailChangeOutcome::InvalidCurrentCode => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid current-address code" })),
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
