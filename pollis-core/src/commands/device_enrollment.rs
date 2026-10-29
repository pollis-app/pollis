//! Device enrollment — approval path.
//!
//! Moves `account_id_key` from an enrolled device to a new one over an ephemeral X25519 channel.
//!
//! Every tier both requests and approves.
//!
//! Device linking, not a login second factor: it runs once per device because no server holds the key.

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

/// 10 minutes: long enough to walk to the other device, short enough to bound exposure.
const ENROLLMENT_TTL_SECS: i64 = 10 * 60;

/// Domain-separates the wrap key from every other KDF use.
const ENROLL_HKDF_INFO: &[u8] = b"pollis-enrollment-wrap-v1";

/// HKDF info for the SAS (#793), domain-separated from the wrap key.
const ENROLL_SAS_INFO: &[u8] = b"pollis-enrollment-sas-v1";

/// Versioned separately from [`ENROLL_SAS_INFO`]: both ends must agree on the salt's shape.
const ENROLL_SAS_SALT_DOMAIN: &[u8] = b"pollis-enrollment-sas-salt-v1";

/// Crockford base32 (no I, L, O, U): 5 bits per character.
const SAS_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Eight chars (40 bits), not six digits (2^20): a hostile server can grind ephemeral keys
/// until one derives the victim's code, so the width is what prices that attack.
const SAS_LEN: usize = 8;

/// Re-exported so the UIs' input gating tracks [`SAS_LEN`].
pub const ENROLLMENT_SAS_LEN: usize = SAS_LEN;

/// [`SAS_ALPHABET`] as a `&str`, for UIs.
pub const ENROLLMENT_SAS_ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Normalize what a human typed into the canonical verification-code form.
///
/// Forgiving about form (case, spaces, dashes), not content: an outside character is DROPPED,
/// never mapped to a lookalike, so a mis-read code cannot compare equal (#1096).
///
/// Twins: `frontend/src/utils/enrollmentSas.ts`, `mobile/lib/enrollmentSas.ts`; their tests quote the constants.
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
// approver_pub (32) || nonce (12) || ciphertext+tag (32 + 16) = 92
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
    /// The new device installed `account_id_key` and is enrolled.
    Approved,
    /// An existing device rejected the request.
    Rejected,
    /// TTL elapsed.
    Expired,
}

/// One open enrollment request, as the APPROVING device sees it.
///
/// Carries **no verification code** (#1096): the approver must type the one the new device
/// shows, so the UI has nothing to pre-fill with.
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

/// Keyset cursor for the security-event log (#1548); opaque to the UI.
pub use pollis_api::account_reads::SecurityEventCursor;

/// One page of the security-event log, newest first (#1548).
#[derive(Debug, Serialize, Deserialize)]
pub struct SecurityEventsPage {
    pub events: Vec<SecurityEvent>,
    /// Pass back as `before` for the next page; `None` = this page reached the
    /// oldest matching row.
    pub next_cursor: Option<SecurityEventCursor>,
}

/// The verification code the human compares, derived from the new device's
/// ephemeral public key (#793).
///
/// Binding, not secret: substituting the key changes the code the approver displays.
///
/// Salted per request ([`sas_salt`]) so a key→code table (a one-time 2^40 precomputation
/// if unsalted) must be rebuilt for every request inside its TTL.
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

/// The per-request salt for [`derive_verification_code`].
///
/// Length-prefixed so the encoding is injective: no two triples share a salt.
fn sas_salt(request_id: &str, user_id: &str, created_at: &str) -> Vec<u8> {
    let mut salt = Vec::new();
    salt.extend_from_slice(ENROLL_SAS_SALT_DOMAIN);
    for field in [request_id, user_id, created_at] {
        salt.extend_from_slice(&(field.len() as u32).to_be_bytes());
        salt.extend_from_slice(field.as_bytes());
    }
    salt
}

/// Wrap key for the account-identity transfer, bound to the full transcript.
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

/// Reject an X25519 exchange whose output the peer did not contribute to.
///
/// `x25519-dalek` 2.x returns an all-zero secret for low-order points instead of failing, which
/// would let the DS compute the wrap key and forge an approval with a seed of its choosing.
///
/// Checked on both sides: either peer key is server-influenced.
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

/// New-device side: file a pending enrollment request and return its handle.
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

    // A device signed in by SHOWING a QR (#1361) must enroll with the key that QR carried.
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
    // Derived once, sent verbatim, and re-derived by the approver from the same
    // three strings — so this is the exact spelling that has to travel.
    let created_at_str = now.to_rfc3339();

    // DERIVED from the ephemeral key and salted per request (#793); see `derive_verification_code`.
    let verification_code: String = derive_verification_code(
        ephemeral_public.as_bytes(),
        &request_id,
        &user_id,
        &created_at_str,
    );

    // Session-gated: the new device has no cert yet, so it cannot device-sign. The DS binds
    // `user_id` and `new_device_id` from the session.
    let enrollment_session = state.enrollment_session.lock().await.clone();
    match enrollment_session {
        Some(token) => {
            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD;
            // v1 links (#1207) tag the request with the QR-keyed MAC; v2 offers (#1361) carry no tag.
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
            // The in-memory session is gone (e.g. app restart); only a new OTP re-mints it.
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

    // The DS sends the inbox nudge: this pre-enrollment device cannot device-sign.
    crate::diag!(
        "[enrollment] requested enrollment for user {user_id} device {device_id} (request {request_id})"
    );

    Ok(EnrollmentHandle {
        request_id,
        verification_code,
        expires_at: expires_at_str,
    })
}

/// New-device side: poll the request; on `Approved`, unwrap and install the key.
pub async fn poll_enrollment_status(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    // Gated on possession of `request_id` (#987): this device has no signing key and its
    // OTP session (600s, minted earlier) has already expired.
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

    // TTL check — if expired, short-circuit without touching the status column.
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

            // Next: pin-create (`set_pin` wraps the key), then `finalize_device_enrollment`.
            // An AEAD open doesn't prove WHICH seed is inside, so adopt checks it is the published one.
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
/// A human approving takes seconds; this keeps a full-TTL wait to a handful of reads.
const AWAIT_BACKOFF_MAX_MS: u64 = 8_000;

/// Wait for an enrollment request to reach a terminal state, then return it.
///
/// ONE call, not a renderer poll (#874): the backend backs off and stops at the request's TTL.
pub async fn await_enrollment_approval(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    // A little past the TTL so the expiry is reported by the same status check
    // every other path uses, rather than being inferred from a timeout here.
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
    // The DS filters, orders and scopes the list to the signer's own account.
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

/// Existing-device side: check the typed SAS, then wrap `account_id_key` to the requester.
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

    // Device-signed: the DS serves the ephemeral key and code only to the account's own devices (#987).
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

    // Check the typed code against one derived from the key WE fetched, never `stored_code` (#793).
    // The salt fields come off the served row, so altering any of them shows as a mismatch.
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

    // A stored copy that disagrees means the key was altered after publishing; report it.
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
/// Callers MUST have authenticated `ephemeral_pub`; `via` lands in the security event.
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

    // The DS binds the request to the signer, so a device approves only its OWN account.
    let metadata = format!("via={via},approver={approver_device_id}");
    {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let body = pollis_api::account::ApproveEnrollmentBody {
            request_id: request_id.clone(),
            wrapped_account_key: b64.encode(&wrapped),
            approved_by_device_id: approver_device_id.to_string(),
            // Must equal the signed user (auth on); is the actor when auth is off.
            user_id: Some(user_id.clone()),
        };
        crate::commands::mls::ds_post_ok(state, &body).await?;

        // Security event, best-effort.
        let ev = pollis_api::account::SecurityEventBody {
            kind: "device_enrolled".to_string(),
            device_id: Some(new_device_id.clone()),
            metadata: Some(metadata),
            // Must equal the signed user (auth on); is the actor when auth is off.
            user_id: Some(user_id.clone()),
        };
        if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
            crate::diag!("[enrollment] DS security-event failed (non-fatal): {e}");
        }
    }

    // MLS adds wait for the new device's KPs: its `enrollment_finalized` event triggers reconcile.

    crate::diag!(
        "[enrollment] approved request {request_id} for new device {new_device_id} of user {user_id}"
    );

    Ok(())
}


/// New-device side: recover `account_id_key` with the Secret Key, then finalize as for approval.
pub async fn recover_with_secret_key(
    state: &Arc<AppState>,
    user_id: String,
    secret_key: String,
) -> Result<()> {
    // OTP-session credential; the DS logs every fetch as a security event (#987).
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

    // Same chokepoint as approval: a SUPERSEDED recovery row still unwraps under its old Secret Key.
    crate::commands::account_identity::adopt_recovered_account_identity(
        state,
        &user_id,
        &account_id_private,
    )
    .await?;

    // No audit row: pre-finalize, this device cannot device-sign `/v1/security-events`.

    Ok(())
}

/// Soft recovery path. The user has no Secret Key, no sibling device,
/// and no way to approve. They confirm by typing their email, and we:
/// verify the email, rotate the identity, drop all memberships and the local DB, and return
/// the new Secret Key. Admins must re-add the user.
///
/// Destructive; the frontend must warn clearly first.
pub async fn reset_identity_and_recover(
    state: &Arc<AppState>,
    user_id: String,
    confirm_email: String,
) -> Result<String> {
    // The DS answers only for the AUTHENTICATED caller (#987), so this is no email oracle.
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

    // 2. Rotate the account identity and install the new key locally.
    let new_secret_key =
        crate::commands::account_identity::reset_identity(state, &user_id).await?;

    // No MLS remove commits (the identity is already invalid); sent messages stay readable.
    {
        let current_device_id = state.device_id.lock().await.clone();

        // One DS transaction (#419): a half-cleaned account is corrupt. For a session-authenticated
        // caller the `rotate-identity` above already ran this wipe, so this is an idempotent no-op.
        let body = pollis_api::account::ResetRecoverBody {
            current_device_id,
            // Must equal the signed user (auth on); is the actor when auth is off.
            user_id: Some(user_id.clone()),
        };
        crate::commands::mls::ds_post_signed_or_session_ok(state, &body)
            .await?;

        // Purge pending welcomes, same credential.
        {
            let body = pollis_api::writes::PurgeBody {
                // Must equal the signed user (auth on); is the actor when auth is off.
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

        // Wipe local DB (MLS group state, cached messages — all invalid now)
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

        // No reopen: `reset_identity` set up `AppState.unlock`; `set_pin` opens the new DB.

        crate::diag!(
            "[reset] cleaned up memberships, key packages, welcomes, devices, and local DB for {user_id}"
        );
    }

    Ok(new_secret_key)
}

/// One page of the user's security event log, most recent first (#1548).
///
/// `before` is the previous page's `next_cursor`; `None` starts at the newest
/// row. `kinds` `None` or empty = every kind.
pub async fn list_security_events(
    state: &Arc<AppState>,
    user_id: String,
    limit: Option<i64>,
    before: Option<SecurityEventCursor>,
    kinds: Option<Vec<String>>,
) -> Result<SecurityEventsPage> {
    // The clamp stays here AND is re-applied server-side (#987): a body cannot
    // ask the DS for the whole table regardless of what this client sends.
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

/// Existing-device side: reject an enrollment request.
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

    // The DS binds the request to the signer.
    let body = pollis_api::account::RejectEnrollmentBody {
        request_id,
        approved_by_device_id: approver_device_id,
        // Must equal the signed user (auth on); is the actor when auth is off.
        user_id: Some(_user_id.clone()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    let ev = pollis_api::account::SecurityEventBody {
        kind: "device_rejected".to_string(),
        device_id: Some(new_device_id),
        metadata: None,
        // Must equal the signed user (auth on); is the actor when auth is off.
        user_id: Some(_user_id.clone()),
    };
    if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
        crate::diag!("[enrollment] DS security-event (reject) failed (non-fatal): {e}");
    }

    Ok(())
}

/// New device, after `set_pin`: publish the cert and KPs, then external-join every
/// conversation without local MLS state. Idempotent.
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

    // Device-signed read (#987): works only because the cert was published above.
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
        // New device generates ephemeral keypair
        let mut rng = OsRng;
        let mut requester_priv_bytes = [0u8; 32];
        rng.fill_bytes(&mut requester_priv_bytes);
        let requester_priv = StaticSecret::from(requester_priv_bytes);
        let requester_pub = PublicKey::from(&requester_priv);

        // Approver wraps account_id_key
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

        // New device unwraps
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

        // Attacker can't decrypt.
        assert!(unwrap_account_key(&wrapped, &attacker_priv).is_err());
    }

    /// Low-order points must be refused; `x25519-dalek` 2.x does not do it for us.
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
            // The server knows the wrap key, so it can seal any seed.
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

    /// The check must not reject an honest exchange.
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

    /// Stand-ins for the request ULID, account id and RFC3339 stamp.
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

    /// The same key derives different codes in different requests.
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

    /// Requester and approver agree for the same request context.
    #[test]
    fn the_requester_and_the_approver_derive_the_same_code_for_one_request() {
        let mut rng = OsRng;
        let mut secret = [0u8; 32];
        rng.fill_bytes(&mut secret);
        let ephemeral_pub = PublicKey::from(&StaticSecret::from(secret));

        // Requester: it minted request_id / created_at and knows its own user.
        let displayed = derive_verification_code(ephemeral_pub.as_bytes(), REQ, USER, WHEN);
        // Approver: the same strings off the row it fetched.
        let re_derived = derive_verification_code(ephemeral_pub.as_bytes(), REQ, USER, WHEN);

        assert_eq!(
            displayed, re_derived,
            "the two sides MUST agree or every legitimate enrollment is refused"
        );
        assert_eq!(normalize_enrollment_sas(&displayed), displayed);
    }

    /// A DS altering any context string makes the approver's code diverge visibly.
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
        // And the domain tag opens every one of them.
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

    /// #1096: the input rule is part of the security boundary.
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
        // A derived code survives normalization unchanged, for many keys.
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

        // The real new device cannot open it.
        assert!(unwrap_account_key(&blob, &victim_bytes).is_err());
        let _ = victim_pub;
    }
}
