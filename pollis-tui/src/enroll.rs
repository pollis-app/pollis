//! Multi-device enrollment + Secret-Key recovery (M4).
//!
//! Thin, order-enforcing wrappers over
//! `pollis_core::commands::device_enrollment` — the exact command surface the
//! desktop app reaches over Tauri `invoke`, wrapped so the screens cannot get the call ORDER wrong.
//!
//! ## The two flows this covers (spec §7 "Additional-device enrollment" + §11)
//!
//! Neither inherits history; the device then holds its own MLS leaf.
//!
//! ### A. Sibling-approval enrollment
//!
//! The new device proves the account email (`request_otp` → `verify_otp`), then:
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
//! No sibling online: the email is proven the same way, then the Secret Key unwraps the account:
//!
//! ```text
//! new device:  recover(user_id, secret_key)   (installs account key into AppState.unlock)
//! new device:  set_pin -> finalize(user_id) -> initialize_identity
//! ```
//!
//! `reset_and_recover` is the last resort: it rotates the identity and returns a **new** Secret
//! Key.

use std::sync::Arc;

use anyhow::Result;
use pollis_core::commands::{auth, device_enrollment, pin};
use pollis_core::state::AppState;

pub use pollis_core::commands::device_enrollment::{
    EnrollmentHandle, EnrollmentStatus, PendingEnrollmentRequest, SecurityEvent,
};

/// New device: kick off an enrollment request; needs `verify_otp` against the account email first.
pub async fn request_enrollment(state: &Arc<AppState>, user_id: String) -> Result<EnrollmentHandle> {
    let handle = device_enrollment::start_device_enrollment(state, user_id).await?;
    Ok(handle)
}

/// New device: poll the current status of an enrollment request. On
/// [`EnrollmentStatus::Approved`] the account key is in `state.unlock`; next `set_pin` →
/// [`finalize`].
pub async fn enrollment_status(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    let status = device_enrollment::poll_enrollment_status(state, request_id).await?;
    Ok(status)
}

/// New device: finish an enrollment/recovery. Publishes this device's
/// cross-signing cert and key packages and external-joins every group/DM. Call after `set_pin`.
pub async fn finalize(state: &Arc<AppState>, user_id: String) -> Result<()> {
    device_enrollment::finalize_device_enrollment(state, user_id).await?;
    Ok(())
}

/// New device: the SetPin tail shared by BOTH the approval and recovery paths.
/// The ORDER (set_pin → finalize → initialize_identity) is load-bearing.
pub async fn set_pin_and_finalize(
    state: &Arc<AppState>,
    user_id: &str,
    new_pin: &str,
) -> Result<()> {
    // No old PIN; `finalize` must follow before `initialize_identity`.
    pin::set_pin(state, None, new_pin.to_string()).await?;
    finalize(state, user_id.to_string()).await?;
    auth::initialize_identity(state, user_id.to_string()).await?;
    Ok(())
}

/// New device: Secret-Key recovery. Unwraps the server-stored `account_recovery`
/// blob with the Secret Key into `state.unlock`; the approval path's tail follows.
pub async fn recover(state: &Arc<AppState>, user_id: String, secret_key: String) -> Result<()> {
    device_enrollment::recover_with_secret_key(state, user_id, secret_key).await?;
    Ok(())
}

/// New device: last-resort soft recovery. Verifies `confirm_email`, rotates the
/// account identity (orphaning every other device) and returns the **new** Secret Key.
pub async fn reset_and_recover(
    state: &Arc<AppState>,
    user_id: String,
    confirm_email: String,
) -> Result<String> {
    let new_secret_key =
        device_enrollment::reset_identity_and_recover(state, user_id, confirm_email).await?;
    Ok(new_secret_key)
}

/// Existing device: list the open enrollment requests for this account (the
/// poll-fallback for a missed inbox push).
pub async fn pending_requests(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<Vec<PendingEnrollmentRequest>> {
    let pending = device_enrollment::list_pending_enrollment_requests(state, user_id).await?;
    Ok(pending)
}

/// Existing device: approve a pending request, confirming the `verification_code`
/// the new device displayed, then wraps the account key to it.
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
