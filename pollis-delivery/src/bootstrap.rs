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
//! `user_id` is ALWAYS bound from the session record, never the body — the same
//! "actor can't write as someone else" property the device-signature path gets
//! from `resolve_actor`. All three land on the MAIN DB (`state.db`).

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::cert::verify_device_cert;
use crate::error::AuthRejection;
use crate::session::verify_session;
use crate::writes::bad_request;
use crate::AppState;

fn b64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.decode(s)?)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn ok_status() -> Response {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
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

// ── POST /v1/auth/establish-identity ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct EstablishIdentityBody {
    /// New account identity public key, base64 (STANDARD).
    pub account_id_pub: String,
    /// `account_recovery` blob, all base64 (STANDARD).
    pub salt: String,
    pub nonce: String,
    pub wrapped_key: String,
}

/// POST /v1/auth/establish-identity — version-1 account-identity establishment,
/// session-gated, signup-only. ONE transaction: a CAS `UPDATE users … WHERE id =
/// :session AND account_id_pub IS NULL` (0 rows ⇒ 409, an existing identity is
/// NEVER overwritten — reset has its own CAS-guarded path), plus the
/// `account_key_log` v1 append and the `account_recovery` insert. Mirrors
/// pollis-core `account_identity::generate_account_identity`'s writes.
pub async fn establish_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let claims = match verify_session(&headers, &state.sessions, now_unix()) {
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
    if pub_bytes.len() != 32 {
        return bad_request("account_id_pub must be 32 bytes");
    }

    let conn = match state.db.conn() {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    match apply_establish_identity(&conn, &claims.user_id, &pub_bytes, &salt, &nonce, &wrapped).await
    {
        Ok(EstablishOutcome::Applied) => ok_status(),
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

/// Version-1 account-identity establishment in ONE transaction (CAS UPDATE +
/// `account_key_log` v1 + `account_recovery`). Extracted from the handler so the
/// in-process integration harness drives the identical writes against the shared
/// main DB. `user_id` is the session-bound caller, never a body field.
pub async fn apply_establish_identity(
    conn: &libsql::Connection,
    user_id: &str,
    pub_bytes: &[u8],
    salt: &[u8],
    nonce: &[u8],
    wrapped: &[u8],
) -> anyhow::Result<EstablishOutcome> {
    let tx = conn.transaction().await?;

    // CAS: claim the identity only if none is set. 0 rows ⇒ already established ⇒
    // conflict. This is the invariant that makes "a re-login overwrites the
    // account key" unrepresentable.
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

// ── POST /v1/auth/register-device ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RegisterDeviceBody {
    /// Must equal the session's device. Bound from the session regardless; a
    /// mismatch is rejected so a token can't register some other device.
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
}

/// POST /v1/auth/register-device — INSERT the device row (COALESCE-preserving any
/// existing name) + seed conversation watermarks, all bound to the session's
/// `user_id`. Mirrors pollis-core `auth::register_device`'s remote writes.
pub async fn register_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let claims = match verify_session(&headers, &state.sessions, now_unix()) {
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
    let device_name = parsed
        .device_name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "device".to_string());

    let conn = match state.db.conn() {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    match apply_register_device(&conn, &claims.user_id, &device_id, &device_name).await {
        Ok(()) => ok_status(),
        Err(e) => internal(e),
    }
}

/// INSERT the device row (COALESCE-preserving any existing name) + seed
/// conversation watermarks, bound to the session's `user_id`. Extracted from the
/// handler so the in-process integration harness drives the identical writes
/// against the shared main DB.
pub async fn apply_register_device(
    conn: &libsql::Connection,
    user_id: &str,
    device_id: &str,
    device_name: &str,
) -> anyhow::Result<()> {
    let tx = conn.transaction().await?;

    tx.execute(
        "INSERT INTO user_device (device_id, user_id, device_name) VALUES (?1, ?2, ?3) \
         ON CONFLICT(device_id) DO UPDATE SET \
            last_seen = datetime('now'), \
            device_name = COALESCE(user_device.device_name, excluded.device_name)",
        libsql::params![device_id.to_string(), user_id.to_string(), device_name.to_string()],
    )
    .await?;

    // Seed watermark rows for every conversation the user already belongs to so a
    // new device doesn't retroactively block envelope cleanup. INSERT OR IGNORE —
    // mirrors auth.rs.
    tx.execute(
        "INSERT OR IGNORE INTO conversation_watermark \
            (conversation_id, user_id, device_id, last_fetched_at) \
         SELECT c.id, ?1, ?2, datetime('now') \
         FROM channels c \
         JOIN group_member gm ON gm.group_id = c.group_id AND gm.user_id = ?1",
        libsql::params![user_id.to_string(), device_id.to_string()],
    )
    .await?;
    tx.execute(
        "INSERT OR IGNORE INTO conversation_watermark \
            (conversation_id, user_id, device_id, last_fetched_at) \
         SELECT dcm.dm_channel_id, ?1, ?2, datetime('now') \
         FROM dm_channel_member dcm WHERE dcm.user_id = ?1",
        libsql::params![user_id.to_string(), device_id.to_string()],
    )
    .await?;

    tx.commit().await?;
    Ok(())
}

// ── POST /v1/auth/publish-device-cert ────────────────────────────────────────

#[derive(Deserialize)]
pub struct PublishCertBody {
    pub device_id: String,
    /// 64-byte Ed25519 device cert, base64 (STANDARD).
    pub device_cert: String,
    /// Unix seconds the cert was issued at (stored as TEXT — lossless u64 round
    /// trip for later verification, mirroring `device.rs`).
    pub cert_issued_at: i64,
    pub cert_identity_version: u32,
    /// Raw 32-byte MLS signing pubkey, base64 (STANDARD) — the column the
    /// device-signature gate verifies against.
    pub mls_signature_pub: String,
    /// The publishing user. IGNORED when a session is present (bound from the
    /// session instead). REQUIRED on the cert-validity-alone (subsequent-device)
    /// path, where there is no session to bind the user from — the cert
    /// verification against this user's `account_id_pub` is the load-bearing proof.
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/auth/publish-device-cert — the PIVOT write. DUAL gate, in both cases
/// the cert's Ed25519 signature is re-verified against the account's stored
/// `account_id_pub` (a 409 if no identity is established yet) before the
/// `user_device` cert columns are populated:
///
///   (a) **session + cert-validity** — first-device signup. `user_id`/`device_id`
///       are bound from the session (never the body); the session is invalidated
///       on success (single-use through the pivot).
///   (b) **cert-validity ALONE** — a subsequent device (sibling-approval
///       enrollment / Secret-Key recovery) whose session may have expired while
///       it waited for approval. No session header: `user_id`/`device_id` come
///       from the body and the cert verification IS the authentication. This is
///       strictly STRONGER than a bearer session — forging it needs the account
///       private key — and `apply_publish_device_cert` still requires a
///       pre-existing `user_device` row and never fails open.
///
/// Mirrors pollis-core `mls::device::ensure_device_cert`'s write.
pub async fn publish_device_cert(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let now = now_unix();

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
    if parsed.cert_issued_at < 0 {
        return bad_request("cert_issued_at must be non-negative");
    }
    let issued_at = parsed.cert_issued_at as u64;

    // Pick the gate: a live session (gate a) takes precedence; otherwise fall
    // back to cert-validity-alone with the body `user_id` (gate b). A token that
    // is present but expired/unknown does NOT resolve, so it cleanly falls
    // through to (b) — which is exactly the "session outlived by slow approval"
    // case the subsequent-device path is built for.
    let session_token = crate::session::session_token(&headers)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string());
    let session_claims = session_token
        .as_ref()
        .and_then(|t| state.sessions.resolve(t, now));

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

    let conn = match state.db.conn() {
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
    )
    .await
    {
        Ok(PublishCertOutcome::Applied) => {
            // Single-use through the pivot: invalidate the bootstrap session if
            // one was used (gate a). Gate (b) had no session to spend.
            if let Some(token) = invalidate_token {
                state.sessions.invalidate(&token);
            }
            ok_status()
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

/// The PIVOT write: verify the cert chains to the account's `account_id_pub`
/// (the cert-validity gate) then populate the `user_device` cert columns +
/// `mls_signature_pub`. Extracted from the handler so the in-process integration
/// harness drives the identical verify + write against the shared main DB.
/// Does NOT touch the session — the caller invalidates it on `Applied`.
pub async fn apply_publish_device_cert(
    conn: &libsql::Connection,
    user_id: &str,
    device_id: &str,
    cert_bytes: &[u8],
    issued_at: u64,
    cert_identity_version: u32,
    mls_sig_pub: &[u8],
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

    // Cert-validity gate: the cert must be a valid Ed25519 signature by the
    // account key over (device_id, mls_sig_pub, identity_version, issued_at).
    if !verify_device_cert(
        &account_id_pub,
        device_id,
        mls_sig_pub,
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
                 mls_signature_pub = ?4 \
             WHERE device_id = ?5 AND user_id = ?6",
            libsql::params![
                cert_bytes.to_vec(),
                issued_at.to_string(),
                cert_identity_version as i64,
                mls_sig_pub.to_vec(),
                device_id.to_string(),
                user_id.to_string(),
            ],
        )
        .await?;
    if affected == 0 {
        // The device row isn't the actor's (or doesn't exist) — register first.
        return Ok(PublishCertOutcome::DeviceNotRegistered);
    }

    Ok(PublishCertOutcome::Applied)
}

// ── POST /v1/auth/enrollment-request ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct EnrollmentRequestBody {
    pub request_id: String,
    /// New device's ephemeral X25519 public key, base64 (STANDARD). The private
    /// half never leaves the requesting device.
    pub new_device_ephemeral_pub: String,
    pub verification_code: String,
    pub created_at: String,
    pub expires_at: String,
}

/// POST /v1/auth/enrollment-request — INSERT a pending `device_enrollment_request`
/// for the session's user + device. SESSION-gated because the requesting device
/// is pre-credential (its `mls_signature_pub` is still NULL, so it cannot
/// device-sign). `user_id` and `new_device_id` are bound from the session, NEVER
/// the body — a token can only file an enrollment request for ITS OWN account +
/// device. Mirrors pollis-core `device_enrollment::start_device_enrollment`'s
/// remote write.
pub async fn enrollment_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let claims = match verify_session(&headers, &state.sessions, now_unix()) {
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

    let conn = match state.db.conn() {
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
        Ok(()) => {
            // Notify the user's already-enrolled devices via their inbox room.
            // The requesting device can't send this itself — it is
            // pre-enrollment (no signing credential, local DB closed), so its
            // client-side device-signed send-data fails with "not signed in".
            // The DS holds the LiveKit admin secret, so it emits the nudge here.
            // Best-effort: a miss is covered by the sibling's login-time
            // `list_pending_enrollment_requests` poll.
            let inbox = format!("inbox-{}", claims.user_id);
            let event = serde_json::json!({
                "type": "enrollment_requested",
                "request_id": parsed.request_id,
                "new_device_id": claims.device_id,
                "verification_code": parsed.verification_code,
            });
            if let Err(e) = crate::broker::room_send_data(&state, &inbox, &event).await {
                tracing::warn!("enrollment-request inbox notify failed (non-fatal): {e}");
            }
            ok_status()
        }
        Err(e) => internal(e),
    }
}

/// INSERT the pending enrollment request, `user_id` + `new_device_id` bound to
/// the session caller. Extracted from the handler so the in-process integration
/// harness drives the identical write against the shared main DB.
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
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO device_enrollment_request \
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
    Ok(())
}
