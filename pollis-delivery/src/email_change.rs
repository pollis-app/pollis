//! Email-change OTP, device-signed. Three independent proofs are required:
//!
//!   - the **device signature** ([`crate::writes::gate`]) proves the current account;
//!   - the **new-address OTP** proves control of the new mailbox;
//!   - the **current-address OTP** (#1161) proves control of the mailbox that owns
//!     the account. It is the one a stolen unlocked device lacks, so it makes the
//!     change one the owner approves rather than one they are told about.
//!
//! Step 1 records `(authed → new_email)` and sends both codes; step 2 requires
//! the verifying `authed` to equal the recorded requester, so a different signed
//! user can never consume someone else's pending change.

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

// Request bodies are declared once in `pollis-api`, shared with the client.
pub use pollis_api::email_change::*;

/// Dedicated OTP stores plus the requester binding. Separate from signup's
/// `state.otp` so a change to address X never collides with a concurrent signup
/// `request-otp` for X (both key by email).
#[derive(Clone, Default)]
pub struct EmailChangeStore {
    /// Keyed by `new_email`.
    otp: OtpStore,
    /// The current-address challenge (#1161), keyed by the account's existing
    /// email. Not shared with `otp`: a change TO the address the account is
    /// already on would let one code answer both questions.
    current_otp: OtpStore,
    /// Normalized `new_email` → the device-signed `user_id` that requested it.
    requesters: Arc<Mutex<HashMap<String, String>>>,
}

impl EmailChangeStore {
    /// Bind `requester` to `new_email` (latest requester wins) and send both
    /// codes. `current_email` comes from the account row, never the body, or the
    /// caller could choose which mailbox approves the change; `None` (no address
    /// on the row) sends only the new-address code.
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

    fn requester_of(&self, new_email: &str) -> Option<String> {
        self.requesters
            .lock()
            .expect("email-change requesters mutex poisoned")
            .get(&normalize_email(new_email))
            .cloned()
    }

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

/// POST /v1/auth/request-email-change-otp. **Always 200** (anti-enumeration):
/// the uniqueness check is deferred to verify time. The requester is bound from
/// the signature, never the body.
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

/// Mapped to the wire by [`email_change_response`].
#[derive(Debug, PartialEq, Eq)]
pub enum EmailChangeOutcome {
    /// Carries the username so the client's `accounts.json` mirror needs no
    /// follow-up read.
    Updated { username: Option<String> },
    /// Wrong / expired / unknown code for the new address → 401.
    InvalidCode,
    /// Missing / wrong / expired code for the current address (#1161) → 401,
    /// with its own message so the UI can say which code is wrong.
    InvalidCurrentCode,
    /// Past the attempt limit → 429.
    LockedOut,
    /// The device-signed caller is not the one who requested this change → 403.
    Mismatch,
    /// The new email already belongs to another account → 409.
    EmailTaken,
}

/// POST /v1/auth/verify-email-change.
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

/// Validate the requester binding and both codes, then swap `users.email`.
/// Shared with the in-process harness. `authed` is the device-signed caller.
pub async fn apply_verify_email_change(
    conn: &libsql::Connection,
    store: &EmailChangeStore,
    cfg: &OtpConfig,
    authed: &str,
    new_email: &str,
    code: &str,
    current_code: Option<&str>,
) -> anyhow::Result<EmailChangeOutcome> {
    // The OTP store's key, and what `users.email` holds for every row (#1088).
    let canonical = normalize_email(new_email);
    let trimmed = canonical.as_str();

    // Before any OTP check, so another user cannot burn the attempt counter on
    // someone else's pending change.
    match store.requester_of(trimmed) {
        Some(r) if r == authed => {}
        _ => return Ok(EmailChangeOutcome::Mismatch),
    }

    // Re-prove the current address (#1161) before the new-address code, so a
    // caller who cannot answer never burns an attempt on the new mailbox. Only a
    // row with no address skips it; an omitted `current_code` fails, since the
    // check is worthless if skipping it is the caller's choice.
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

    // Validate WITHOUT consuming: the code stays valid until the write succeeds,
    // so a DB failure is a clean 5xx and the code still works on retry (#518).
    match store.otp.check(trimmed, code, cfg, crate::util::now_unix()) {
        VerifyOutcome::Ok => {}
        VerifyOutcome::LockedOut => {
            // check() already deleted the code; drop the binding so a retry must
            // re-request.
            store.clear(trimmed);
            return Ok(EmailChangeOutcome::LockedOut);
        }
        VerifyOutcome::Invalid | VerifyOutcome::Expired | VerifyOutcome::NotFound => {
            return Ok(EmailChangeOutcome::InvalidCode);
        }
    }

    // Someone else may have claimed this email since the request.
    let mut rows = conn
        .query(
            "SELECT 1 FROM users WHERE email = ?1 AND id != ?2",
            libsql::params![trimmed.to_string(), authed.to_string()],
        )
        .await?;
    if rows.next().await?.is_some() {
        store.otp.consume(trimmed);
        store.clear(trimmed);
        return Ok(EmailChangeOutcome::EmailTaken);
    }

    conn.execute(
        "UPDATE users SET email = ?1 WHERE id = ?2",
        libsql::params![trimmed.to_string(), authed.to_string()],
    )
    .await?;
    store.otp.consume(trimmed);
    if let Some(current) = old_email.as_deref() {
        store.current_otp.consume(&normalize_email(current));
    }
    store.clear(trimmed);

    // Notify the account; neither notice can fail the already-committed change.
    // The security event is DS-authored so a client cannot suppress it and the
    // `/v1/security-events` append budget cannot bury it; the email reaches an
    // owner who has lost the device too.
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

/// Shared by the handler and the in-process harness so both speak the same
/// status codes.
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
