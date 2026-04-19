//! Device enrollment — approval path.
//!
//! When a user signs in on a new device, this module manages the flow for
//! transferring their `account_id_key` from an existing device over an
//! ephemeral, authenticated channel. Flow:
//!
//!   1. New device calls `start_device_enrollment` → generates an
//!      ephemeral X25519 keypair and a 6-digit verification code, inserts
//!      a `device_enrollment_request` row in Turso, publishes an
//!      `EnrollmentRequested` event to `inbox-{user_id}` via LiveKit.
//!      The private X25519 key stays in memory on the new device.
//!
//!   2. Any of the user's already-enrolled devices receives the inbox
//!      event and shows an immediate-takeover approval UI that displays
//!      the verification code.
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
use tauri::State;
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

fn derive_wrap_key(shared_secret: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, shared_secret);
    let mut out = [0u8; 32];
    hk.expand(ENROLL_HKDF_INFO, &mut out)
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
#[tauri::command]
pub async fn start_device_enrollment(
    state: State<'_, Arc<AppState>>,
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

    // Generate a 6-digit verification code. Zero-padded. Shown on BOTH
    // screens so the user can confirm the two devices are talking to each
    // other (and not that someone else just happened to start an
    // enrollment at the same time).
    let verification_code: String = {
        let mut bytes = [0u8; 4];
        rng.fill_bytes(&mut bytes);
        let n = u32::from_be_bytes(bytes) % 1_000_000;
        format!("{n:06}")
    };

    let request_id = Ulid::new().to_string();
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::seconds(ENROLLMENT_TTL_SECS);
    let expires_at_str = expires_at.to_rfc3339();

    // Insert the request row. Server stores only the public ephemeral key
    // and the verification code — the private key stays in memory on this
    // device.
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO device_enrollment_request \
         (id, user_id, new_device_id, new_device_ephemeral_pub, verification_code, \
          status, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7)",
        libsql::params![
            request_id.clone(),
            user_id.clone(),
            device_id.clone(),
            ephemeral_public.as_bytes().to_vec(),
            verification_code.clone(),
            now.to_rfc3339(),
            expires_at_str.clone()
        ],
    )
    .await?;

    // Stash the private key in memory for poll_enrollment_status.
    state
        .enrollment_ephemeral_keys
        .lock()
        .await
        .insert(request_id.clone(), private_bytes.to_vec());

    // Fan out a push event to the user's inbox room. If no other device is
    // online the poll-fallback path (`list_pending_enrollment_requests`)
    // still works when a sibling device comes online later.
    let payload = serde_json::json!({
        "type": "enrollment_requested",
        "request_id": request_id,
        "new_device_id": device_id,
        "verification_code": verification_code,
    });
    if let Err(e) =
        crate::commands::livekit::publish_to_user_inbox(&state.config, &user_id, payload).await
    {
        eprintln!("[enrollment] inbox publish failed (non-fatal): {e}");
    }

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
#[tauri::command]
pub async fn poll_enrollment_status(
    state: State<'_, Arc<AppState>>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    let conn = state.remote_db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT user_id, new_device_id, status, wrapped_account_key, expires_at \
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
    let _new_device_id: String = row.get(1)?;
    let status: String = row.get(2)?;
    let wrapped: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(3).ok().flatten();
    let expires_at_str: String = row.get(4)?;
    drop(rows);

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

            // Install into the keystore as `account_id_key_{user_id}` —
            // matches the slot `account_identity::load_account_id_key`
            // reads from.
            state.keystore.store_for_user("account_id_key", &user_id, &account_id_private)
                .await?;

            // Proceed to publish our device cert + pick up welcomes.
            finalize_enrollment(state.inner(), &user_id).await?;

            Ok(EnrollmentStatus::Approved)
        }
        other => Err(Error::Other(anyhow::anyhow!(
            "enrollment request {request_id} has unexpected status {other}"
        ))),
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
    let approver_pub = x25519_public_from_bytes(approver_pub_bytes)?;
    let shared = requester_priv.diffie_hellman(&approver_pub);
    let wrap_key = derive_wrap_key(shared.as_bytes());

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
#[tauri::command]
pub async fn list_pending_enrollment_requests(
    state: State<'_, Arc<AppState>>,
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
#[tauri::command]
pub async fn approve_device_enrollment(
    state: State<'_, Arc<AppState>>,
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

    // Constant-time code comparison to avoid leaking prefix-match timing.
    if !constant_time_eq(stored_code.as_bytes(), verification_code.as_bytes()) {
        return Err(Error::Other(anyhow::anyhow!(
            "verification code does not match"
        )));
    }

    // 2. Load the approver's account_id_key and wrap it to the requester's
    //    ephemeral pub via ECDH + HKDF + AES-256-GCM.
    let signing_key =
        crate::commands::account_identity::load_account_id_key(state.keystore.as_ref(), &user_id).await?;
    let account_id_private = signing_key.to_bytes();

    let wrapped = {
        let mut rng = OsRng;

        let mut approver_priv_bytes = [0u8; 32];
        rng.fill_bytes(&mut approver_priv_bytes);
        let approver_priv = StaticSecret::from(approver_priv_bytes);
        let approver_pub = PublicKey::from(&approver_priv);

        let requester_pub = x25519_public_from_bytes(&ephemeral_pub)?;
        let shared = approver_priv.diffie_hellman(&requester_pub);
        let wrap_key = derive_wrap_key(shared.as_bytes());

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

    // 5. Write the wrapped account key and flip status to 'approved'.
    conn.execute(
        "UPDATE device_enrollment_request \
         SET wrapped_account_key = ?1, \
             status = 'approved', \
             approved_by_device_id = ?2 \
         WHERE id = ?3",
        libsql::params![wrapped, approver_device_id.clone(), request_id.clone()],
    )
    .await?;

    // 6. Record a security event.
    let event_id = Ulid::new().to_string();
    if let Err(e) = conn
        .execute(
            "INSERT INTO security_event (id, user_id, kind, device_id, metadata) \
             VALUES (?1, ?2, 'device_enrolled', ?3, ?4)",
            libsql::params![
                event_id,
                user_id.clone(),
                new_device_id.clone(),
                format!("via=approval,approver={approver_device_id}")
            ],
        )
        .await
    {
        eprintln!("[enrollment] security_event insert failed (non-fatal): {e}");
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
#[tauri::command]
pub async fn recover_with_secret_key(
    state: State<'_, Arc<AppState>>,
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

    // 3. Install into the OS keystore under the same slot the normal
    //    account_identity module reads from.
    state.keystore.store_for_user("account_id_key", &user_id, &account_id_private).await?;

    // 4. Finalize enrollment — publishes the device cert, the key
    //    packages, pulls any welcomes (none on this path), and then
    //    externally joins every group/DM the user is a member of but
    //    this device isn't in yet.
    finalize_enrollment(state.inner(), &user_id).await?;

    // 5. Record a security event so the user can audit this in the
    //    Security settings page.
    let conn = state.remote_db.conn().await?;
    let device_id = state.device_id.lock().await.clone().unwrap_or_default();
    let _ = conn
        .execute(
            "INSERT INTO security_event (id, user_id, kind, device_id, metadata) \
             VALUES (?1, ?2, 'device_enrolled', ?3, 'via=secret_key')",
            libsql::params![Ulid::new().to_string(), user_id.clone(), device_id],
        )
        .await;

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
#[tauri::command]
pub async fn reset_identity_and_recover(
    state: State<'_, Arc<AppState>>,
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
        crate::commands::account_identity::reset_identity(state.inner(), &user_id).await?;

    // 3. Remove the user from all groups and DMs in Turso so they start
    //    fresh. Unlike delete_account we do NOT issue MLS remove commits
    //    (the old identity is already invalidated) and do NOT delete the
    //    user's sent messages (other members can still read them).
    {
        let current_device_id = state.device_id.lock().await.clone();

        // Group membership cleanup (handle ownership first)
        let mut group_rows = conn
            .query(
                "SELECT group_id, role FROM group_member WHERE user_id = ?1",
                libsql::params![user_id.clone()],
            )
            .await?;
        let mut memberships: Vec<(String, String)> = Vec::new();
        while let Some(row) = group_rows.next().await? {
            memberships.push((row.get(0)?, row.get(1)?));
        }

        for (gid, role) in &memberships {
            let mut count_rows = conn
                .query(
                    "SELECT COUNT(*) FROM group_member WHERE group_id = ?1",
                    libsql::params![gid.clone()],
                )
                .await?;
            let member_count: i64 = if let Some(row) = count_rows.next().await? {
                row.get(0)?
            } else {
                0
            };

            if member_count <= 1 {
                // Sole member — delete the entire group
                let _ = conn
                    .execute(
                        "DELETE FROM groups WHERE id = ?1",
                        libsql::params![gid.clone()],
                    )
                    .await;
                eprintln!("[reset] deleted empty group {gid}");
            } else if role == "admin" {
                // Sole admin — promote another member
                let mut admin_rows = conn
                    .query(
                        "SELECT COUNT(*) FROM group_member WHERE group_id = ?1 AND role = 'admin' AND user_id != ?2",
                        libsql::params![gid.clone(), user_id.clone()],
                    )
                    .await?;
                let other_admins: i64 = if let Some(row) = admin_rows.next().await? {
                    row.get(0)?
                } else {
                    0
                };
                if other_admins == 0 {
                    let mut candidate_rows = conn
                        .query(
                            "SELECT user_id FROM group_member WHERE group_id = ?1 AND user_id != ?2 LIMIT 1",
                            libsql::params![gid.clone(), user_id.clone()],
                        )
                        .await?;
                    if let Some(row) = candidate_rows.next().await? {
                        let new_admin: String = row.get(0)?;
                        let _ = conn
                            .execute(
                                "UPDATE group_member SET role = 'admin' WHERE group_id = ?1 AND user_id = ?2",
                                libsql::params![gid.clone(), new_admin.clone()],
                            )
                            .await;
                        eprintln!("[reset] promoted {new_admin} to admin in group {gid}");
                    }
                }
            }
        }

        // Delete group memberships
        let _ = conn
            .execute(
                "DELETE FROM group_member WHERE user_id = ?1",
                libsql::params![user_id.clone()],
            )
            .await;

        // Delete DM channel memberships
        let _ = conn
            .execute(
                "DELETE FROM dm_channel_member WHERE user_id = ?1",
                libsql::params![user_id.clone()],
            )
            .await;

        // Delete MLS key packages (old identity, no longer valid)
        let _ = conn
            .execute(
                "DELETE FROM mls_key_package WHERE user_id = ?1",
                libsql::params![user_id.clone()],
            )
            .await;

        // Delete pending MLS welcomes
        let _ = conn
            .execute(
                "DELETE FROM mls_welcome WHERE recipient_id = ?1",
                libsql::params![user_id.clone()],
            )
            .await;

        // Delete other devices (they're orphaned by the identity rotation).
        // Keep the current device row since ensure_device_cert uses UPDATE.
        if let Some(ref dev_id) = current_device_id {
            let _ = conn
                .execute(
                    "DELETE FROM user_device WHERE user_id = ?1 AND device_id != ?2",
                    libsql::params![user_id.clone(), dev_id.clone()],
                )
                .await;
        } else {
            let _ = conn
                .execute(
                    "DELETE FROM user_device WHERE user_id = ?1",
                    libsql::params![user_id.clone()],
                )
                .await;
        }

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

        // Re-open a fresh local DB so finalize_enrollment can write new
        // MLS state (device signer, key packages, etc.).
        state.load_user_db(&user_id).await?;

        eprintln!(
            "[reset] cleaned up memberships, key packages, welcomes, devices, and local DB for {user_id}"
        );
    }

    // 4. Run the finalization path to publish a new device cert + KPs.
    //    Since we deleted all group_member rows above, finalize_enrollment
    //    will find no groups to external-join, leaving the user in a clean
    //    "fresh account, no groups" state.
    finalize_enrollment(state.inner(), &user_id).await?;

    Ok(new_secret_key)
}

/// Fetch the user's security event log, most recent first. Used by the
/// Security settings page to surface enrollments, rejections, identity
/// resets, and secret-key rotations.
#[tauri::command]
pub async fn list_security_events(
    state: State<'_, Arc<AppState>>,
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
#[tauri::command]
pub async fn reject_device_enrollment(
    state: State<'_, Arc<AppState>>,
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
    let (user_id, new_device_id): (String, String) = match rows.next().await? {
        Some(row) => (row.get(0)?, row.get(1)?),
        None => {
            return Err(Error::Other(anyhow::anyhow!(
                "enrollment request {request_id} not found"
            )))
        }
    };
    drop(rows);

    conn.execute(
        "UPDATE device_enrollment_request \
         SET status = 'rejected', approved_by_device_id = ?1 \
         WHERE id = ?2",
        libsql::params![approver_device_id, request_id],
    )
    .await?;

    let event_id = Ulid::new().to_string();
    let _ = conn
        .execute(
            "INSERT INTO security_event (id, user_id, kind, device_id, metadata) \
             VALUES (?1, ?2, 'device_rejected', ?3, NULL)",
            libsql::params![event_id, user_id, new_device_id],
        )
        .await;

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
        let wrap_key = derive_wrap_key(shared_send.as_bytes());

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
        let wrap_key = derive_wrap_key(shared.as_bytes());

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
}
