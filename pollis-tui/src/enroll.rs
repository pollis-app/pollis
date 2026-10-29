//! Order-enforcing wrappers over `pollis_core::commands::device_enrollment`
//! for adding a device to an existing account (spec §7, §11). The new device
//! starts empty (no history) but holds its own MLS leaf afterwards.
//!
//! ### A. Sibling-approval enrollment
//!
//! After `request_otp` → `verify_otp` (which mints the `enrollment_session`
//! that gates the request write):
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
//! No sibling online; after the same OTP proof:
//!
//! ```text
//! new device:  recover(user_id, secret_key)   (installs account key into AppState.unlock)
//! new device:  set_pin -> finalize(user_id) -> initialize_identity
//! ```
//!
//! `reset_and_recover` is the last resort (no sibling, no Secret Key).

use std::sync::Arc;

use anyhow::Result;
use pollis_core::commands::{auth, device_enrollment, pin};
use pollis_core::state::AppState;

pub use pollis_core::commands::device_enrollment::{
    EnrollmentHandle, EnrollmentStatus, PendingEnrollmentRequest, SecurityEvent,
};

/// New device: start a request. The handle carries the `request_id` to poll and
/// the 6-digit code to show. Requires a prior `verify_otp` (sets
/// `state.enrollment_session`).
pub async fn request_enrollment(state: &Arc<AppState>, user_id: String) -> Result<EnrollmentHandle> {
    let handle = device_enrollment::start_device_enrollment(state, user_id).await?;
    Ok(handle)
}

/// New device: on [`EnrollmentStatus::Approved`] the account key is already in
/// `state.unlock`; proceed to [`set_pin_and_finalize`].
pub async fn enrollment_status(
    state: &Arc<AppState>,
    request_id: String,
) -> Result<EnrollmentStatus> {
    let status = device_enrollment::poll_enrollment_status(state, request_id).await?;
    Ok(status)
}

/// New device: publish the cross-signing cert + key packages and external-join
/// every group/DM. Idempotent. Call after `set_pin` has opened the local DB.
pub async fn finalize(state: &Arc<AppState>, user_id: String) -> Result<()> {
    device_enrollment::finalize_device_enrollment(state, user_id).await?;
    Ok(())
}

/// New device: the tail shared by approval and recovery. The order
/// set_pin → finalize → initialize_identity is load-bearing (mirrors the rig's
/// `finish_enrollment`).
pub async fn set_pin_and_finalize(
    state: &Arc<AppState>,
    user_id: &str,
    new_pin: &str,
) -> Result<()> {
    // old_pin = None: keys come from the account key in `state.unlock`, not a
    // bootstrap session, so `finalize` must precede `initialize_identity`.
    pin::set_pin(state, None, new_pin.to_string()).await?;
    finalize(state, user_id.to_string()).await?;
    auth::initialize_identity(state, user_id.to_string()).await?;
    Ok(())
}

/// New device: unwrap the server-stored `account_recovery` blob with the Secret
/// Key into `state.unlock`; then [`set_pin_and_finalize`].
pub async fn recover(state: &Arc<AppState>, user_id: String, secret_key: String) -> Result<()> {
    device_enrollment::recover_with_secret_key(state, user_id, secret_key).await?;
    Ok(())
}

/// New device: rotates the account identity (orphaning every other device and
/// clearing group membership) and returns the new Secret Key. Destructive; the
/// UI must warn before calling.
pub async fn reset_and_recover(
    state: &Arc<AppState>,
    user_id: String,
    confirm_email: String,
) -> Result<String> {
    let new_secret_key =
        device_enrollment::reset_identity_and_recover(state, user_id, confirm_email).await?;
    Ok(new_secret_key)
}

/// Existing device: open requests (poll fallback for a missed inbox push).
pub async fn pending_requests(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<Vec<PendingEnrollmentRequest>> {
    let pending = device_enrollment::list_pending_enrollment_requests(state, user_id).await?;
    Ok(pending)
}

/// Existing device: approve, confirming the code the new device displayed.
/// Wraps the account key to the requester's ephemeral pub.
pub async fn approve(
    state: &Arc<AppState>,
    request_id: String,
    verification_code: String,
) -> Result<()> {
    device_enrollment::approve_device_enrollment(state, request_id, verification_code).await?;
    Ok(())
}

/// Existing device: the new device's poller then sees
/// [`EnrollmentStatus::Rejected`].
pub async fn reject(state: &Arc<AppState>, request_id: String) -> Result<()> {
    device_enrollment::reject_device_enrollment(state, request_id).await?;
    Ok(())
}
