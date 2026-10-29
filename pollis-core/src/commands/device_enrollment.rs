//! Device enrollment: an existing device hands the account identity key to a
//! new one over an ephemeral X25519 channel, authenticated by an 8-character
//! SAS the human reads off the new device and types into the approver.
//!
//! 1. New device: `start_device_enrollment` writes the request (session-gated;
//!    it cannot device-sign yet) and displays the SAS. The DS sends the
//!    `enrollment_requested` nudge server-side.
//! 2. Existing device: `approve_device_enrollment` re-derives the SAS from the
//!    ephemeral key it fetched, checks the typed value, and wraps the key
//!    (ECDH, HKDF, AES-256-GCM).
//! 3. New device: `poll_enrollment_status` unwraps, checks the seed against the
//!    published `account_id_pub`, and `finalize_enrollment` later publishes its
//!    cert and joins groups.
//!
//! This is device linking (as in Signal/WhatsApp), not a login second factor:
//! it runs once per new device because no server ever holds the identity key.
//! Every tier can both request and approve. Protocol: `docs/security-whitepaper.md`.

use std::sync::Arc;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use ulid::Ulid;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Pending-request lifetime: long enough to walk to the other device, short
/// enough to bound exposure of an observed code.
const ENROLLMENT_TTL_SECS: i64 = 10 * 60;

/// HKDF info binding the wrap key to this protocol.
const ENROLL_HKDF_INFO: &[u8] = b"pollis-enrollment-wrap-v1";

/// HKDF info for the SAS (#793), domain-separated from the wrap key.
const ENROLL_SAS_INFO: &[u8] = b"pollis-enrollment-sas-v1";

/// Domain tag opening the per-request SAS salt; both ends must agree on its
/// shape byte for byte.
const ENROLL_SAS_SALT_DOMAIN: &[u8] = b"pollis-enrollment-sas-salt-v1";

/// Crockford-style base32 without I, L, O, U: 5 bits per character.
const SAS_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 40 bits: the server can grind ephemeral keys toward a target code, so the
/// width is the margin (six digits would be ~2^20 keygens, seconds of work).
const SAS_LEN: usize = 8;

/// [`SAS_LEN`], for the UIs that gate the typed input on it.
pub const ENROLLMENT_SAS_LEN: usize = SAS_LEN;

/// [`SAS_ALPHABET`] as a `&str`, for UI input validation.
pub const ENROLLMENT_SAS_ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Normalize a typed SAS (#1096): upper-case, drop separators, truncate at
/// [`ENROLLMENT_SAS_LEN`].
///
/// A character outside the alphabet is dropped, never mapped to a lookalike:
/// `O`->`0` would let a mis-read code compare equal. Twins in
/// `frontend/src/utils/enrollmentSas.ts` and `mobile/lib/enrollmentSas.ts` are
/// pinned to these constants by their unit tests.
pub fn normalize_enrollment_sas(raw: &str) -> String {
    let mut out = String::with_capacity(SAS_LEN);
    for ch in raw.chars().flat_map(|c| c.to_uppercase()) {
        if ENROLLMENT_SAS_ALPHABET.contains(ch) {
            out.push(ch);
            if out.chars().count() == SAS_LEN {
                break;
            }
        }
    }
    out
}

const X25519_PUB_LEN: usize = 32;
const AES_NONCE_LEN: usize = 12;
const ACCOUNT_ID_PRIVATE_LEN: usize = 32;
// approver_pub || nonce || ciphertext || 16-byte GCM tag
const WRAPPED_ACCOUNT_KEY_LEN: usize = X25519_PUB_LEN + AES_NONCE_LEN + ACCOUNT_ID_PRIVATE_LEN + 16;

#[derive(Debug, Serialize, Deserialize)]
pub struct EnrollmentHandle {
    pub request_id: String,
    pub verification_code: String,
    pub expires_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EnrollmentStatus {
    /// Still waiting for an existing device to approve.
    Pending,
    /// `account_id_key` is installed locally; the frontend proceeds.
    Approved,
    /// An existing device rejected the request.
    Rejected,
    /// TTL elapsed.
    Expired,
}

/// One open enrollment request, as the approving device sees it.
///
/// Carries no verification code (#1096): the approver types the one shown on
/// the new device, so there is nothing for a UI to pre-fill.
#[derive(Debug, Serialize, Deserialize)]
pub struct PendingEnrollmentRequest {
    pub request_id: String,
    pub new_device_id: String,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SecurityEvent {
    pub id: String,
    pub kind: String,
    pub device_id: Option<String>,
    pub created_at: String,
    pub metadata: Option<String>,
}

/// Keyset position in the security-event log (#1548); the UI passes
/// `next_cursor` back verbatim.
pub use pollis_api::account_reads::SecurityEventCursor;

/// One page of the security-event log, newest first (#1548).
#[derive(Debug, Serialize, Deserialize)]
pub struct SecurityEventsPage {
    pub events: Vec<SecurityEvent>,
    /// Pass back as `before` for the next page; `None` = this page reached the
    /// oldest matching row.
    pub next_cursor: Option<SecurityEventCursor>,
}

/// The SAS, derived from the new device's ephemeral public key (#793) so a
/// substituted key changes the code the approver derives. Not a secret; its
/// job is binding.
///
/// Salted per request ([`sas_salt`]) so a key-to-code grinding table cannot be
/// built once and reused: the ~2^40 work must be redone inside each TTL.
fn derive_verification_code(
    ephemeral_pub: &[u8],
    request_id: &str,
    user_id: &str,
    created_at: &str,
) -> String {
    let salt = sas_salt(request_id, user_id, created_at);
    let hk = Hkdf::<Sha256>::new(Some(&salt), ephemeral_pub);
    let mut out = [0u8; SAS_LEN];
    hk.expand(ENROLL_SAS_INFO, &mut out)
        .expect("HKDF expand SAS_LEN bytes is infallible");
    out.iter()
        .map(|b| SAS_ALPHABET[(b & 0x1f) as usize] as char)
        .collect()
}

/// Length-prefixed so the encoding is injective: two different field triples
/// cannot share a salt.
fn sas_salt(request_id: &str, user_id: &str, created_at: &str) -> Vec<u8> {
    let mut salt = Vec::new();
    salt.extend_from_slice(ENROLL_SAS_SALT_DOMAIN);
    for field in [request_id, user_id, created_at] {
        salt.extend_from_slice(&(field.len() as u32).to_be_bytes());
        salt.extend_from_slice(field.as_bytes());
    }
    salt
}

/// Wrap key bound to both ephemeral public keys, not just the ECDH output.
fn derive_wrap_key(shared_secret: &[u8], requester_pub: &[u8], approver_pub: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, shared_secret);
    let mut info = Vec::with_capacity(ENROLL_HKDF_INFO.len() + requester_pub.len() + approver_pub.len());
    info.extend_from_slice(ENROLL_HKDF_INFO);
    info.extend_from_slice(requester_pub);
    info.extend_from_slice(approver_pub);
    let mut out = [0u8; 32];
    hk.expand(&info, &mut out)
        .expect("HKDF expand 32 bytes is infallible");
    out
}

fn aead_encrypt(key: &[u8; 32], nonce: &[u8; AES_NONCE_LEN], pt: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    cipher
        .encrypt(GenericArray::from_slice(nonce), pt)
        .map_err(|e| Error::Crypto(format!("enrollment aes-gcm encrypt: {e}")))
}

fn aead_decrypt(key: &[u8; 32], nonce: &[u8; AES_NONCE_LEN], ct: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    cipher
        .decrypt(GenericArray::from_slice(nonce), ct)
        .map_err(|e| Error::Crypto(format!("enrollment aes-gcm decrypt: {e}")))
}

/// Refuse a low-order peer key: it forces a zero shared secret, so the DS could
/// compute the wrap key and seal a seed of its choosing. `x25519-dalek` 2.x
/// leaves this check to the caller. Both peer keys are DS-served, so both
/// sides check.
fn require_contributory(shared: &x25519_dalek::SharedSecret, whose: &str) -> Result<()> {
    if !shared.was_contributory() {
        return Err(Error::Crypto(format!(
            "enrollment key exchange is non-contributory: {whose} is a low-order point, so the \
             shared secret is computable by anyone who saw the public keys"
        )));
    }
    Ok(())
}

fn x25519_private_from_bytes(bytes: &[u8]) -> Result<StaticSecret> {
    if bytes.len() != 32 {
        return Err(Error::Crypto(format!(
            "x25519 private key has wrong length: {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(bytes);
    Ok(StaticSecret::from(arr))
}

fn x25519_public_from_bytes(bytes: &[u8]) -> Result<PublicKey> {
    if bytes.len() != X25519_PUB_LEN {
        return Err(Error::Crypto(format!(
            "x25519 public key has wrong length: {}",
            bytes.len()
        )));
    }
    let mut arr = [0u8; X25519_PUB_LEN];
    arr.copy_from_slice(bytes);
    Ok(PublicKey::from(arr))
}

/// New-device side. Write a pending enrollment request and return the handle
/// to poll.
pub async fn start_device_enrollment(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<EnrollmentHandle> {
    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("device_id not set — login incomplete"))?;

    // A device signed in by showing a QR (#1361) must use the key that QR
    // carried: the scanning device wraps only to it.
    let offered_secret = match state.device_link_pending.lock().await.as_ref().map(|l| &l.kind) {
        Some(crate::commands::device_link::PendingLinkKind::Offered { ephemeral_secret }) => {
            Some(**ephemeral_secret)
        }
        _ => None,
    };
    let mut private_bytes = [0u8; 32];
    match offered_secret {
        Some(secret) => private_bytes = secret,
        None => OsRng.fill_bytes(&mut private_bytes),
    }
    let ephemeral_secret = StaticSecret::from(private_bytes);
    let ephemeral_public = PublicKey::from(&ephemeral_secret);

    let request_id = Ulid::new().to_string();
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::seconds(ENROLLMENT_TTL_SECS);
    let expires_at_str = expires_at.to_rfc3339();
    // Sent verbatim and re-derived by the approver, so this exact spelling
    // must travel.
    let created_at_str = now.to_rfc3339();

    let verification_code: String = derive_verification_code(
        ephemeral_public.as_bytes(),
        &request_id,
        &user_id,
        &created_at_str,
    );

    // Session-gated: this device is pre-enrollment (`mls_signature_pub` NULL)
    // and cannot device-sign. The DS binds `user_id` and `new_device_id` from
    // the session.
    let enrollment_session = state.enrollment_session.lock().await.clone();
    match enrollment_session {
        Some(token) => {
            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD;
            // QR device link (#1207): tag the request with the QR-keyed MAC the
            // approver verifies instead of a typed code. A v2 offer (#1361) has no tag.
            let link_tag = state.device_link_pending.lock().await.as_ref().and_then(|link| match &link.kind {
                crate::commands::device_link::PendingLinkKind::Claimed { mac_key } => {
                    Some(b64.encode(crate::commands::device_link::link_tag(
                        mac_key,
                        &link.link_id,
                        &request_id,
                        &device_id,
                        ephemeral_public.as_bytes(),
                    )))
                }
                crate::commands::device_link::PendingLinkKind::Offered { .. } => None,
            });
            let body = pollis_api::bootstrap::EnrollmentRequestBody {
                request_id: request_id.clone(),
                new_device_ephemeral_pub: b64.encode(ephemeral_public.as_bytes()),
                verification_code: verification_code.clone(),
                created_at: created_at_str.clone(),
                expires_at: expires_at_str.clone(),
                link_tag,
            };
            crate::commands::mls::ds_post_session_ok(
                state,
                &token,
                &body,
            )
            .await?;
        }
        None => {
            // The in-memory session is gone (e.g. an app restart); re-requesting the
            // OTP re-mints it.
            return Err(Error::Other(anyhow::anyhow!(
                "Your sign-in session expired. Please sign in again to enroll this device."
            )));
        }
    }

    state
        .enrollment_ephemeral_keys
        .lock()
        .await
        .insert(request_id.clone(), private_bytes.to_vec());

    // The DS sends the `enrollment_requested` nudge: this device has no
    // signing credential to send it.
    crate::diag!(
        "[enrollment] requested enrollment for user {user_id} device {device_id} (request {request_id})"
    );

    Ok(EnrollmentHandle {
        request_id,
        verification_code,
        expires_at: expires_at_str,
    })
}

/// New-device side. Read the request's status; on `Approved`, unwrap and
/// adopt `account_id_key` (finalization runs after `set_pin`).
pub async fn poll_enrollment_status(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    // Gated on possession of `request_id` (#987): this device has no signing
    // key and its OTP session has already expired.
    let row = crate::commands::ds_reads::enrollment_request(state, &request_id, false)
        .await?
        .ok_or_else(|| {
            Error::Other(anyhow::anyhow!("enrollment request {request_id} not found"))
        })?;
    let user_id = row.user_id;
    let status = row.status;
    let expires_at_str = row.expires_at;
    let wrapped = match row.wrapped_account_key.as_deref() {
        Some(b64) => Some(crate::commands::ds_reads::decode_b64(
            "wrapped_account_key",
            b64,
        )?),
        None => None,
    };

    // Expired: short-circuit without touching the status column.
    if let Ok(expires_at) = chrono::DateTime::parse_from_rfc3339(&expires_at_str) {
        if chrono::Utc::now() > expires_at.with_timezone(&chrono::Utc) {
            state
                .enrollment_ephemeral_keys
                .lock()
                .await
                .remove(&request_id);
            return Ok(EnrollmentStatus::Expired);
        }
    }

    match status.as_str() {
        "pending" => Ok(EnrollmentStatus::Pending),
        "rejected" => {
            state
                .enrollment_ephemeral_keys
                .lock()
                .await
                .remove(&request_id);
            Ok(EnrollmentStatus::Rejected)
        }
        "expired" => {
            state
                .enrollment_ephemeral_keys
                .lock()
                .await
                .remove(&request_id);
            Ok(EnrollmentStatus::Expired)
        }
        "approved" => {
            let wrapped = wrapped.ok_or_else(|| {
                Error::Other(anyhow::anyhow!(
                    "enrollment request {request_id} is approved but wrapped_account_key is NULL"
                ))
            })?;

            let priv_bytes = {
                let mut keys = state.enrollment_ephemeral_keys.lock().await;
                keys.remove(&request_id).ok_or_else(|| {
                    Error::Other(anyhow::anyhow!(
                        "no in-memory ephemeral key for request {request_id} — did the app restart during enrollment?"
                    ))
                })?
            };

            let account_id_private =
                unwrap_account_key(&wrapped, &priv_bytes)?;

            // Next is pin-create, where `set_pin` wraps these bytes and opens the local
            // DB. The adopt call refuses a seed whose public half is not the account's
            // published key: an AEAD open proves who the blob was sealed to, not which
            // seed is inside.
            crate::commands::account_identity::adopt_recovered_account_identity(
                state,
                &user_id,
                &account_id_private,
            )
            .await?;

            Ok(EnrollmentStatus::Approved)
        }
        other => Err(Error::Other(anyhow::anyhow!(
            "enrollment request {request_id} has unexpected status {other}"
        ))),
    }
}

/// First delay after a `Pending` answer in [`await_enrollment_approval`].
const AWAIT_BACKOFF_START_MS: u64 = 1_000;
/// Backoff ceiling: approval is a human on another device, so seconds of
/// latency are invisible and a full-TTL wait costs a handful of reads.
const AWAIT_BACKOFF_MAX_MS: u64 = 8_000;

/// Wait for an enrollment request to reach a terminal state (#874).
///
/// The new device cannot subscribe to realtime, so the answer is fetched here
/// with backoff bounded by the request's TTL; the renderer awaits one promise
/// instead of polling.
pub async fn await_enrollment_approval(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    // Past the TTL so expiry is reported by the normal status check.
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(ENROLLMENT_TTL_SECS as u64 + 5);
    let mut delay = std::time::Duration::from_millis(AWAIT_BACKOFF_START_MS);

    loop {
        match poll_enrollment_status(state, request_id.clone()).await? {
            EnrollmentStatus::Pending => {}
            terminal => return Ok(terminal),
        }
        if std::time::Instant::now() + delay >= deadline {
            return Ok(EnrollmentStatus::Expired);
        }
        tokio::time::sleep(delay).await;
        delay = std::cmp::min(
            delay * 2,
            std::time::Duration::from_millis(AWAIT_BACKOFF_MAX_MS),
        );
    }
}

fn unwrap_account_key(wrapped: &[u8], requester_private: &[u8]) -> Result<Vec<u8>> {
    if wrapped.len() != WRAPPED_ACCOUNT_KEY_LEN {
        return Err(Error::Crypto(format!(
            "wrapped_account_key has wrong length: {} (expected {})",
            wrapped.len(),
            WRAPPED_ACCOUNT_KEY_LEN
        )));
    }
    let approver_pub_bytes = &wrapped[..X25519_PUB_LEN];
    let nonce_bytes = &wrapped[X25519_PUB_LEN..X25519_PUB_LEN + AES_NONCE_LEN];
    let ct = &wrapped[X25519_PUB_LEN + AES_NONCE_LEN..];

    let requester_priv = x25519_private_from_bytes(requester_private)?;
    let requester_pub = PublicKey::from(&requester_priv);
    let approver_pub = x25519_public_from_bytes(approver_pub_bytes)?;
    let shared = requester_priv.diffie_hellman(&approver_pub);
    require_contributory(&shared, "the approver_pub in the wrapped blob")?;
    // Same transcript the approver bound in: our own public key, then theirs.
    let wrap_key = derive_wrap_key(
        shared.as_bytes(),
        requester_pub.as_bytes(),
        approver_pub.as_bytes(),
    );

    let mut nonce = [0u8; AES_NONCE_LEN];
    nonce.copy_from_slice(nonce_bytes);
    let pt = aead_decrypt(&wrap_key, &nonce, ct)?;

    if pt.len() != ACCOUNT_ID_PRIVATE_LEN {
        return Err(Error::Crypto(format!(
            "decrypted account_id_key has wrong length: {}",
            pt.len()
        )));
    }
    Ok(pt)
}

/// Existing-device side. Any device of the same user lists open requests
/// (called on login as a fallback in case the inbox push was missed).
pub async fn list_pending_enrollment_requests(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<Vec<PendingEnrollmentRequest>> {
    // The DS filters to pending, unexpired requests on the signer's own account.
    let rows = crate::commands::ds_reads::pending_enrollments(state, &user_id).await?;
    Ok(rows
        .into_iter()
        .map(|r| PendingEnrollmentRequest {
            request_id: r.id,
            new_device_id: r.new_device_id,
            created_at: r.created_at,
            expires_at: r.expires_at,
        })
        .collect())
}

/// Existing-device side. Check the typed SAS, then wrap `account_id_key` to
/// the requester's ephemeral key and mark the request approved.
pub async fn approve_device_enrollment(
    state: &Arc<AppState>,
    request_id: String,
    verification_code: String,
) -> Result<()> {
    let approver_device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("device_id not set — login incomplete"))?;

    // Device-signed with `want_approval_fields` (#987): the DS serves the
    // ephemeral key and code only to a signed device on the request's account.
    let row = crate::commands::ds_reads::enrollment_request(state, &request_id, true)
        .await?
        .ok_or_else(|| {
            Error::Other(anyhow::anyhow!("enrollment request {request_id} not found"))
        })?;

    let user_id: String = row.user_id;
    let new_device_id: String = row.new_device_id;
    let ephemeral_pub: Vec<u8> = match row.new_device_ephemeral_pub.as_deref() {
        Some(b64) => crate::commands::ds_reads::decode_b64("new_device_ephemeral_pub", b64)?,
        None => {
            return Err(Error::Other(anyhow::anyhow!(
                "enrollment request {request_id} has no ephemeral key"
            )))
        }
    };
    let stored_code: String = row.verification_code.unwrap_or_default();
    let status: String = row.status;
    let expires_at_str: String = row.expires_at;
    let created_at_str: String = row.created_at;

    if status != "pending" {
        return Err(Error::Other(anyhow::anyhow!(
            "enrollment request {request_id} is not pending (status={status})"
        )));
    }

    if let Ok(expires_at) = chrono::DateTime::parse_from_rfc3339(&expires_at_str) {
        if chrono::Utc::now() > expires_at.with_timezone(&chrono::Utc) {
            return Err(Error::Other(anyhow::anyhow!(
                "enrollment request {request_id} has expired"
            )));
        }
    }

    // #793: compare against the code derived from the key this device fetched,
    // never the server's stored copy. The salt strings come off the DS row too,
    // so altering any of them diverges from the code on the new device's screen.
    let expected_code = derive_verification_code(
        &ephemeral_pub,
        &request_id,
        &user_id,
        &created_at_str,
    );

    // Constant-time comparison to avoid leaking prefix-match timing.
    if !constant_time_eq(expected_code.as_bytes(), verification_code.as_bytes()) {
        return Err(Error::Other(anyhow::anyhow!(
            "verification code does not match"
        )));
    }

    // A stored copy that disagrees means the key was altered after publishing;
    // report it rather than passing silently.
    if !constant_time_eq(stored_code.as_bytes(), expected_code.as_bytes()) {
        return Err(Error::Other(anyhow::anyhow!(
            "enrollment request {request_id}: the stored verification code does not match \
             the one derived from the published ephemeral key — the request was altered in transit"
        )));
    }

    wrap_and_approve(
        state,
        &approver_device_id,
        &request_id,
        &user_id,
        &new_device_id,
        &ephemeral_pub,
        "approval",
    )
    .await

}

/// Wrap `account_id_key` to a new device's ephemeral key and post the approval.
///
/// Callers MUST have authenticated `ephemeral_pub` (the typed SAS, or the QR
/// link tag in `device_link::approve_device_link`, #1207). `via` lands in the
/// security event.
pub(crate) async fn wrap_and_approve(
    state: &Arc<AppState>,
    approver_device_id: &str,
    request_id: &str,
    user_id: &str,
    new_device_id: &str,
    ephemeral_pub: &[u8],
    via: &str,
) -> Result<()> {
    let request_id = request_id.to_string();
    let user_id = user_id.to_string();
    let new_device_id = new_device_id.to_string();
    let signing_key =
        crate::commands::account_identity::load_account_id_key(state, &user_id).await?;
    // The ML-DSA-44 private key is its 32-byte seed.
    let account_id_private = signing_key.to_seed().to_vec();

    let wrapped = {
        let mut rng = OsRng;

        let mut approver_priv_bytes = [0u8; 32];
        rng.fill_bytes(&mut approver_priv_bytes);
        let approver_priv = StaticSecret::from(approver_priv_bytes);
        let approver_pub = PublicKey::from(&approver_priv);

        let requester_pub = x25519_public_from_bytes(ephemeral_pub)?;
        let shared = approver_priv.diffie_hellman(&requester_pub);
        require_contributory(&shared, "the request's published ephemeral key")?;
        let wrap_key = derive_wrap_key(
            shared.as_bytes(),
            requester_pub.as_bytes(),
            approver_pub.as_bytes(),
        );

        let mut nonce = [0u8; AES_NONCE_LEN];
        rng.fill_bytes(&mut nonce);
        let ct = aead_encrypt(&wrap_key, &nonce, &account_id_private)?;

        let mut out = Vec::with_capacity(WRAPPED_ACCOUNT_KEY_LEN);
        out.extend_from_slice(approver_pub.as_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    };

    // The new device signs and publishes its own cert in `finalize_enrollment`.

    // The DS binds the request to the signer's account.
    let metadata = format!("via={via},approver={approver_device_id}");
    {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let body = pollis_api::account::ApproveEnrollmentBody {
            request_id: request_id.clone(),
            wrapped_account_key: b64.encode(&wrapped),
            approved_by_device_id: approver_device_id.to_string(),
            // No-auth fallback actor (`pollis_delivery::writes::resolve_actor`).
            user_id: Some(user_id.clone()),
        };
        crate::commands::mls::ds_post_ok(state, &body).await?;

        let ev = pollis_api::account::SecurityEventBody {
            kind: "device_enrolled".to_string(),
            device_id: Some(new_device_id.clone()),
            metadata: Some(metadata),
            // No-auth fallback actor (`pollis_delivery::writes::resolve_actor`).
            user_id: Some(user_id.clone()),
        };
        if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
            crate::diag!("[enrollment] DS security-event failed (non-fatal): {e}");
        }
    }

    // No MLS adds here: the new device has no key packages yet. Its
    // `enrollment_finalized` event prompts a sibling to reconcile and send Welcomes.

    crate::diag!(
        "[enrollment] approved request {request_id} for new device {new_device_id} of user {user_id}"
    );

    Ok(())
}


/// New-device side. Recover `account_id_key` from the recovery blob with the
/// user's Secret Key and adopt it; finalization runs after `set_pin`.
pub async fn recover_with_secret_key(
    state: &Arc<AppState>,
    user_id: String,
    secret_key: String,
) -> Result<()> {
    // OTP-session credential (#987); the DS logs a security event for every
    // fetch.
    let (salt, nonce, wrapped_key) =
        crate::commands::ds_reads::recovery_blob(state, &user_id)
            .await?
            .ok_or_else(|| {
                Error::Other(anyhow::anyhow!("No recovery blob found for user {user_id}"))
            })?;

    let account_id_private = crate::commands::account_identity::unwrap_recovery_blob(
        &secret_key,
        &salt,
        &nonce,
        &wrapped_key,
    )?;

    // The same chokepoint as approval; it also rejects a superseded recovery
    // blob that still unwraps under its old Secret Key.
    crate::commands::account_identity::adopt_recovered_account_identity(
        state,
        &user_id,
        &account_id_private,
    )
    .await?;

    // No security event: the audit endpoint is device-signed, and this device
    // cannot sign until `finalize_enrollment` publishes its cert.

    Ok(())
}

/// Soft recovery when the user has no Secret Key and no sibling device: after
/// an email confirmation, rotate the account identity (orphaning every other
/// device), drop all memberships, and wipe the local DB. Admins must re-add the
/// user. Returns the new Secret Key.
///
/// Destructive; the frontend must warn clearly first.
pub async fn reset_identity_and_recover(
    state: &Arc<AppState>,
    user_id: String,
    confirm_email: String,
) -> Result<String> {
    // The DS answers for the authenticated caller only (#987), so this is no
    // email oracle.
    let stored_email: String = crate::commands::ds_reads::account_status(state, &user_id)
        .await?
        .and_then(|a| a.email)
        .ok_or_else(|| Error::Other(anyhow::anyhow!("user {user_id} not found")))?;

    if !constant_time_eq(
        stored_email.trim().to_lowercase().as_bytes(),
        confirm_email.trim().to_lowercase().as_bytes(),
    ) {
        return Err(Error::Other(anyhow::anyhow!(
            "email confirmation does not match"
        )));
    }

    let new_secret_key =
        crate::commands::account_identity::reset_identity(state, &user_id).await?;

    // No MLS remove commits (the old identity is already invalid); sent
    // messages stay readable to other members.
    {
        let current_device_id = state.device_id.lock().await.clone();

        // One DS transaction: a half-cleaned account is corrupt. For a pre-enrollment
        // caller on an OTP session, `rotate-identity` already ran this wipe
        // (`pollis_delivery::account::apply_rotate_identity`), so this is a no-op.
        let body = pollis_api::account::ResetRecoverBody {
            current_device_id,
            // No-auth fallback actor (`pollis_delivery::writes::resolve_actor`).
            user_id: Some(user_id.clone()),
        };
        crate::commands::mls::ds_post_signed_or_session_ok(state, &body)
            .await?;

        // Purge pending MLS welcomes, same credential.
        {
            let body = pollis_api::writes::PurgeBody {
                // No-auth fallback actor (`pollis_delivery::writes::resolve_actor`).
                user_id: Some(user_id.clone()),
            };
            match crate::commands::mls::ds_post_signed_or_session(state, &body).await {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => {
                    let s = resp.status();
                    let txt = resp.text().await.unwrap_or_default();
                    crate::diag!("[reset] DS welcomes purge {s}: {txt}");
                }
                Err(e) => {
                    crate::diag!("[reset] DS welcomes purge failed: {e}");
                }
            }
        }

        // Local MLS state and cached messages are all invalid now.
        state.unload_user_db().await;
        {
            let data_dir = state.local_data_dir();
            let db_path = data_dir.join(format!("pollis_{user_id}.db"));
            if db_path.exists() {
                let _ = std::fs::remove_file(&db_path);
            }
            let _ = std::fs::remove_file(data_dir.join(format!("pollis_{user_id}.db-wal")));
            let _ = std::fs::remove_file(data_dir.join(format!("pollis_{user_id}.db-shm")));
        }

        // Don't reopen: `reset_identity` already populated `AppState.unlock`, and
        // `set_pin` opens the new DB next.

        crate::diag!(
            "[reset] cleaned up memberships, key packages, welcomes, devices, and local DB for {user_id}"
        );
    }

    Ok(new_secret_key)
}

/// One page of the user's security event log, newest first (#1548).
///
/// `before` is the previous page's `next_cursor` (`None` = newest); `kinds`
/// `None` or empty = every kind.
pub async fn list_security_events(
    state: &Arc<AppState>,
    user_id: String,
    limit: Option<i64>,
    before: Option<SecurityEventCursor>,
    kinds: Option<Vec<String>>,
) -> Result<SecurityEventsPage> {
    // Also clamped server-side (#987).
    let limit = limit.unwrap_or(100).clamp(1, 500);
    let kinds = kinds.filter(|k| !k.is_empty());
    let resp =
        crate::commands::ds_reads::security_events(state, &user_id, limit, before, kinds).await?;
    Ok(SecurityEventsPage {
        events: resp
            .events
            .into_iter()
            .map(|e| SecurityEvent {
                id: e.id,
                kind: e.kind,
                device_id: e.device_id,
                created_at: e.created_at,
                metadata: e.metadata,
            })
            .collect(),
        next_cursor: resp.next_cursor,
    })
}

/// Existing-device side. Reject an enrollment request without installing
/// anything. The new device's poller will see `Rejected` and surface a
/// "request rejected" message to the user.
pub async fn reject_device_enrollment(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<()> {
    let approver_device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("device_id not set — login incomplete"))?;

    // Without `want_approval_fields`: rejecting never needs the code.
    let row = crate::commands::ds_reads::enrollment_request(state, &request_id, false)
        .await?
        .ok_or_else(|| {
            Error::Other(anyhow::anyhow!("enrollment request {request_id} not found"))
        })?;
    let (_user_id, new_device_id): (String, String) = (row.user_id, row.new_device_id);

    // The DS binds the request to the signer's account.
    let body = pollis_api::account::RejectEnrollmentBody {
        request_id,
        approved_by_device_id: approver_device_id,
        // No-auth fallback actor (`pollis_delivery::writes::resolve_actor`).
        user_id: Some(_user_id.clone()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    let ev = pollis_api::account::SecurityEventBody {
        kind: "device_rejected".to_string(),
        device_id: Some(new_device_id),
        metadata: None,
        // No-auth fallback actor (`pollis_delivery::writes::resolve_actor`).
        user_id: Some(_user_id.clone()),
    };
    if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
        crate::diag!("[enrollment] DS security-event (reject) failed (non-fatal): {e}");
    }

    Ok(())
}

/// Run on the new device after `set_pin` on any enrollment path (approval,
/// Secret-Key recovery, identity reset): publishes the device cert and key
/// packages, then external-joins every group/DM without local MLS state.
/// Idempotent.
pub async fn finalize_device_enrollment(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<()> {
    finalize_enrollment(state, &user_id).await
}

async fn finalize_enrollment(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    // The device is enrolled; a claimed QR link (#1207) has done its job.
    *state.device_link_pending.lock().await = None;
    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("device_id not set during finalize"))?;

    if let Err(e) = crate::commands::mls::ensure_device_cert(state, user_id, &device_id).await {
        crate::diag!("[enrollment] finalize: ensure_device_cert failed: {e}");
    }

    if let Err(e) =
        crate::commands::mls::ensure_mls_key_package(state, user_id, &device_id).await
    {
        crate::diag!("[enrollment] finalize: ensure_mls_key_package failed: {e}");
    }

    // Scoped by the DS to the signer's memberships; signable because the cert
    // was published above.
    let mine = crate::commands::ds_reads::conversations(state, user_id).await?;
    let candidate_ids: Vec<String> = mine.group_ids.into_iter().chain(mine.dm_ids).collect();

    for conv_id in candidate_ids {
        let already_joined = {
            let guard = state.local_db.lock().await;
            guard.as_ref().is_some_and(|db| {
                crate::commands::mls::has_local_group(db.conn(), &conv_id)
            })
        };
        if already_joined {
            continue;
        }

        if let Err(e) = crate::commands::mls::external_join_group(state, &conv_id, user_id).await {
            crate::diag!(
                "[enrollment] finalize: external_join_group({conv_id}) failed: {e}"
            );
        }
    }

    Ok(())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_wrap_roundtrip() {
        let mut rng = OsRng;
        let mut requester_priv_bytes = [0u8; 32];
        rng.fill_bytes(&mut requester_priv_bytes);
        let requester_priv = StaticSecret::from(requester_priv_bytes);
        let requester_pub = PublicKey::from(&requester_priv);

        let mut approver_priv_bytes = [0u8; 32];
        rng.fill_bytes(&mut approver_priv_bytes);
        let approver_priv = StaticSecret::from(approver_priv_bytes);
        let approver_pub = PublicKey::from(&approver_priv);

        let shared_send = approver_priv.diffie_hellman(&requester_pub);
        let wrap_key = derive_wrap_key(
            shared_send.as_bytes(),
            requester_pub.as_bytes(),
            approver_pub.as_bytes(),
        );

        let mut nonce = [0u8; AES_NONCE_LEN];
        rng.fill_bytes(&mut nonce);
        let plaintext = [42u8; ACCOUNT_ID_PRIVATE_LEN];
        let ct = aead_encrypt(&wrap_key, &nonce, &plaintext).unwrap();

        let mut wrapped = Vec::with_capacity(WRAPPED_ACCOUNT_KEY_LEN);
        wrapped.extend_from_slice(approver_pub.as_bytes());
        wrapped.extend_from_slice(&nonce);
        wrapped.extend_from_slice(&ct);
        assert_eq!(wrapped.len(), WRAPPED_ACCOUNT_KEY_LEN);

        let unwrapped = unwrap_account_key(&wrapped, &requester_priv_bytes).unwrap();
        assert_eq!(unwrapped, plaintext);
    }

    #[test]
    fn enrollment_wrap_wrong_requester_key_fails() {
        let mut rng = OsRng;
        let mut legit_priv = [0u8; 32];
        rng.fill_bytes(&mut legit_priv);
        let requester_pub = PublicKey::from(&StaticSecret::from(legit_priv));

        let mut attacker_priv = [0u8; 32];
        rng.fill_bytes(&mut attacker_priv);

        let mut approver_priv = [0u8; 32];
        rng.fill_bytes(&mut approver_priv);
        let approver_secret = StaticSecret::from(approver_priv);
        let approver_pub = PublicKey::from(&approver_secret);
        let shared = approver_secret.diffie_hellman(&requester_pub);
        let wrap_key = derive_wrap_key(
            shared.as_bytes(),
            requester_pub.as_bytes(),
            approver_pub.as_bytes(),
        );

        let mut nonce = [0u8; AES_NONCE_LEN];
        rng.fill_bytes(&mut nonce);
        let ct = aead_encrypt(&wrap_key, &nonce, &[0xabu8; ACCOUNT_ID_PRIVATE_LEN]).unwrap();

        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(approver_pub.as_bytes());
        wrapped.extend_from_slice(&nonce);
        wrapped.extend_from_slice(&ct);

        assert!(unwrap_account_key(&wrapped, &attacker_priv).is_err());
    }

    /// Low-order points force a zero shared secret for every private key.
    #[test]
    fn a_blob_whose_approver_pub_is_a_low_order_point_is_refused() {
        const LOW_ORDER_POINTS: [[u8; 32]; 3] = [
            // The identity.
            [0u8; 32],
            // Order 1 in the twist / small-order representative.
            [
                1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0,
            ],
            // Order 8.
            [
                224, 235, 122, 124, 59, 65, 184, 174, 22, 86, 227, 250, 241, 159, 196, 106, 218,
                9, 141, 235, 156, 50, 177, 253, 134, 98, 5, 22, 95, 73, 184, 0,
            ],
        ];

        let mut rng = OsRng;
        let mut requester_priv = [0u8; 32];
        rng.fill_bytes(&mut requester_priv);

        for point in LOW_ORDER_POINTS {
            // The server knows the wrap key too, so it can seal ANY seed it
            // likes. Build exactly that blob.
            let victim = StaticSecret::from(requester_priv);
            let victim_pub = PublicKey::from(&victim);
            let forged_shared = victim.diffie_hellman(&PublicKey::from(point));
            let wrap_key =
                derive_wrap_key(forged_shared.as_bytes(), victim_pub.as_bytes(), &point);
            let nonce = [1u8; AES_NONCE_LEN];
            let ct = aead_encrypt(&wrap_key, &nonce, &[0xffu8; ACCOUNT_ID_PRIVATE_LEN]).unwrap();

            let mut blob = Vec::with_capacity(WRAPPED_ACCOUNT_KEY_LEN);
            blob.extend_from_slice(&point);
            blob.extend_from_slice(&nonce);
            blob.extend_from_slice(&ct);

            let err = unwrap_account_key(&blob, &requester_priv)
                .expect_err("a non-contributory exchange must be refused");
            assert!(
                format!("{err}").contains("non-contributory"),
                "refused for the wrong reason: {err}"
            );
        }
    }

    /// The check must not refuse an honest exchange.
    #[test]
    fn an_honest_exchange_is_contributory() {
        let mut rng = OsRng;
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        rng.fill_bytes(&mut a);
        rng.fill_bytes(&mut b);
        let a_priv = StaticSecret::from(a);
        let b_priv = StaticSecret::from(b);
        let shared = a_priv.diffie_hellman(&PublicKey::from(&b_priv));
        require_contributory(&shared, "test").expect("a real X25519 exchange is contributory");
    }

    #[test]
    fn constant_time_eq_matches_regular_eq() {
        assert!(constant_time_eq(b"123456", b"123456"));
        assert!(!constant_time_eq(b"123456", b"123457"));
        assert!(!constant_time_eq(b"123", b"1234"));
        assert!(constant_time_eq(b"", b""));
    }

    /// Stand-ins for the ULID, account id and RFC3339 stamp.
    const REQ: &str = "01J8ZQ5X7K0000000000000000";
    const USER: &str = "01J8ZQ0000000000000000USER";
    const WHEN: &str = "2026-09-18T10:11:12.131415+00:00";

    fn sas(pub_bytes: &[u8]) -> String {
        derive_verification_code(pub_bytes, REQ, USER, WHEN)
    }

    #[test]
    fn the_verification_code_is_determined_by_the_ephemeral_key() {
        let pub_a = [7u8; 32];
        assert_eq!(
            sas(&pub_a),
            sas(&pub_a),
            "both devices derive from the same public value, so they must agree"
        );
    }

    #[test]
    fn substituting_the_ephemeral_key_changes_the_code() {
        // Exactly the attack in #793: the key is swapped in flight.
        let honest = [7u8; 32];
        let attacker = [8u8; 32];
        assert_ne!(
            sas(&honest),
            sas(&attacker),
            "a swapped key MUST change the code the approver shows, or the human check is decorative"
        );
    }

    #[test]
    fn a_one_bit_change_in_the_key_changes_the_code() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        b[31] = 1;
        assert_ne!(sas(&a), sas(&b));
        a[0] = 1;
        assert_ne!(sas(&a), sas(&b));
    }

    #[test]
    fn the_same_ephemeral_key_derives_a_different_code_in_a_different_request() {
        let key = [7u8; X25519_PUB_LEN];
        let base = derive_verification_code(&key, REQ, USER, WHEN);

        let other_request = derive_verification_code(&key, "01J8ZQ5X7K000000000000000B", USER, WHEN);
        assert_ne!(
            base, other_request,
            "a precomputed key->code table must not carry over to another request"
        );

        let other_user = derive_verification_code(&key, REQ, "01J8ZQ0000000000000000OTHR", WHEN);
        assert_ne!(base, other_user, "the code must be bound to the account");

        let other_time =
            derive_verification_code(&key, REQ, USER, "2026-09-18T10:11:12.131416+00:00");
        assert_ne!(base, other_time, "the code must be bound to the creation time");
    }

    #[test]
    fn the_requester_and_the_approver_derive_the_same_code_for_one_request() {
        let mut rng = OsRng;
        let mut secret = [0u8; 32];
        rng.fill_bytes(&mut secret);
        let ephemeral_pub = PublicKey::from(&StaticSecret::from(secret));

        let displayed = derive_verification_code(ephemeral_pub.as_bytes(), REQ, USER, WHEN);
        let re_derived = derive_verification_code(ephemeral_pub.as_bytes(), REQ, USER, WHEN);

        assert_eq!(
            displayed, re_derived,
            "the two sides MUST agree or every legitimate enrollment is refused"
        );
        assert_eq!(normalize_enrollment_sas(&displayed), displayed);
    }

    #[test]
    fn altering_the_stored_request_context_diverges_the_approvers_code() {
        let key = [3u8; X25519_PUB_LEN];
        let displayed = derive_verification_code(&key, REQ, USER, WHEN);
        for (rid, uid, when) in [
            ("01J8ZQ5X7K000000000000000B", USER, WHEN),
            (REQ, "01J8ZQ0000000000000000EVIL", WHEN),
            (REQ, USER, "2026-09-18T10:11:13.000000+00:00"),
        ] {
            assert_ne!(
                displayed,
                derive_verification_code(&key, rid, uid, when),
                "a tampered row must not re-derive the displayed code"
            );
        }
    }

    #[test]
    fn the_salt_encoding_cannot_be_confused_across_field_boundaries() {
        assert_ne!(sas_salt("ab", "c", "d"), sas_salt("a", "bc", "d"));
        assert_ne!(sas_salt("a", "bc", "d"), sas_salt("a", "b", "cd"));
        assert_ne!(sas_salt("", "a", "b"), sas_salt("a", "", "b"));
        assert!(sas_salt("a", "b", "c").starts_with(ENROLL_SAS_SALT_DOMAIN));
    }

    #[test]
    fn the_code_is_wide_enough_that_grinding_is_not_cheap() {
        let code = sas(&[3u8; 32]);
        assert_eq!(code.chars().count(), SAS_LEN);
        assert_eq!(SAS_LEN, 8);
        assert!(
            (SAS_ALPHABET.len() as f64).log2() * SAS_LEN as f64 >= 40.0,
            "the human-compared code must carry at least 40 bits"
        );
    }

    #[test]
    fn normalizing_typed_input_is_forgiving_about_form_and_strict_about_content() {
        // Form: case, and the separators a human adds reading a code aloud.
        assert_eq!(normalize_enrollment_sas("h7k2pq3m"), "H7K2PQ3M");
        assert_eq!(normalize_enrollment_sas("H7K2 PQ3M"), "H7K2PQ3M");
        assert_eq!(normalize_enrollment_sas(" h7k2-pq3m\n"), "H7K2PQ3M");

        // Content: an excluded glyph is dropped, NOT mapped to its lookalike.
        assert_eq!(normalize_enrollment_sas("O0"), "0");
        assert_eq!(normalize_enrollment_sas("I1"), "1");
        assert_eq!(normalize_enrollment_sas("L1"), "1");
        assert_eq!(normalize_enrollment_sas("UV"), "V");
        assert_eq!(normalize_enrollment_sas("oilu"), "");

        // Bounded, and a partial code stays partial so the UI stays gated.
        assert_eq!(normalize_enrollment_sas("H7K2PQ3MEXTRA").len(), SAS_LEN);
        assert!(normalize_enrollment_sas("H7K").len() < SAS_LEN);
    }

    #[test]
    fn the_exported_input_rule_accepts_every_derived_code() {
        assert_eq!(ENROLLMENT_SAS_LEN, SAS_LEN);
        assert_eq!(
            ENROLLMENT_SAS_ALPHABET.as_bytes(),
            &SAS_ALPHABET[..],
            "the alphabet a UI validates against must be the one codes are built from"
        );
        for seed in 0u8..32 {
            let code = sas(&[seed; X25519_PUB_LEN]);
            assert_eq!(normalize_enrollment_sas(&code), code);
        }
    }

    #[test]
    fn the_code_only_uses_unambiguous_characters() {
        for seed in 0u8..64 {
            for c in sas(&[seed; 32]).chars() {
                assert!(SAS_ALPHABET.contains(&(c as u8)), "unexpected character {c}");
                assert!(!"ILOU".contains(c), "ambiguous character {c} in the code");
            }
        }
    }

    #[test]
    fn the_wrap_key_is_bound_to_both_public_keys() {
        let shared = [9u8; 32];
        let req = [1u8; 32];
        let app = [2u8; 32];
        let base = derive_wrap_key(&shared, &req, &app);
        assert_ne!(base, derive_wrap_key(&shared, &[3u8; 32], &app));
        assert_ne!(base, derive_wrap_key(&shared, &req, &[4u8; 32]));
        // Order matters: swapping the roles must not derive the same key.
        assert_ne!(base, derive_wrap_key(&shared, &app, &req));
    }

    #[test]
    fn wrap_and_unwrap_round_trip_under_transcript_binding() {
        let mut rng = OsRng;
        let mut req_priv_bytes = [0u8; 32];
        rng.fill_bytes(&mut req_priv_bytes);
        let req_priv = StaticSecret::from(req_priv_bytes);
        let req_pub = PublicKey::from(&req_priv);

        let mut app_priv_bytes = [0u8; 32];
        rng.fill_bytes(&mut app_priv_bytes);
        let app_priv = StaticSecret::from(app_priv_bytes);
        let app_pub = PublicKey::from(&app_priv);

        let secret = [42u8; ACCOUNT_ID_PRIVATE_LEN];
        let shared = app_priv.diffie_hellman(&req_pub);
        let wrap_key = derive_wrap_key(shared.as_bytes(), req_pub.as_bytes(), app_pub.as_bytes());
        let nonce = [5u8; AES_NONCE_LEN];
        let ct = aead_encrypt(&wrap_key, &nonce, &secret).expect("encrypt");

        let mut blob = Vec::new();
        blob.extend_from_slice(app_pub.as_bytes());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);

        let out = unwrap_account_key(&blob, &req_priv_bytes).expect("unwrap");
        assert_eq!(out, secret.to_vec(), "the legitimate pair must still round-trip");
    }

    #[test]
    fn a_blob_wrapped_to_a_different_requester_does_not_unwrap() {
        let mut rng = OsRng;
        let mut victim_bytes = [0u8; 32];
        rng.fill_bytes(&mut victim_bytes);
        let victim_priv = StaticSecret::from(victim_bytes);
        let victim_pub = PublicKey::from(&victim_priv);

        let mut attacker_bytes = [0u8; 32];
        rng.fill_bytes(&mut attacker_bytes);
        let attacker_priv = StaticSecret::from(attacker_bytes);
        let attacker_pub = PublicKey::from(&attacker_priv);

        let mut app_bytes = [0u8; 32];
        rng.fill_bytes(&mut app_bytes);
        let app_priv = StaticSecret::from(app_bytes);
        let app_pub = PublicKey::from(&app_priv);

        // The approver wrapped to the ATTACKER's key (the #793 scenario).
        let shared = app_priv.diffie_hellman(&attacker_pub);
        let wrap_key =
            derive_wrap_key(shared.as_bytes(), attacker_pub.as_bytes(), app_pub.as_bytes());
        let nonce = [6u8; AES_NONCE_LEN];
        let ct = aead_encrypt(&wrap_key, &nonce, &[1u8; ACCOUNT_ID_PRIVATE_LEN]).expect("encrypt");
        let mut blob = Vec::new();
        blob.extend_from_slice(app_pub.as_bytes());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);

        assert!(unwrap_account_key(&blob, &victim_bytes).is_err());
        let _ = victim_pub;
    }
}
