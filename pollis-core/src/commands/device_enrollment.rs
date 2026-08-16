//! Device enrollment — approval path.
//!
//! When a user signs in on a new device, this module manages the flow for
//! transferring their `account_id_key` from an existing device over an
//! ephemeral, authenticated channel. Flow:
//!
//!   1. New device calls `start_device_enrollment` → generates an
//!      ephemeral X25519 keypair and a 6-digit verification code, and
//!      writes a `device_enrollment_request` row via the session-gated DS
//!      endpoint (the new device is pre-enrollment, so it can't device-sign).
//!      The DS then emits the `enrollment_requested` inbox nudge SERVER-SIDE
//!      — the new device can't, since a client-side send-data needs a signing
//!      credential it doesn't have yet. The private X25519 key stays in
//!      memory on the new device.
//!
//!   2. Any of the user's already-enrolled devices receives the inbox
//!      event (or, if it was offline at request time, picks the request up
//!      via its login-time `list_pending_enrollment_requests` poll) and shows
//!      an immediate-takeover approval UI that displays the verification code.
//!
//!   3. User confirms the code matches and taps approve. The approving
//!      device calls `approve_device_enrollment`:
//!        a. Verifies the verification code against the request row.
//!        b. Generates its own ephemeral X25519 keypair.
//!        c. ECDH(approver_priv, requester_pub) → HKDF → wrap key.
//!        d. AES-256-GCM wraps `account_id_key.private` and writes
//!           `approver_pub || nonce || ciphertext` into the request row.
//!        e. Signs a `device_cert` for the new device.
//!        f. Adds the new device to every group/DM the user is in.
//!        g. Marks the request `status = 'approved'`.
//!
//!   4. New device polls `poll_enrollment_status`. When it sees
//!      `approved`, it unwraps the blob with its stored ephemeral private
//!      key, stores `account_id_key` in its OS keystore, and calls
//!      `finalize_enrollment` which publishes the new device's cert and
//!      processes any welcomes.
//!
//! Rejection and expiry: `reject_device_enrollment` flips status and
//! records a security event. Requests that time out (10-minute TTL) are
//! treated as `expired` by the poller without special cleanup.

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

// ── Constants ────────────────────────────────────────────────────────────────

/// How long a pending enrollment request is valid. 10 minutes is the
/// budget: long enough to walk to the other device, short enough to bound
/// the attack surface if a code is observed.
const ENROLLMENT_TTL_SECS: i64 = 10 * 60;

/// HKDF info string — binds the derived wrap key to this specific protocol
/// so an attacker with access to the ECDH output cannot reuse it for any
/// other KDF-based scheme in the system.
const ENROLL_HKDF_INFO: &[u8] = b"pollis-enrollment-wrap-v1";

/// HKDF info for the human-compared verification code (#793). Domain-separated
/// from the wrap-key derivation so the two can never collide.
const ENROLL_SAS_INFO: &[u8] = b"pollis-enrollment-sas-v1";

/// Alphabet for the verification code — the same 32 characters the Secret Key
/// uses (`0-9A-Z` minus I, L, O, U, which are easy to misread). Exactly 5 bits
/// per character.
const SAS_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Length of the verification code, in characters.
///
/// EIGHT, not six digits, and the width is load-bearing rather than cosmetic.
/// The code is now a deterministic function of the new device's ephemeral public
/// key, so a hostile server that swaps that key changes the code the approving
/// device shows and the mismatch is visible. But a *bound* code is only as strong
/// as it is *wide*: the server can grind ephemeral keypairs until one happens to
/// derive the victim's code. Six digits is 2^20 — about a million X25519 keygens,
/// which is seconds of work and no protection at all. Eight characters over a
/// 32-symbol alphabet is 40 bits, so the same attack costs ~10^12 keygens.
const SAS_LEN: usize = 8;

const X25519_PUB_LEN: usize = 32;
const AES_NONCE_LEN: usize = 12;
const ACCOUNT_ID_PRIVATE_LEN: usize = 32;
// approver_pub (32) || nonce (12) || ciphertext+tag (32 + 16) = 92
const WRAPPED_ACCOUNT_KEY_LEN: usize = X25519_PUB_LEN + AES_NONCE_LEN + ACCOUNT_ID_PRIVATE_LEN + 16;

// ── DTOs ─────────────────────────────────────────────────────────────────────

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
    /// Approved — the new device has installed `account_id_key` locally
    /// and is now enrolled. The frontend should proceed to the main app.
    Approved,
    /// An existing device rejected the request.
    Rejected,
    /// TTL elapsed.
    Expired,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingEnrollmentRequest {
    pub request_id: String,
    pub new_device_id: String,
    pub verification_code: String,
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

// ── Crypto helpers ───────────────────────────────────────────────────────────

/// The verification code the human compares, derived from the new device's
/// ephemeral public key (#793).
///
/// It used to be a random number the new device generated and POSTed to the
/// server, which the approving device then read back — so the server both knew
/// the code and could swap the ephemeral key underneath it while leaving the code
/// alone. Both screens still matched, the user approved, and the account identity
/// key was wrapped to the attacker's key. The digits proved the two screens were
/// discussing the same request; they proved nothing about *which key* was being
/// wrapped to.
///
/// Deriving it from the key fixes exactly that: substitute the key and the code
/// the approver displays changes. The code is NOT a secret — it is a function of
/// a public value, and the server can compute it too. Its job is binding, not
/// confidentiality.
fn derive_verification_code(ephemeral_pub: &[u8]) -> String {
    let hk = Hkdf::<Sha256>::new(None, ephemeral_pub);
    let mut out = [0u8; SAS_LEN];
    hk.expand(ENROLL_SAS_INFO, &mut out)
        .expect("HKDF expand SAS_LEN bytes is infallible");
    out.iter()
        .map(|b| SAS_ALPHABET[(b & 0x1f) as usize] as char)
        .collect()
}

/// Wrap key for the account-identity transfer, bound to the full transcript.
///
/// Both ephemeral public keys are mixed in, so the derived key commits to the
/// exact pair of parties that ran the exchange rather than to the ECDH output
/// alone.
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

// ── Tauri commands ───────────────────────────────────────────────────────────

/// New-device side. Insert a pending enrollment request, publish an inbox
/// event so other devices see it immediately, and return the handle the
/// frontend needs to poll status.
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

    // Generate ephemeral X25519 keypair for this request.
    let mut rng = OsRng;
    let mut private_bytes = [0u8; 32];
    rng.fill_bytes(&mut private_bytes);
    let ephemeral_secret = StaticSecret::from(private_bytes);
    let ephemeral_public = PublicKey::from(&ephemeral_secret);

    // The verification code is DERIVED from this device's ephemeral public key
    // (#793), not chosen at random. Both screens compute it from the same public
    // value, so if anything substitutes that key in flight the two codes diverge
    // and the user sees it. See `derive_verification_code`.
    let verification_code: String = derive_verification_code(ephemeral_public.as_bytes());

    let request_id = Ulid::new().to_string();
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::seconds(ENROLLMENT_TTL_SECS);
    let expires_at_str = expires_at.to_rfc3339();

    // Insert the request row. Server stores only the public ephemeral key
    // and the verification code — the private key stays in memory on this
    // device.
    //
    // This runs on the NEW device, which is pre-enrollment: it has registered
    // (`register_device`) but never ran `ensure_device_cert`, so its
    // `user_device.mls_signature_pub` is still NULL and it cannot produce a
    // signature the DS would accept. So the write is SESSION-gated (the
    // `enrollment_session` minted by re-login `verify_otp`). The DS binds `user_id`
    // and `new_device_id` from the session — never the body. The enrollment
    // APPROVAL / REJECTION (run on an already-enrolled sibling) route via device
    // signature.
    let enrollment_session = state.enrollment_session.lock().await.clone();
    match enrollment_session {
        Some(token) => {
            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD;
            let body = pollis_api::bootstrap::EnrollmentRequestBody {
                request_id: request_id.clone(),
                new_device_ephemeral_pub: b64.encode(ephemeral_public.as_bytes()),
                verification_code: verification_code.clone(),
                created_at: now.to_rfc3339(),
                expires_at: expires_at_str.clone(),
            };
            crate::commands::mls::ds_post_session_ok(
                state,
                &token,
                &body,
            )
            .await?;
        }
        None => {
            // Bucket-C C6: the in-memory `enrollment_session` is gone (e.g. the app
            // restarted between re-login and enrollment). The session-gated write
            // above is the only authable path here — the requesting device is still
            // pre-credential (`mls_signature_pub` NULL), so it cannot device-sign.
            // Fail LOUD + recoverable: the user re-requests the OTP, which re-mints
            // the enrollment session.
            return Err(Error::Other(anyhow::anyhow!(
                "Your sign-in session expired. Please sign in again to enroll this device."
            )));
        }
    }

    // Stash the private key in memory for poll_enrollment_status.
    state
        .enrollment_ephemeral_keys
        .lock()
        .await
        .insert(request_id.clone(), private_bytes.to_vec());

    // The inbox nudge to the user's already-enrolled devices is emitted
    // SERVER-SIDE by the DS `/v1/auth/enrollment-request` handler. This device
    // is pre-enrollment (no signing credential, local DB closed), so a
    // client-side device-signed send-data here would ALWAYS fail with "not
    // signed in for DS request signing" — the DS, which holds the LiveKit admin
    // secret, sends it instead. A sibling that was offline at request time
    // still catches it via its login-time `list_pending_enrollment_requests`
    // poll.
    eprintln!(
        "[enrollment] requested enrollment for user {user_id} device {device_id} (request {request_id})"
    );

    Ok(EnrollmentHandle {
        request_id,
        verification_code,
        expires_at: expires_at_str,
    })
}

/// New-device side. Poll the current status of an enrollment request. On
/// `Approved`, unwrap `wrapped_account_key`, install `account_id_key` in
/// the OS keystore, and run `finalize_enrollment` so the device picks up
/// welcomes and publishes its own cert.
pub async fn poll_enrollment_status(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    // `with_retry` rather than a bare `conn()`: this runs on a device that has
    // no local database, no keystore entry, and no way to retry itself — a
    // dropped libsql stream here used to mean the user watched a spinner until
    // they gave up and restarted the sign-in. Reconnecting once and asking
    // again is the difference between a blip and a dead end.
    let id = request_id.clone();
    let row_data: Option<(String, String, Option<Vec<u8>>, String)> = state
        .remote_db
        .with_retry(|conn| {
            let id = id.clone();
            async move {
                let mut rows = conn
                    .query(
                        "SELECT user_id, new_device_id, status, wrapped_account_key, expires_at \
                         FROM device_enrollment_request WHERE id = ?1",
                        libsql::params![id],
                    )
                    .await?;
                match rows.next().await? {
                    Some(row) => Ok(Some((
                        row.get::<String>(0)?,
                        row.get::<String>(2)?,
                        row.get::<Option<Vec<u8>>>(3).ok().flatten(),
                        row.get::<String>(4)?,
                    ))),
                    None => Ok(None),
                }
            }
        })
        .await?;

    let (user_id, status, wrapped, expires_at_str) = row_data.ok_or_else(|| {
        Error::Other(anyhow::anyhow!(
            "enrollment request {request_id} not found"
        ))
    })?;

    // TTL check — if expired, short-circuit without touching the status column.
    if let Ok(expires_at) = chrono::DateTime::parse_from_rfc3339(&expires_at_str) {
        if chrono::Utc::now() > expires_at.with_timezone(&chrono::Utc) {
            // Also drop the in-memory private key.
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
            // Approver has written the wrapped account key back. Unwrap,
            // install, and finalize.
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

            // Hand off to AppState.unlock with a freshly generated db_key.
            // The frontend's next step is pin-create; set_pin wraps these
            // bytes under the user's PIN and opens the local DB.
            // finalize_device_enrollment runs after that to publish the
            // device cert, key packages, and external-join existing groups.
            *state.unlock.lock().await = Some(
                crate::commands::account_identity::unlock_state_with_fresh_db_key(
                    &user_id,
                    &account_id_private,
                ),
            );

            Ok(EnrollmentStatus::Approved)
        }
        other => Err(Error::Other(anyhow::anyhow!(
            "enrollment request {request_id} has unexpected status {other}"
        ))),
    }
}

/// First delay after a `Pending` answer in [`await_enrollment_approval`].
const AWAIT_BACKOFF_START_MS: u64 = 1_000;
/// Ceiling the backoff climbs to. Approval is a human tapping a button on
/// another device, so a few seconds of latency is invisible; what matters is
/// that a request left open for its full ten minutes costs a handful of reads
/// instead of three hundred.
const AWAIT_BACKOFF_MAX_MS: u64 = 8_000;

/// Wait for an enrollment request to reach a terminal state, then return it.
///
/// ONE call, not a poll loop in the renderer (#874). The new device is
/// pre-enrollment: it has no device key, so it cannot sign a realtime
/// subscription and cannot be pushed to — the answer genuinely has to be
/// fetched. But `setInterval` in the frontend is banned outright by CLAUDE.md,
/// and for good reason: it fired every two seconds regardless of how long the
/// wait had already run, kept firing through transient failures, and outlived
/// the component whenever a cleanup path was missed.
///
/// Here the waiting is the backend's, with exponential backoff between reads
/// and a hard stop at the request's own TTL — so it cannot outlive the thing it
/// is waiting on. The renderer awaits a promise.
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
    let conn = state.remote_db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT id, new_device_id, verification_code, created_at, expires_at \
             FROM device_enrollment_request \
             WHERE user_id = ?1 AND status = 'pending' \
             AND datetime(expires_at) > datetime('now') \
             ORDER BY created_at DESC",
            libsql::params![user_id],
        )
        .await?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(PendingEnrollmentRequest {
            request_id: row.get(0)?,
            new_device_id: row.get(1)?,
            verification_code: row.get(2)?,
            created_at: row.get(3)?,
            expires_at: row.get(4)?,
        });
    }
    Ok(out)
}

/// Existing-device side. Wrap `account_id_key.private` under the
/// requester's ephemeral pub, publish a device cert for the new device,
/// add the new device to every group/DM the approver is in, and mark the
/// request approved.
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

    // 1. Fetch the request row and validate.
    let conn = state.remote_db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT user_id, new_device_id, new_device_ephemeral_pub, verification_code, \
                    status, expires_at \
             FROM device_enrollment_request WHERE id = ?1",
            libsql::params![request_id.clone()],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| {
        Error::Other(anyhow::anyhow!(
            "enrollment request {request_id} not found"
        ))
    })?;

    let user_id: String = row.get(0)?;
    let new_device_id: String = row.get(1)?;
    let ephemeral_pub: Vec<u8> = row.get(2)?;
    let stored_code: String = row.get(3)?;
    let status: String = row.get(4)?;
    let expires_at_str: String = row.get(5)?;
    drop(rows);

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

    // #793: derive the expected code from the ephemeral key WE fetched, and check
    // the user's input against that — never against `stored_code`. The stored value
    // is server-controlled; the derived one is a function of the key this device is
    // about to wrap the account identity key to. Checking the server's copy would
    // re-introduce exactly the substitution this fix exists to stop.
    let expected_code = derive_verification_code(&ephemeral_pub);

    // Constant-time comparison to avoid leaking prefix-match timing.
    if !constant_time_eq(expected_code.as_bytes(), verification_code.as_bytes()) {
        return Err(Error::Other(anyhow::anyhow!(
            "verification code does not match"
        )));
    }

    // The server's stored copy disagreeing with the derived value means the
    // ephemeral key was altered after the new device published it. The check above
    // has already protected the user; this reports the tampering rather than
    // letting it pass silently.
    if !constant_time_eq(stored_code.as_bytes(), expected_code.as_bytes()) {
        return Err(Error::Other(anyhow::anyhow!(
            "enrollment request {request_id}: the stored verification code does not match \
             the one derived from the published ephemeral key — the request was altered in transit"
        )));
    }

    // 2. Load the approver's account_id_key and wrap it to the requester's
    //    ephemeral pub via ECDH + HKDF + AES-256-GCM.
    let signing_key =
        crate::commands::account_identity::load_account_id_key(state, &user_id).await?;
    // The ML-DSA-44 private key IS its 32-byte seed, so the enrollment transfer
    // envelope is byte-identical in size to the Ed25519 one it replaced.
    let account_id_private = signing_key.to_seed().to_vec();

    let wrapped = {
        let mut rng = OsRng;

        let mut approver_priv_bytes = [0u8; 32];
        rng.fill_bytes(&mut approver_priv_bytes);
        let approver_priv = StaticSecret::from(approver_priv_bytes);
        let approver_pub = PublicKey::from(&approver_priv);

        let requester_pub = x25519_public_from_bytes(&ephemeral_pub)?;
        let shared = approver_priv.diffie_hellman(&requester_pub);
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

    // 3. The new device has no device_cert of its own yet (it has never
    //    held account_id_key). We sign one for it here, using the current
    //    `identity_version` from users. The new device will NOT be able
    //    to re-sign its own cert until `finalize_enrollment` stores the
    //    account key — so we publish the cert on its behalf now.
    let identity_version: u32 = {
        let mut rows = conn
            .query(
                "SELECT identity_version FROM users WHERE id = ?1",
                libsql::params![user_id.clone()],
            )
            .await?;
        match rows.next().await? {
            Some(row) => row.get::<i64>(0).unwrap_or(1) as u32,
            None => {
                return Err(Error::Other(anyhow::anyhow!(
                    "user {user_id} not found"
                )))
            }
        }
    };

    // 4. The new device's MLS signing pub will be generated by the new
    //    device itself when it runs `finalize_enrollment`. The cert will
    //    be re-signed and re-published at that point. For now, just mark
    //    the request approved with the wrapped blob — the new device will
    //    handle cert publishing in its own process.
    let _ = (identity_version, new_device_id.clone());

    // 5. Write the wrapped account key and flip status to 'approved'. Routed
    //    through the DS (#419 domains E+G) — the approver is a fully-enrolled
    //    sibling device, so it can sign. The DS binds the request to the signer
    //    (`WHERE id = ? AND user_id = actor`), so a device can only approve
    //    enrollments for its OWN account.
    let metadata = format!("via=approval,approver={approver_device_id}");
    {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let body = pollis_api::account::ApproveEnrollmentBody {
            request_id: request_id.clone(),
            wrapped_account_key: b64.encode(&wrapped),
            approved_by_device_id: approver_device_id,
            // The DS's no-auth fallback for the acting user
            // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
            // user and this must EQUAL it; auth off → this IS the actor, and a
            // body without it is refused outright. Sending it never widens what
            // the caller may do.
            user_id: Some(user_id.clone()),
        };
        crate::commands::mls::ds_post_ok(state, &body).await?;

        // 6. Record a security event (best-effort).
        let ev = pollis_api::account::SecurityEventBody {
            kind: "device_enrolled".to_string(),
            device_id: Some(new_device_id.clone()),
            metadata: Some(metadata),
            // The DS's no-auth fallback for the acting user
            // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
            // user and this must EQUAL it; auth off → this IS the actor, and a
            // body without it is refused outright. Sending it never widens what
            // the caller may do.
            user_id: Some(user_id.clone()),
        };
        if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
            eprintln!("[enrollment] DS security-event failed (non-fatal): {e}");
        }
    }

    // 7. MLS group addition is deferred — the new device hasn't published
    //    KPs yet so reconcile can't add it. Instead, finalize_enrollment on
    //    the new device publishes KPs and sends an `enrollment_finalized`
    //    event. The approver (or any sibling) picks that up, reconciles all
    //    groups, and the new device receives Welcomes.

    eprintln!(
        "[enrollment] approved request {request_id} for new device {new_device_id} of user {user_id}"
    );

    Ok(())
}

/// New-device side. Recover `account_id_key` from the server-stored
/// recovery blob using the user's Secret Key, install it locally, then
/// run the same finalization path as the approval flow so the device
/// publishes its cert, its KPs, and joins every existing group via
/// external commits.
pub async fn recover_with_secret_key(
    state: &Arc<AppState>,
    user_id: String,
    secret_key: String,
) -> Result<()> {
    // 1. Fetch the wrapped account identity blob from Turso.
    let (salt, nonce, wrapped_key) = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn
            .query(
                "SELECT salt, nonce, wrapped_key FROM account_recovery WHERE user_id = ?1",
                libsql::params![user_id.clone()],
            )
            .await?;
        match rows.next().await? {
            Some(row) => (
                row.get::<Vec<u8>>(0)?,
                row.get::<Vec<u8>>(1)?,
                row.get::<Vec<u8>>(2)?,
            ),
            None => {
                return Err(Error::Other(anyhow::anyhow!(
                    "No recovery blob found for user {user_id}"
                )))
            }
        }
    };

    // 2. Unwrap with the user-provided Secret Key. The helper normalizes
    //    the input (whitespace, case, dashes) before derivation.
    let account_id_private = crate::commands::account_identity::unwrap_recovery_blob(
        &secret_key,
        &salt,
        &nonce,
        &wrapped_key,
    )?;

    // 3. Hand the unwrapped key to AppState.unlock with a freshly
    //    generated db_key. Frontend proceeds to pin-create; set_pin
    //    wraps both, and finalize_device_enrollment finishes the
    //    cert / KP / external-join work.
    *state.unlock.lock().await = Some(
        crate::commands::account_identity::unlock_state_with_fresh_db_key(
            &user_id,
            &account_id_private,
        ),
    );

    // 4. A security-event audit row would go here, but this runs PRE-FINALIZE —
    // the account key was just unwrapped into `AppState.unlock`, yet this device
    // has NOT published its cross-signing cert (`finalize_enrollment` →
    // `ensure_device_cert` runs after `set_pin`), so its
    // `user_device.mls_signature_pub` is still NULL and it cannot device-sign a
    // request the DS gate would accept. The only DS audit endpoint
    // (`/v1/security-events`) is device-signed, so there is no authable path for
    // this NON-load-bearing audit row here — it is skipped.

    Ok(())
}

/// Soft recovery path. The user has no Secret Key, no sibling device,
/// and no way to approve. They confirm by typing their email, and we:
///   1. Verify the email matches `users.email` for this user_id.
///   2. Generate a brand-new `account_id_key` and bump
///      `users.identity_version`, orphaning every other device.
///   3. Remove the user from all groups and DMs (including ownership
///      handoff), delete stale key packages/welcomes/device rows,
///      and wipe the local DB.
///   4. Re-open a fresh local DB and run `finalize_enrollment` so
///      this device publishes a fresh cert and KPs. The user ends
///      up in a clean "no groups" state — admins must re-add them.
///   5. Return the new Secret Key for the user to save.
///
/// This is a destructive operation. The frontend must display a very
/// clear warning before calling it.
pub async fn reset_identity_and_recover(
    state: &Arc<AppState>,
    user_id: String,
    confirm_email: String,
) -> Result<String> {
    // 1. Verify the confirmation email matches what we have on file.
    let conn = state.remote_db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT email FROM users WHERE id = ?1",
            libsql::params![user_id.clone()],
        )
        .await?;
    let stored_email: String = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => {
            return Err(Error::Other(anyhow::anyhow!(
                "user {user_id} not found"
            )))
        }
    };
    drop(rows);

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

    // 3. Remove the user from all groups and DMs in Turso so they start
    //    fresh. Unlike delete_account we do NOT issue MLS remove commits
    //    (the old identity is already invalidated) and do NOT delete the
    //    user's sent messages (other members can still read them).
    {
        let current_device_id = state.device_id.lock().await.clone();

        // Main-DB membership + device cleanup. Route through the DS (#419
        // domains E+G) as ONE transaction when configured: the group-ownership
        // handoff, the group/DM membership + key-package deletes, and the
        // other-device orphaning all commit together or not at all — a
        // half-cleaned account is a corrupt state. Credential: an enrolled
        // caller (reset from a signed-in session) device-signs; the primary
        // caller — a PRE-ENROLLMENT device on the login gate, which has no
        // signing key and no open local DB — authenticates with the
        // verified-OTP bootstrap session instead (the DS's `gate_or_session`).
        let body = pollis_api::account::ResetRecoverBody {
            current_device_id,
            // The DS's no-auth fallback for the acting user
            // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
            // user and this must EQUAL it; auth off → this IS the actor, and a
            // body without it is refused outright. Sending it never widens what
            // the caller may do.
            user_id: Some(user_id.clone()),
        };
        crate::commands::mls::ds_post_signed_or_session_ok(state, &body)
            .await?;

        // Delete pending MLS welcomes (W8 seam). Route through the Delivery
        // Service (sole writer of the log DB), with the same signed-or-session
        // credential as the reset-recover write above.
        {
            let body = pollis_api::writes::PurgeBody {
                // The DS's no-auth fallback for the acting user
                // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
                // user and this must EQUAL it; auth off → this IS the actor, and a
                // body without it is refused outright. Sending it never widens what
                // the caller may do.
                user_id: Some(user_id.clone()),
            };
            match crate::commands::mls::ds_post_signed_or_session(state, &body).await {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => {
                    let s = resp.status();
                    let txt = resp.text().await.unwrap_or_default();
                    eprintln!("[reset] DS welcomes purge {s}: {txt}");
                }
                Err(e) => {
                    eprintln!("[reset] DS welcomes purge failed: {e}");
                }
            }
        }

        // (Other devices are orphaned as part of the membership/device cleanup
        // above — via the DS reset-recover transaction.)

        // Wipe local DB (MLS group state, cached messages — all invalid now)
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

        // Don't reopen the DB here — reset_identity (above) already
        // populated AppState.unlock with the new account_id_key and a
        // fresh db_key. The frontend routes to pin-create next; set_pin
        // wraps and opens the new DB, finalize_device_enrollment
        // publishes the device cert + key packages.

        eprintln!(
            "[reset] cleaned up memberships, key packages, welcomes, devices, and local DB for {user_id}"
        );
    }

    Ok(new_secret_key)
}

/// Fetch the user's security event log, most recent first. Used by the
/// Security settings page to surface enrollments, rejections, identity
/// resets, and secret-key rotations.
pub async fn list_security_events(
    state: &Arc<AppState>,
    user_id: String,
    limit: Option<i64>,
) -> Result<Vec<SecurityEvent>> {
    let limit = limit.unwrap_or(100).clamp(1, 500);
    let conn = state.remote_db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT id, kind, device_id, created_at, metadata \
             FROM security_event \
             WHERE user_id = ?1 \
             ORDER BY created_at DESC \
             LIMIT ?2",
            libsql::params![user_id, limit],
        )
        .await?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(SecurityEvent {
            id: row.get(0)?,
            kind: row.get(1)?,
            device_id: row.get::<Option<String>>(2).ok().flatten(),
            created_at: row.get(3)?,
            metadata: row.get::<Option<String>>(4).ok().flatten(),
        });
    }
    Ok(out)
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

    let conn = state.remote_db.conn().await?;

    // Fetch user_id / new_device_id for the security event.
    let mut rows = conn
        .query(
            "SELECT user_id, new_device_id FROM device_enrollment_request WHERE id = ?1",
            libsql::params![request_id.clone()],
        )
        .await?;
    let (_user_id, new_device_id): (String, String) = match rows.next().await? {
        Some(row) => (row.get(0)?, row.get(1)?),
        None => {
            return Err(Error::Other(anyhow::anyhow!(
                "enrollment request {request_id} not found"
            )))
        }
    };
    drop(rows);

    // Flip the request to 'rejected'. Routed through the DS (#419 domains E+G) —
    // the rejecting device is a fully-enrolled sibling, so it can sign; the DS
    // binds the request to the signer (`WHERE id = ? AND user_id = actor`).
    let body = pollis_api::account::RejectEnrollmentBody {
        request_id,
        approved_by_device_id: approver_device_id,
        // The DS's no-auth fallback for the acting user
        // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
        // user and this must EQUAL it; auth off → this IS the actor, and a
        // body without it is refused outright. Sending it never widens what
        // the caller may do.
        user_id: Some(_user_id.clone()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    let ev = pollis_api::account::SecurityEventBody {
        kind: "device_rejected".to_string(),
        device_id: Some(new_device_id),
        metadata: None,
        // The DS's no-auth fallback for the acting user
        // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
        // user and this must EQUAL it; auth off → this IS the actor, and a
        // body without it is refused outright. Sending it never widens what
        // the caller may do.
        user_id: Some(_user_id.clone()),
    };
    if let Err(e) = crate::commands::mls::ds_post_ok(state, &ev).await {
        eprintln!("[enrollment] DS security-event (reject) failed (non-fatal): {e}");
    }

    Ok(())
}

// ── Finalization ─────────────────────────────────────────────────────────────

/// Run after a successful account_id_key install on the new device.
/// Shared by BOTH the approval path and the Secret Key path:
///   1. Publishes this device's cross-signing cert.
///   2. Publishes fresh MLS key packages.
///   3. Pulls any welcomes the approving device posted (approval path).
///   4. For every group/DM the user is a member of where this device
///      still has no local MLS state, externally joins via the stored
///      GroupInfo. This is the critical step for the Secret Key path
///      where no welcomes exist — but it's also safe for the approval
///      path because it short-circuits when `has_local_group` is true.
/// Tauri-exposed wrapper around `finalize_enrollment`. Frontend invokes
/// this after `set_pin` (or change-PIN flow) completes for an enrollment
/// path: device approval, Secret-Key recovery, or identity reset. By
/// that point the local DB is open and `AppState.unlock` carries the
/// account identity, so the cert / KP / external-join work succeeds.
///
/// Safe to call when there's nothing to do — `ensure_*` and the
/// external-join loop are idempotent.
pub async fn finalize_device_enrollment(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<()> {
    finalize_enrollment(state, &user_id).await
}

async fn finalize_enrollment(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("device_id not set during finalize"))?;

    // Publish this device's cross-signing cert now that account_id_key is
    // in the keystore. No-op if already present.
    if let Err(e) = crate::commands::mls::ensure_device_cert(state, user_id, &device_id).await {
        eprintln!("[enrollment] finalize: ensure_device_cert failed: {e}");
    }

    // Publish fresh key packages so other devices can claim them for
    // future groups.
    if let Err(e) =
        crate::commands::mls::ensure_mls_key_package(state, user_id, &device_id).await
    {
        eprintln!("[enrollment] finalize: ensure_mls_key_package failed: {e}");
    }

    // External-join every group/DM this user belongs to. The new device
    // uses the stored GroupInfo to self-add via an MLS external commit —
    // no coordination with sibling devices required.
    let conn = state.remote_db.conn().await?;
    let group_ids = fetch_user_group_ids(&conn, user_id).await?;
    let dm_ids = fetch_user_dm_ids(&conn, user_id).await?;
    let candidate_ids: Vec<String> = group_ids.into_iter().chain(dm_ids.into_iter()).collect();

    for conv_id in candidate_ids {
        let already_joined = {
            let guard = state.local_db.lock().await;
            guard.as_ref().map_or(false, |db| {
                crate::commands::mls::has_local_group(db.conn(), &conv_id)
            })
        };
        if already_joined {
            continue;
        }

        if let Err(e) = crate::commands::mls::external_join_group(state, &conv_id, user_id).await {
            eprintln!(
                "[enrollment] finalize: external_join_group({conv_id}) failed: {e}"
            );
        }
    }

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn fetch_user_group_ids(conn: &libsql::Connection, user_id: &str) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT g.id FROM groups g \
             JOIN group_member gm ON gm.group_id = g.id \
             WHERE gm.user_id = ?1",
            libsql::params![user_id],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get::<String>(0)?);
    }
    Ok(out)
}

async fn fetch_user_dm_ids(conn: &libsql::Connection, user_id: &str) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT dc.id FROM dm_channel dc \
             JOIN dm_channel_member dcm ON dcm.dm_channel_id = dc.id \
             WHERE dcm.user_id = ?1",
            libsql::params![user_id],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get::<String>(0)?);
    }
    Ok(out)
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

// ── Tests ────────────────────────────────────────────────────────────────────

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

    #[test]
    fn constant_time_eq_matches_regular_eq() {
        assert!(constant_time_eq(b"123456", b"123456"));
        assert!(!constant_time_eq(b"123456", b"123457"));
        assert!(!constant_time_eq(b"123", b"1234"));
        assert!(constant_time_eq(b"", b""));
    }

    // ── #793: the verification code must BIND the ephemeral key ──────────────
    //
    // The defect these pin: the code used to be a random number the new device
    // POSTed to the server, and the approver read it back. A server that swapped
    // `new_device_ephemeral_pub` and left the code alone produced two matching
    // screens, a confirming user, and the account identity key wrapped to the
    // attacker.

    #[test]
    fn the_verification_code_is_determined_by_the_ephemeral_key() {
        let pub_a = [7u8; 32];
        assert_eq!(
            derive_verification_code(&pub_a),
            derive_verification_code(&pub_a),
            "both devices derive from the same public value, so they must agree"
        );
    }

    #[test]
    fn substituting_the_ephemeral_key_changes_the_code() {
        // Exactly the attack in #793: the key is swapped in flight.
        let honest = [7u8; 32];
        let attacker = [8u8; 32];
        assert_ne!(
            derive_verification_code(&honest),
            derive_verification_code(&attacker),
            "a swapped key MUST change the code the approver shows, or the human check is decorative"
        );
    }

    #[test]
    fn a_one_bit_change_in_the_key_changes_the_code() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        b[31] = 1;
        assert_ne!(derive_verification_code(&a), derive_verification_code(&b));
        a[0] = 1;
        assert_ne!(derive_verification_code(&a), derive_verification_code(&b));
    }

    #[test]
    fn the_code_is_wide_enough_that_grinding_is_not_cheap() {
        // Width is the other half of the fix. A bound code is only as strong as it
        // is wide: the server can grind ephemeral keypairs until one derives the
        // victim's code. At six digits that is 2^20 — about a million keygens, or
        // seconds. Eight characters over 32 symbols is 40 bits.
        let code = derive_verification_code(&[3u8; 32]);
        assert_eq!(code.chars().count(), SAS_LEN);
        assert_eq!(SAS_LEN, 8);
        assert!(
            (SAS_ALPHABET.len() as f64).log2() * SAS_LEN as f64 >= 40.0,
            "the human-compared code must carry at least 40 bits"
        );
    }

    #[test]
    fn the_code_only_uses_unambiguous_characters() {
        // I, L, O and U are excluded so a user reading digits aloud cannot
        // manufacture a false mismatch (or a false match).
        for seed in 0u8..64 {
            for c in derive_verification_code(&[seed; 32]).chars() {
                assert!(SAS_ALPHABET.contains(&(c as u8)), "unexpected character {c}");
                assert!(!"ILOU".contains(c), "ambiguous character {c} in the code");
            }
        }
    }

    // ── Transcript binding on the wrap key ───────────────────────────────────

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

        // The real new device cannot open it — and, with the code now bound to the
        // key, would never have been approved in the first place.
        assert!(unwrap_account_key(&blob, &victim_bytes).is_err());
        let _ = victim_pub;
    }
}
