//! Multi-device enrollment + Secret-Key recovery (M4).
//!
//! Thin, order-enforcing wrappers over
//! `pollis_core::commands::device_enrollment` — the exact command surface the
//! desktop app reaches over Tauri `invoke`. We do **not** fork any logic; these
//! functions exist so the (later, M4b) ratatui screens have one ergonomic,
//! typed entry point per step and cannot get the call ORDER wrong.
//!
//! ## The two flows this covers (spec §7 "Additional-device enrollment" + §11)
//!
//! Both add a brand-new terminal to an account that already exists on another
//! device. Neither inherits history (§ bounded-history: a new device starts
//! empty) — but afterwards the device holds its own MLS leaf and can send/receive
//! at the current epoch.
//!
//! ### A. Sibling-approval enrollment
//!
//! The new device proves the account email (`request_otp` → `verify_otp`, which
//! resolves to the existing `user_id`, sets `enrollment_required = true`, and
//! mints the in-memory `enrollment_session` that gates the request write), then:
//!
//! ```text
//! new device:  request_enrollment(user_id) -> EnrollmentHandle { request_id, code }
//! old device:  pending_requests(user_id) -> [.. match request_id, confirm code ..]
//! old device:  approve(request_id, code)
//! new device:  enrollment_status(request_id) -> Approved   (installs account key into AppState.unlock)
//! new device:  set_pin -> finalize(user_id) -> initialize_identity   (publishes cert/KPs, external-joins groups)
//! ```
//!
//! ### B. Secret-Key recovery
//!
//! No sibling online. The new device proves the email the same way, then unwraps
//! the server-stored `account_recovery` blob with the user's Secret Key:
//!
//! ```text
//! new device:  recover(user_id, secret_key)   (installs account key into AppState.unlock)
//! new device:  set_pin -> finalize(user_id) -> initialize_identity
//! ```
//!
//! `reset_and_recover` is the last-resort soft path (no sibling, no Secret Key):
//! it rotates the account identity, orphans every other device, and returns a
//! **new** Secret Key the user must save.

use std::sync::Arc;

use anyhow::Result;
use pollis_core::commands::device_enrollment;
use pollis_core::state::AppState;

// Re-export the DTOs so the M4b UI (and the smokes) can render/inspect them
// without reaching into `pollis_core` directly.
pub use pollis_core::commands::device_enrollment::{
    EnrollmentHandle, EnrollmentStatus, PendingEnrollmentRequest, SecurityEvent,
};

// ── New-device side ────────────────────────────────────────────────────────────

/// New device: kick off an enrollment request. Returns the handle carrying the
/// `request_id` to poll and the 6-digit verification code to display so the user
/// can confirm it on their existing device. The session-gated write goes through
/// the Delivery Service; the ephemeral private key stays in `AppState`.
///
/// Precondition: `verify_otp` has run against the existing account's email (so
/// `state.enrollment_session` is set).
pub async fn request_enrollment(state: &Arc<AppState>, user_id: String) -> Result<EnrollmentHandle> {
    let handle = device_enrollment::start_device_enrollment(state, user_id).await?;
    Ok(handle)
}

/// New device: poll the current status of an enrollment request. On
/// [`EnrollmentStatus::Approved`] the core has already unwrapped the account key
/// into `state.unlock` — the caller proceeds to `set_pin` → [`finalize`].
pub async fn enrollment_status(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    let status = device_enrollment::poll_enrollment_status(state, request_id).await?;
    Ok(status)
}

/// New device: finish an enrollment/recovery. Publishes this device's
/// cross-signing cert + fresh MLS key packages and external-joins every group/DM
/// the user belongs to. Idempotent. Call **after** `set_pin` has opened the local
/// DB (the account key is in `state.unlock` by then).
pub async fn finalize(state: &Arc<AppState>, user_id: String) -> Result<()> {
    device_enrollment::finalize_device_enrollment(state, user_id).await?;
    Ok(())
}

/// New device: Secret-Key recovery. Unwraps the server-stored `account_recovery`
/// blob with the user-entered Secret Key and installs the account key into
/// `state.unlock`. The caller then runs `set_pin` → [`finalize`] →
/// `initialize_identity`, exactly like the approval path's tail.
pub async fn recover(state: &Arc<AppState>, user_id: String, secret_key: String) -> Result<()> {
    device_enrollment::recover_with_secret_key(state, user_id, secret_key).await?;
    Ok(())
}

/// New device: last-resort soft recovery. Verifies `confirm_email`, rotates the
/// account identity (orphaning every other device and clearing group membership),
/// installs the fresh key into `state.unlock`, and returns the **new** Secret Key
/// for the user to save. Destructive — the UI must warn clearly before calling.
pub async fn reset_and_recover(
    state: &Arc<AppState>,
    user_id: String,
    confirm_email: String,
) -> Result<String> {
    let new_secret_key =
        device_enrollment::reset_identity_and_recover(state, user_id, confirm_email).await?;
    Ok(new_secret_key)
}

// ── Existing-device side ────────────────────────────────────────────────────────

/// Existing device: list the open enrollment requests for this account (the
/// poll-fallback for a missed inbox push). Show each with its verification code
/// so the user can confirm the code matches the new device's screen.
pub async fn pending_requests(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<Vec<PendingEnrollmentRequest>> {
    let pending = device_enrollment::list_pending_enrollment_requests(state, user_id).await?;
    Ok(pending)
}

/// Existing device: approve a pending request, confirming the `verification_code`
/// the new device displayed (anti-mis-approval). Wraps the account key to the
/// requester's ephemeral pub and flips the request to `approved` via the DS.
pub async fn approve(
    state: &Arc<AppState>,
    request_id: String,
    verification_code: String,
) -> Result<()> {
    device_enrollment::approve_device_enrollment(state, request_id, verification_code).await?;
    Ok(())
}

/// Existing device: reject a pending request. The new device's poller then sees
/// [`EnrollmentStatus::Rejected`].
pub async fn reject(state: &Arc<AppState>, request_id: String) -> Result<()> {
    device_enrollment::reject_device_enrollment(state, request_id).await?;
    Ok(())
}
