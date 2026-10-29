//! Device enrollment: transferring `account_id_key` from an enrolled device to a
//! new one over an ephemeral X25519 channel.
//!
//! 1. The new device (`start_device_enrollment`) generates an ephemeral keypair,
//!    derives an 8-character SAS from its public half (salted per request), and
//!    files a request via the session-gated DS endpoint; the DS sends the inbox
//!    nudge. The SAS is shown on the new device.
//! 2. An enrolled sibling sees the request and asks the human to type the SAS
//!    (#1096); `approve_device_enrollment` re-derives it from the key it fetched,
//!    refuses a non-contributory ECDH, wraps the seed under ECDH + HKDF, and marks
//!    the request approved.
//! 3. The new device unwraps, checks the seed against the account's published
//!    identity key, and later runs `finalize_enrollment`.
//!
//! Requests expire after 10 minutes. Every tier can request and approve. This is
//! device linking, not a login second factor: it runs once per device because
//! it is the only way the account key reaches it. See
//! `.codesight/wiki/commands.md` and `pin-design.md`.

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

/// How long a pending request is valid.
const ENROLLMENT_TTL_SECS: i64 = 10 * 60;

/// HKDF info binding the wrap key to this protocol.
const ENROLL_HKDF_INFO: &[u8] = b"pollis-enrollment-wrap-v1";

/// HKDF info for the human-compared code (#793), separate from the wrap key's.
const ENROLL_SAS_INFO: &[u8] = b"pollis-enrollment-sas-v1";

/// Domain tag opening the per-request SAS salt; both ends must agree byte for
/// byte.
const ENROLL_SAS_SALT_DOMAIN: &[u8] = b"pollis-enrollment-sas-salt-v1";

/// The Secret Key's alphabet (`0-9A-Z` minus I, L, O, U): 5 bits per character.
const SAS_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Code length. Eight characters (40 bits), not six digits: a hostile server can
/// grind ephemeral keypairs until one derives the victim's code, so the bound
/// code is only as strong as it is wide.
const SAS_LEN: usize = 8;

/// Code length for the UIs that gate on it.
pub const ENROLLMENT_SAS_LEN: usize = SAS_LEN;

/// The code alphabet as a `&str`, for UIs that filter typed characters.
pub const ENROLLMENT_SAS_ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Normalize typed input to the canonical code form: forgiving about form (case,
/// spaces, dashes), strict about content. A character outside the alphabet is
/// dropped, never mapped to a lookalike, which would let a mis-read code compare
/// equal. Truncates at [`ENROLLMENT_SAS_LEN`].
///
/// Source of truth for the twins in `frontend/src/utils/enrollmentSas.ts` and
/// `mobile/lib/enrollmentSas.ts`, whose tests quote the two constants above.
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
// approver_pub (32) || nonce (12) || ciphertext+tag (32 + 16) = 92.
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
    Pending,
    /// Approved: `account_id_key` is installed locally.
    Approved,
    Rejected,
    Expired,
}

/// One open enrollment request, as the approving device sees it. Carries no
/// verification code (#1096): with the DS's copy here the UI pre-filled it and
/// nobody compared anything. The approver types the code from the new device's
/// screen instead.
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

/// The human-compared code, derived from the new device's ephemeral public key
/// (#793), so a swapped key changes the code the approver derives. Not a secret:
/// its job is binding.
///
/// Salted with [`sas_salt`] (request id, account, creation time), so a
/// precomputed key-to-code table is valid for one request only and the 2^40 grind
/// must be redone inside each TTL. Both sides hold all three fields.
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

/// Per-request salt for [`derive_verification_code`]. Each field is u32-BE
/// length-prefixed so the encoding is injective: no two triples share a salt.
fn sas_salt(request_id: &str, user_id: &str, created_at: &str) -> Vec<u8> {
    let mut salt = Vec::new();
    salt.extend_from_slice(ENROLL_SAS_SALT_DOMAIN);
    for field in [request_id, user_id, created_at] {
        salt.extend_from_slice(&(field.len() as u32).to_be_bytes());
        salt.extend_from_slice(field.as_bytes());
    }
    salt
}

/// Wrap key for the transfer, bound to both ephemeral public keys.
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

/// Reject an X25519 exchange the peer didn't contribute to.
///
/// `x25519-dalek` 2.x returns the all-zero secret for a low-order peer key
/// instead of failing, which would make the wrap key a function of public values:
/// the DS could compute it and forge an approval carrying an attacker-chosen
/// seed. Checked on both sides, since both peer keys pass through the DS.
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

/// New-device side: file a pending request and return the handle the frontend
/// waits on.
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

    // A fresh ephemeral key, unless this device was signed in by showing a QR
    // (#1361): then the key the QR carried, the only one the scanner will wrap to.
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
    // The exact spelling the approver re-derives from.
    let created_at_str = now.to_rfc3339();

    // Derived from the ephemeral key (#793) and salted per request; see
    // `derive_verification_code`.
    let verification_code: String = derive_verification_code(
        ephemeral_public.as_bytes(),
        &request_id,
        &user_id,
        &created_at_str,
    );

    // The server gets only the public key and the code. Session-gated (the
    // `enrollment_session` from `verify_otp`): this pre-enrollment device can't
    // device-sign. The DS binds `user_id` and `new_device_id` from the session.
    let enrollment_session = state.enrollment_session.lock().await.clone();
    match enrollment_session {
        Some(token) => {
            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD;
            // A QR-claimed sign-in (#1207) tags the request with the QR-keyed MAC; a v2
            // offer (#1361) and an email-OTP sign-in carry none.
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
            // The session is gone (e.g. restart mid-enrollment) and this device can't
            // device-sign: fail recoverably so the user signs in again.
            return Err(Error::Other(anyhow::anyhow!(
                "Your sign-in session expired. Please sign in again to enroll this device."
            )));
        }
    }

    // Kept in memory for `poll_enrollment_status`.
    state
        .enrollment_ephemeral_keys
        .lock()
        .await
        .insert(request_id.clone(), private_bytes.to_vec());

    // The DS sends the inbox nudge to sibling devices; this device has no signing
    // credential. An offline sibling catches it via
    // `list_pending_enrollment_requests` at login.
    crate::diag!(
        "[enrollment] requested enrollment for user {user_id} device {device_id} (request {request_id})"
    );

    Ok(EnrollmentHandle {
        request_id,
        verification_code,
        expires_at: expires_at_str,
    })
}

/// New-device side: the request's status. On `Approved`, unwrap and install the
/// account key.
pub async fn poll_enrollment_status(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    // Gated on possession of `request_id` (#987 §6.5): this device has no
    // credential; see `ds_reads::enrollment_request`.
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

            // Into `AppState.unlock` with a fresh `db_key`; `set_pin` and
            // `finalize_device_enrollment` follow. Through
            // `adopt_recovered_account_identity`, which refuses a seed that isn't the
            // account's published key: a successful AEAD open says nothing about which seed
            // is inside.
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

const AWAIT_BACKOFF_START_MS: u64 = 1_000;
/// Backoff ceiling: approval is a human on another device, so seconds of latency
/// are invisible, and a ten-minute wait costs a handful of reads.
const AWAIT_BACKOFF_MAX_MS: u64 = 8_000;

/// Wait for an enrollment request to reach a terminal state. One awaited call
/// (#874), not a renderer poll: the pre-enrollment device can't be pushed to, so
/// the backend waits with exponential backoff, stopping at the request's TTL.
pub async fn await_enrollment_approval(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    // Slightly past the TTL, so expiry is reported by the usual status check.
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
    // Same transcript the approver bound: our public key, then theirs.
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

/// Existing-device side: list open requests (the login-time fallback for a
/// missed inbox push).
pub async fn list_pending_enrollment_requests(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<Vec<PendingEnrollmentRequest>> {
    // The DS filters to pending, unexpired requests for the signer's own account.
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

/// Existing-device side: check the typed code, wrap the account key to the
/// requester's ephemeral key and mark the request approved.
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

    // Device-signed with `want_approval_fields` (#987): the DS serves the ephemeral
    // key and code only to a signed caller owning the account.
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

    // #793: check the typed code against one derived from the key we fetched, never
    // against `stored_code`, which is server-controlled. The salt fields come off
    // the served row, so altering any of them also diverges the code.
    let expected_code = derive_verification_code(
        &ephemeral_pub,
        &request_id,
        &user_id,
        &created_at_str,
    );

    if !constant_time_eq(expected_code.as_bytes(), verification_code.as_bytes()) {
        return Err(Error::Other(anyhow::anyhow!(
            "verification code does not match"
        )));
    }

    // A stored code that disagrees with the derived one means the key was altered
    // after publication. The check above already protected the user; this reports
    // the tampering.
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
/// Callers must have authenticated `ephemeral_pub` (by typed SAS, or by
/// `device_link::approve_device_link`'s link tag, #1207). `via` lands in the
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
    // An ML-DSA-44 private key is its 32-byte seed.
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

    // The new device generates and publishes its own MLS key and cert in
    // `finalize_enrollment`.

    // The DS binds the request to the signer, so a device can only approve its own
    // account's enrollments.
    let metadata = format!("via={via},approver={approver_device_id}");
    {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let body = pollis_api::account::ApproveEnrollmentBody {
            request_id: request_id.clone(),
            wrapped_account_key: b64.encode(&wrapped),
            approved_by_device_id: approver_device_id.to_string(),
            // The DS's no-auth fallback actor (`resolve_actor`); when signed it must equal
            // the signed user.
            user_id: Some(user_id.clone()),
        };
        crate::commands::mls::ds_post_ok(state, &body).await?;

        let ev = pollis_api::account::SecurityEventBody {
            kind: "device_enrolled".to_string(),
            device_id: Some(new_device_id.clone()),
            metadata: Some(metadata),
            // Same no-auth fallback actor.
            user_id: Some(user_id.clone()),
        };
        if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
            crate::diag!("[enrollment] DS security-event failed (non-fatal): {e}");
        }
    }

    // MLS addition is deferred: the new device publishes key packages in
    // `finalize_enrollment`, and a sibling reconciles on `enrollment_finalized`.

    crate::diag!(
        "[enrollment] approved request {request_id} for new device {new_device_id} of user {user_id}"
    );

    Ok(())
}


/// New-device side: recover `account_id_key` from the server's recovery blob with
/// the user's Secret Key and install it locally; `finalize_enrollment` follows.
pub async fn recover_with_secret_key(
    state: &Arc<AppState>,
    user_id: String,
    secret_key: String,
) -> Result<()> {
    // OTP-session credential (#987); the DS logs a security event per fetch.
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

    // Through the same chokepoint as approval: a superseded `account_recovery` row
    // still unwraps under its Secret Key, and only the published-key check tells a
    // current blob from a replayed one.
    crate::commands::account_identity::adopt_recovered_account_identity(
        state,
        &user_id,
        &account_id_private,
    )
    .await?;

    // No audit row here: the device can't device-sign until `finalize_enrollment`
    // publishes its cert, and `/v1/security-events` requires that.

    Ok(())
}

/// Soft recovery for a user with no Secret Key and no sibling device, confirmed
/// by typing their email: rotate to a new account identity (orphaning other
/// devices), drop all memberships and the local DB, and return the new Secret
/// Key. Destructive; the frontend must warn clearly first.
pub async fn reset_identity_and_recover(
    state: &Arc<AppState>,
    user_id: String,
    confirm_email: String,
) -> Result<String> {
    // The DS answers `account-status` for the authenticated caller only (#987), so
    // this is no "is X's email Y?" oracle.
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

    // Leave all groups and DMs. No MLS remove commits (the old identity is already
    // invalid) and sent messages are kept.
    {
        let current_device_id = state.device_id.lock().await.clone();

        // One DS transaction for ownership handoff, membership and key-package deletes
        // and orphaning other devices: a half-cleaned account is corrupt. For an
        // OTP-session caller the DS already ran this inside `rotate-identity`, so this
        // is an idempotent no-op there.
        let body = pollis_api::account::ResetRecoverBody {
            current_device_id,
            // Same no-auth fallback actor.
            user_id: Some(user_id.clone()),
        };
        crate::commands::mls::ds_post_signed_or_session_ok(state, &body)
            .await?;

        // Purge pending MLS welcomes, with the same signed-or-session credential.
        {
            let body = pollis_api::writes::PurgeBody {
                // Same no-auth fallback actor.
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

        // The local DB's MLS state and cached messages are all invalid now.
        state.unload_user_db().await;
        {
            let data_dir = crate::db::local::dirs_path();
            let db_path = data_dir.join(format!("pollis_{user_id}.db"));
            if db_path.exists() {
                let _ = std::fs::remove_file(&db_path);
            }
            let _ = std::fs::remove_file(data_dir.join(format!("pollis_{user_id}.db-wal")));
            let _ = std::fs::remove_file(data_dir.join(format!("pollis_{user_id}.db-shm")));
        }

        // `reset_identity` already populated `AppState.unlock`; `set_pin` opens the new
        // DB.

        crate::diag!(
            "[reset] cleaned up memberships, key packages, welcomes, devices, and local DB for {user_id}"
        );
    }

    Ok(new_secret_key)
}

/// The user's security event log, most recent first.
pub async fn list_security_events(
    state: &Arc<AppState>,
    user_id: String,
    limit: Option<i64>,
) -> Result<Vec<SecurityEvent>> {
    // Clamped here and again by the DS (#987).
    let limit = limit.unwrap_or(100).clamp(1, 500);
    let rows = crate::commands::ds_reads::security_events(state, &user_id, limit).await?;
    Ok(rows
        .into_iter()
        .map(|e| SecurityEvent {
            id: e.id,
            kind: e.kind,
            device_id: e.device_id,
            created_at: e.created_at,
            metadata: e.metadata,
        })
        .collect())
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

    // Device-signed without `want_approval_fields`: rejecting never needs the code.
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
        // Same no-auth fallback actor.
        user_id: Some(_user_id.clone()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    let ev = pollis_api::account::SecurityEventBody {
        kind: "device_rejected".to_string(),
        device_id: Some(new_device_id),
        metadata: None,
        // Same no-auth fallback actor.
        user_id: Some(_user_id.clone()),
    };
    if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
        crate::diag!("[enrollment] DS security-event (reject) failed (non-fatal): {e}");
    }

    Ok(())
}

/// After the account key is installed on the new device (approval, Secret Key
/// recovery or identity reset), and after `set_pin`: publish the device cert and
/// key packages, then externally join every group/DM with no local MLS state
/// (essential on the Secret Key path, where no Welcomes exist). Idempotent.
pub async fn finalize_device_enrollment(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<()> {
    finalize_enrollment(state, &user_id).await
}

async fn finalize_enrollment(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    // A claimed QR link (#1207) is done.
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

    // External-join every group/DM this user is in via the stored GroupInfo. The
    // device has published its cert above, so this read can be device-signed.
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

    /// The 32 zero bytes are the canonical low-order point; `x25519-dalek` 2.x
    /// returns the zero secret for it rather than failing.
    #[test]
    fn a_blob_whose_approver_pub_is_a_low_order_point_is_refused() {
        const LOW_ORDER_POINTS: [[u8; 32]; 3] = [
            [0u8; 32],
            [
                1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0,
            ],
            [
                224, 235, 122, 124, 59, 65, 184, 174, 22, 86, 227, 250, 241, 159, 196, 106, 218,
                9, 141, 235, 156, 50, 177, 253, 134, 98, 5, 22, 95, 73, 184, 0,
            ],
        ];

        let mut rng = OsRng;
        let mut requester_priv = [0u8; 32];
        rng.fill_bytes(&mut requester_priv);

        for point in LOW_ORDER_POINTS {
            // The server knows the wrap key too, so it can seal any seed.
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

    /// The check must not fire on an honest exchange.
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

    // #793: the code must bind the ephemeral key.

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

    // The SAS must be per-request, not a global function of the key.

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

    /// The requester's and approver's codes must agree for one request context.
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

    /// A DS altering any context string on the row diverges the approver's code.
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

    /// The salt encoding is injective across field boundaries.
    #[test]
    fn the_salt_encoding_cannot_be_confused_across_field_boundaries() {
        assert_ne!(sas_salt("ab", "c", "d"), sas_salt("a", "bc", "d"));
        assert_ne!(sas_salt("a", "bc", "d"), sas_salt("a", "b", "cd"));
        assert_ne!(sas_salt("", "a", "b"), sas_salt("a", "", "b"));
        assert!(sas_salt("a", "b", "c").starts_with(ENROLL_SAS_SALT_DOMAIN));
    }

    #[test]
    fn the_code_is_wide_enough_that_grinding_is_not_cheap() {
        // At least 40 bits; six digits would be 2^20, seconds of grinding.
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
        assert_eq!(normalize_enrollment_sas("h7k2pq3m"), "H7K2PQ3M");
        assert_eq!(normalize_enrollment_sas("H7K2 PQ3M"), "H7K2PQ3M");
        assert_eq!(normalize_enrollment_sas(" h7k2-pq3m\n"), "H7K2PQ3M");

        assert_eq!(normalize_enrollment_sas("O0"), "0");
        assert_eq!(normalize_enrollment_sas("I1"), "1");
        assert_eq!(normalize_enrollment_sas("L1"), "1");
        assert_eq!(normalize_enrollment_sas("UV"), "V");
        assert_eq!(normalize_enrollment_sas("oilu"), "");

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
        // I, L, O and U are excluded so reading the code aloud can't manufacture a
        // false match or mismatch.
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

        // The approver wrapped to the attacker's key (#793).
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
