//! The OTP-session-gated bootstrap writes — the credential-*establishing* writes
//! that can't be device-signed (chicken-and-egg) and so are gated by a verified
//! [`crate::session`] token instead. See `docs/otp-server-bootstrap-design.md`.
//!
//!   - `POST /v1/auth/establish-identity` — version-1 account-identity (CAS:
//!     never overwrite an existing `account_id_pub`).
//!   - `POST /v1/auth/register-device`    — the device row + watermark seeds.
//!   - `POST /v1/auth/publish-device-cert`— the PIVOT: populate
//!     `mls_signature_pub` (the column the device-signature gate verifies
//!     against), gated by session AND cert-validity.
//!
//! `user_id` is ALWAYS bound from the session record, never the body.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use crate::cert::verify_device_cert;
use crate::error::AuthRejection;
use crate::session::{verify_session, verify_session_for_enrollment, SessionScope};
use crate::writes::bad_request;
use crate::AppState;
use crate::util::b64_decode;

// Request bodies live in `pollis-api`, shared with pollis-core; re-exported here.
pub use pollis_api::bootstrap::*;

/// The shared `{"status":"ok"}`, generic over the endpoint (#922).
fn ok_status<B>() -> Response
where
    B: pollis_api::DsRequest<Response = pollis_api::StatusOk>,
{
    crate::writes::ok_response::<B>(pollis_api::StatusOk::Ok)
}

fn conflict(msg: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({ "status": "conflict", "error": msg })),
    )
        .into_response()
}

fn internal(e: anyhow::Error) -> Response {
    tracing::error!("bootstrap internal error: {e:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "internal error" })),
    )
        .into_response()
}

/// POST /v1/auth/establish-identity — session-gated; ONE transaction, a CAS never overwriting an identity.
pub async fn establish_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let claims = match verify_session(&headers, &state.sessions, crate::util::now_unix()) {
        Ok(c) => c,
        Err(rej) => return rej.into_response(),
    };
    let parsed: EstablishIdentityBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let (pub_bytes, salt, nonce, wrapped) = match (
        b64_decode(&parsed.account_id_pub),
        b64_decode(&parsed.salt),
        b64_decode(&parsed.nonce),
        b64_decode(&parsed.wrapped_key),
    ) {
        (Ok(p), Ok(s), Ok(n), Ok(w)) => (p, s, n, w),
        _ => return bad_request("invalid base64 field"),
    };
    if pub_bytes.len() != pollis_device_cert::MLDSA44_PUB_LEN {
        return bad_request("account_id_pub must be an ML-DSA-44 verifying key");
    }

    let conn = match state.db.conn().await {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    match apply_establish_identity(&conn, &claims.user_id, &pub_bytes, &salt, &nonce, &wrapped).await
    {
        Ok(EstablishOutcome::Applied) => ok_status::<EstablishIdentityBody>(),
        Ok(EstablishOutcome::Conflict) => conflict("identity already established"),
        Err(e) => internal(e),
    }
}

/// Outcome of [`apply_establish_identity`] — `Conflict` is the CAS loss (an
/// identity already exists; never overwritten).
pub enum EstablishOutcome {
    Applied,
    Conflict,
}

/// Version-1 identity establishment in ONE transaction; `user_id` is the session-bound caller.
pub async fn apply_establish_identity(
    conn: &libsql::Connection,
    user_id: &str,
    pub_bytes: &[u8],
    salt: &[u8],
    nonce: &[u8],
    wrapped: &[u8],
) -> anyhow::Result<EstablishOutcome> {
    let tx = conn.transaction().await?;

    // CAS: claim the identity only if none is set, so a re-login can never overwrite the account key.
    let affected = tx
        .execute(
            "UPDATE users SET account_id_pub = ?1, identity_version = 1 \
             WHERE id = ?2 AND account_id_pub IS NULL",
            libsql::params![pub_bytes.to_vec(), user_id.to_string()],
        )
        .await?;
    if affected == 0 {
        // Nothing written; roll back and report the conflict.
        drop(tx);
        return Ok(EstablishOutcome::Conflict);
    }

    tx.execute(
        "INSERT INTO account_key_log (user_id, account_id_pub, identity_version) \
         VALUES (?1, ?2, 1)",
        libsql::params![user_id.to_string(), pub_bytes.to_vec()],
    )
    .await?;
    tx.execute(
        "INSERT INTO account_recovery \
         (user_id, identity_version, salt, nonce, wrapped_key, created_at, updated_at) \
         VALUES (?1, 1, ?2, ?3, ?4, datetime('now'), datetime('now'))",
        libsql::params![user_id.to_string(), salt.to_vec(), nonce.to_vec(), wrapped.to_vec()],
    )
    .await?;

    tx.commit().await?;
    Ok(EstablishOutcome::Applied)
}

/// POST /v1/auth/register-device — INSERT the device row, bound to the session's `user_id`.
pub async fn register_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // OTP or device-link session (#1207): a linked device registers here.
    let claims = match verify_session_for_enrollment(&headers, &state.sessions, crate::util::now_unix()) {
        Ok(c) => c,
        Err(rej) => return rej.into_response(),
    };
    let parsed: RegisterDeviceBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    // The session is bound to one device; a token can only register THAT device.
    if parsed.device_id.trim().is_empty() || parsed.device_id != claims.device_id {
        return AuthRejection::Forbidden.into_response();
    }
    let device_id = claims.device_id.clone();
    // Shipped clients send "{hostname} ({os})", and a hostname can embed the
    // machine's IP; no client IP may be stored, so redact rather than refuse.
    let device_name = parsed
        .device_name
        .filter(|s| !s.trim().is_empty())
        .map(|s| crate::util::redact_ip_literals(&s))
        .unwrap_or_else(|| "device".to_string());

    let conn = match state.db.conn().await {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    match apply_register_device(&conn, &claims.user_id, &device_id, &device_name).await {
        Ok(()) => {
            // Evict anyway, so every `user_device` write evicts, no exceptions (#658).
            state
                .device_keys
                .invalidate_device(&claims.user_id, &device_id);
            ok_status::<RegisterDeviceBody>()
        }
        Err(e) => internal(e),
    }
}

/// INSERT the device row (keeping any existing name), bound to the session's `user_id`.
///
/// #1092: no watermark seeding here; an unapproved ghost would pin retention.
pub async fn apply_register_device(
    conn: &libsql::Connection,
    user_id: &str,
    device_id: &str,
    device_name: &str,
) -> anyhow::Result<()> {
    // `device_id` may be another account's; the `WHERE` makes the upsert a no-op there (#1092).
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, device_name) VALUES (?1, ?2, ?3) \
         ON CONFLICT(device_id) DO UPDATE SET \
            last_seen = datetime('now'), \
            device_name = COALESCE(user_device.device_name, excluded.device_name) \
         WHERE user_device.user_id = excluded.user_id",
        libsql::params![device_id.to_string(), user_id.to_string(), device_name.to_string()],
    )
    .await?;
    Ok(())
}

/// Seed watermark rows for every conversation `user_id` already belongs to, so a
/// newly enrolled device does not retroactively block envelope cleanup.
///
/// `reported_at = datetime('now')` marks the device LIVE, so the #720 dormancy bound bites.
///
/// The cursor is `conversation_seq.next_seq`: the device has consumed the backlog (#1087).
///
/// Called at cert publish, not at register — see [`apply_register_device`].
pub async fn seed_conversation_watermarks(
    conn: &libsql::Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<()> {
    let cursor = crate::messages::seeded_watermark_cursor();
    conn.execute(
        "INSERT OR IGNORE INTO conversation_watermark \
            (conversation_id, user_id, device_id, last_fetched_at, last_seq, reported_at) \
         SELECT c.id, ?1, ?2, ?3, \
                COALESCE((SELECT next_seq FROM conversation_seq WHERE conversation_id = c.id), 0), \
                datetime('now') \
         FROM channels c \
         JOIN group_member gm ON gm.group_id = c.group_id AND gm.user_id = ?1",
        libsql::params![user_id.to_string(), device_id.to_string(), cursor.clone()],
    )
    .await?;
    conn.execute(
        "INSERT OR IGNORE INTO conversation_watermark \
            (conversation_id, user_id, device_id, last_fetched_at, last_seq, reported_at) \
         SELECT dcm.dm_channel_id, ?1, ?2, ?3, \
                COALESCE((SELECT next_seq FROM conversation_seq \
                           WHERE conversation_id = dcm.dm_channel_id), 0), \
                datetime('now') \
         FROM dm_channel_member dcm WHERE dcm.user_id = ?1",
        libsql::params![user_id.to_string(), device_id.to_string(), cursor],
    )
    .await?;
    Ok(())
}

/// POST /v1/auth/publish-device-cert — the PIVOT; the cert is always re-verified, under one of:
///
///   (a) **session + cert-validity** — first-device signup; ids from the session, then spent.
///   (b) **cert-validity ALONE** — a later device whose session expired; ids from the body.
///
/// Mirrors pollis-core `mls::device::ensure_device_cert`'s write.
pub async fn publish_device_cert(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let now = crate::util::now_unix();

    let parsed: PublishCertBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let cert_bytes = match b64_decode(&parsed.device_cert) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid device_cert"),
    };
    let mls_sig_pub = match b64_decode(&parsed.mls_signature_pub) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid mls_signature_pub"),
    };
    let mls_sig_pub_pq = match b64_decode(&parsed.mls_signature_pub_pq) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid mls_signature_pub_pq"),
    };
    if parsed.cert_issued_at < 0 {
        return bad_request("cert_issued_at must be non-negative");
    }
    let issued_at = parsed.cert_issued_at as u64;

    // A live session takes gate (a); an absent or dead token falls through to (b).
    let session_token = crate::session::session_token(&headers)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string());
    // Gate (a) takes an OTP session only; a device-link session falls through to (b).
    let session_claims = session_token
        .as_ref()
        .and_then(|t| state.sessions.resolve(t, now))
        .filter(|c| c.scope == SessionScope::Otp);

    let (user_id, device_id, invalidate_token) = match session_claims {
        Some(claims) => {
            // Gate (a): bind from the session; the body device_id must match.
            if parsed.device_id != claims.device_id {
                return AuthRejection::Forbidden.into_response();
            }
            (claims.user_id, claims.device_id, session_token)
        }
        None => {
            // Gate (b): no live session — the user_id MUST come from the body and
            // the cert-validity check below is the load-bearing proof.
            let uid = match parsed.user_id.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(u) => u.to_string(),
                None => return AuthRejection::Unauthorized.into_response(),
            };
            (uid, parsed.device_id.clone(), None)
        }
    };

    let conn = match state.db.conn().await {
        Ok(c) => c,
        Err(e) => return internal(e),
    };

    match apply_publish_device_cert(
        &conn,
        &user_id,
        &device_id,
        &cert_bytes,
        issued_at,
        parsed.cert_identity_version,
        &mls_sig_pub,
        &mls_sig_pub_pq,
    )
    .await
    {
        Ok(PublishCertOutcome::Applied) => {
            // Single-use through the pivot (gate a).
            if let Some(token) = invalidate_token {
                state.sessions.invalidate(&token);
            }
            // This rewrites `mls_signature_pub_pq`, so evict the cached key (#658).
            state.device_keys.invalidate_device(&user_id, &device_id);
            ok_status::<PublishCertBody>()
        }
        Ok(PublishCertOutcome::IdentityNotEstablished) => {
            conflict("account identity not established")
        }
        Ok(PublishCertOutcome::CertInvalid) => AuthRejection::Unauthorized.into_response(),
        Ok(PublishCertOutcome::DeviceNotRegistered) => {
            conflict("device not registered for this user")
        }
        Err(e) => internal(e),
    }
}

/// Outcome of [`apply_publish_device_cert`]. `CertInvalid` is the cert-validity
/// gate failing (401); the two conflicts are out-of-order bootstrap (409).
pub enum PublishCertOutcome {
    Applied,
    IdentityNotEstablished,
    CertInvalid,
    DeviceNotRegistered,
}

/// The PIVOT write: verify the cert, then fill the `user_device` cert columns. Session untouched.
// The full cert-publication tuple as it arrives on the wire; the handler and the
// integration harness must both pass exactly this, so it stays flat.
#[allow(clippy::too_many_arguments)]
pub async fn apply_publish_device_cert(
    conn: &libsql::Connection,
    user_id: &str,
    device_id: &str,
    cert_bytes: &[u8],
    issued_at: u64,
    cert_identity_version: u32,
    mls_sig_pub: &[u8],
    mls_sig_pub_pq: &[u8],
) -> anyhow::Result<PublishCertOutcome> {
    // The account_id_pub the cert must chain to. Absent/NULL ⇒ identity not yet
    // established ⇒ out of order.
    let account_id_pub: Vec<u8> = {
        let mut rows = conn
            .query(
                "SELECT account_id_pub FROM users WHERE id = ?1",
                libsql::params![user_id.to_string()],
            )
            .await?;
        match rows.next().await? {
            Some(row) => match row.get::<Option<Vec<u8>>>(0)? {
                Some(p) => p,
                None => return Ok(PublishCertOutcome::IdentityNotEstablished),
            },
            None => return Ok(PublishCertOutcome::IdentityNotEstablished),
        }
    };

    // Cert-validity gate: the cert must be a valid ML-DSA-44 signature by the
    // account key over (device_id, both leaf pubs, identity_version, issued_at).
    if !verify_device_cert(
        &account_id_pub,
        device_id,
        mls_sig_pub,
        mls_sig_pub_pq,
        cert_identity_version,
        issued_at,
        cert_bytes,
    ) {
        return Ok(PublishCertOutcome::CertInvalid);
    }

    // Bound to the session user; populating `mls_signature_pub` is what makes the
    // device device-signature-authenticatable from here on.
    let affected = conn
        .execute(
            "UPDATE user_device \
             SET device_cert = ?1, cert_issued_at = ?2, cert_identity_version = ?3, \
                 mls_signature_pub = ?4, mls_signature_pub_pq = ?5 \
             WHERE device_id = ?6 AND user_id = ?7",
            libsql::params![
                cert_bytes.to_vec(),
                issued_at.to_string(),
                cert_identity_version as i64,
                mls_sig_pub.to_vec(),
                mls_sig_pub_pq.to_vec(),
                device_id.to_string(),
                user_id.to_string(),
            ],
        )
        .await?;
    if affected == 0 {
        // The device row isn't the actor's (or doesn't exist) — register first.
        return Ok(PublishCertOutcome::DeviceNotRegistered);
    }

    // #1092: here the device becomes real, so here it starts pinning retention.
    seed_conversation_watermarks(conn, user_id, device_id).await?;

    Ok(PublishCertOutcome::Applied)
}

/// POST /v1/auth/enrollment-request — SESSION-gated (pre-credential); ids bound from the session.
pub async fn enrollment_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // OTP or device-link session (#1207): a linked device files its request here.
    let claims = match verify_session_for_enrollment(&headers, &state.sessions, crate::util::now_unix()) {
        Ok(c) => c,
        Err(rej) => return rej.into_response(),
    };
    let parsed: EnrollmentRequestBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let ephemeral_pub = match b64_decode(&parsed.new_device_ephemeral_pub) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid new_device_ephemeral_pub"),
    };

    // QR device link (#1207): a device-link session carries its link's tag; an OTP session never does.
    // A v2 offer session (#1361) carries NO tag.
    let link: Option<(String, Option<String>)> = match (&claims.scope, &parsed.link_tag) {
        (SessionScope::DeviceLink { link_id }, Some(tag)) if !tag.is_empty() => {
            Some((link_id.clone(), Some(tag.clone())))
        }
        (SessionScope::DeviceLink { .. }, _) => {
            return bad_request("link_tag is required on a device-link session")
        }
        (SessionScope::DeviceOffer { link_id }, None) => Some((link_id.clone(), None)),
        (SessionScope::DeviceOffer { .. }, Some(_)) => {
            return bad_request("link_tag is not valid on a device-offer session")
        }
        (SessionScope::Otp, Some(_)) => {
            return bad_request("link_tag is only valid on a device-link session")
        }
        (SessionScope::Otp, None) => None,
    };
    let conn = match state.db.conn().await {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    match apply_enrollment_request(
        &conn,
        &claims.user_id,
        &claims.device_id,
        &parsed.request_id,
        &ephemeral_pub,
        &parsed.verification_code,
        &parsed.created_at,
        &parsed.expires_at,
    )
    .await
    {
        // A resubmit of the caller's OWN request is answered the same, re-emitting the nudge.
        Ok(EnrollmentRequestOutcome::Created | EnrollmentRequestOutcome::Duplicate) => {
            // Attach the request to its link so the device showing the QR sees it.
            if let Some((link_id, tag)) = &link {
                let now = crate::util::now_unix();
                if !state.links.record_request(link_id, &claims.device_id, &parsed.request_id, tag.as_deref(), now) {
                    tracing::info!("enrollment-request: link {link_id} not attachable (expired or already attached)");
                }
            }
            // Nudge enrolled devices via the inbox; the requester can't (pre-credential). Best-effort.
            let inbox = format!("inbox-{}", claims.user_id);
            // `link_id` (#1207): approved on the link tag, so no typed-code prompt.
            let link_id = link.as_ref().map(|(id, _)| id.clone());
            let event = serde_json::json!({
                "type": "enrollment_requested",
                "request_id": parsed.request_id,
                "new_device_id": claims.device_id,
                "verification_code": parsed.verification_code,
                "link_id": link_id,
            });
            if let Err(e) = crate::broker::room_send_data(&state, &inbox, &event).await {
                tracing::warn!("enrollment-request inbox notify failed (non-fatal): {e}");
            }
            ok_status::<EnrollmentRequestBody>()
        }
        Ok(EnrollmentRequestOutcome::Conflict) => conflict("request_id_taken"),
        Err(e) => internal(e),
    }
}

/// What a `POST /v1/auth/enrollment-request` did.
#[derive(Debug, PartialEq, Eq)]
pub enum EnrollmentRequestOutcome {
    /// The row was inserted.
    Created,
    /// The id names THIS session's own pending request: a retry, answered 200.
    Duplicate,
    /// The id names someone else's request: 409, as the row holds the verification code.
    Conflict,
}

/// INSERT the pending enrollment request, bound to the session caller.
#[allow(clippy::too_many_arguments)]
pub async fn apply_enrollment_request(
    conn: &libsql::Connection,
    user_id: &str,
    new_device_id: &str,
    request_id: &str,
    ephemeral_pub: &[u8],
    verification_code: &str,
    created_at: &str,
    expires_at: &str,
) -> anyhow::Result<EnrollmentRequestOutcome> {
    // `OR IGNORE`: a duplicate client-chosen id is resolved by reading the winner, never overwritten.
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO device_enrollment_request \
             (id, user_id, new_device_id, new_device_ephemeral_pub, verification_code, \
              status, created_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7)",
            libsql::params![
                request_id.to_string(),
                user_id.to_string(),
                new_device_id.to_string(),
                ephemeral_pub.to_vec(),
                verification_code.to_string(),
                created_at.to_string(),
                expires_at.to_string(),
            ],
        )
        .await?;
    if inserted > 0 {
        return Ok(EnrollmentRequestOutcome::Created);
    }

    let mut rows = conn
        .query(
            "SELECT user_id, new_device_id FROM device_enrollment_request WHERE id = ?1",
            libsql::params![request_id.to_string()],
        )
        .await?;
    let owner = match rows.next().await? {
        Some(row) => Some((row.get::<String>(0)?, row.get::<String>(1)?)),
        None => None,
    };
    Ok(match owner {
        // The row vanished since the INSERT; refuse.
        None => EnrollmentRequestOutcome::Conflict,
        Some((owner_user, owner_device)) => {
            if owner_user == user_id && owner_device == new_device_id {
                EnrollmentRequestOutcome::Duplicate
            } else {
                EnrollmentRequestOutcome::Conflict
            }
        }
    })
}
