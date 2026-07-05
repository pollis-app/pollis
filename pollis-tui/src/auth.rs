//! Auth orchestration (M1).
//!
//! These are thin, order-enforcing wrappers over `pollis_core::commands::*`. We
//! do **not** fork any command logic — every call goes to the exact same core
//! surface the desktop app reaches over Tauri `invoke`, in the exact same order
//! the flows harness (`TestClient::sign_up`) uses.
//!
//! ## The critical order gotcha (spec §7)
//!
//! `verify_otp` deliberately leaves the local SQLCipher DB **closed**. Until
//! `set_pin` (first device) or `unlock` (returning) opens it, every DB-touching
//! command fails with `"Not signed in"`. So the first-device order is:
//!
//! ```text
//! request_otp -> verify_otp -> set_pin(new, old=None) -> initialize_identity
//! ```
//!
//! `set_pin` is what calls `load_user_db_with_key`; skipping it (or reordering it
//! after `initialize_identity`) breaks the whole session.

use std::sync::Arc;

use anyhow::Result;
use pollis_core::commands::auth::UserProfile;
use pollis_core::commands::{auth, pin};
use pollis_core::state::AppState;

/// What the boot-time session probe found.
pub enum Boot {
    /// A returning device with a persisted account — needs PIN unlock.
    Returning(UserProfile),
    /// No local account — first-device signup.
    Fresh,
}

/// Returning-launch probe: rehydrate the profile from the keystore/accounts
/// index. Does **not** open the local DB — that happens in [`unlock`]. Also
/// (re)registers the device, which sets `state.device_id` for the subsequent
/// unlock/MLS calls.
pub async fn boot(state: &Arc<AppState>) -> Result<Boot> {
    match auth::get_session(state).await? {
        Some(profile) => Ok(Boot::Returning(profile)),
        None => Ok(Boot::Fresh),
    }
}

/// Step 1 of signup: ask the Delivery Service to send an OTP to `email`.
pub async fn request_otp(state: &Arc<AppState>, email: &str) -> Result<()> {
    auth::request_otp(state, email.to_string()).await?;
    Ok(())
}

/// Step 2 of signup: verify the OTP. Returns the profile, but the local DB is
/// still **closed** afterwards — the caller must proceed to [`set_pin`].
pub async fn verify_otp(state: &Arc<AppState>, email: &str, code: &str) -> Result<UserProfile> {
    let profile = auth::verify_otp(state, email.to_string(), code.to_string()).await?;
    Ok(profile)
}

/// Steps 3+4 of signup: set the PIN (opens the SQLCipher DB) and then publish
/// the MLS key package. Order matters — `initialize_identity` touches the DB, so
/// `set_pin` must land first.
pub async fn set_pin_and_init(state: &Arc<AppState>, user_id: &str, new_pin: &str) -> Result<()> {
    // old_pin = None: first-time PIN, sourcing the initial keys from the
    // post-verify bootstrap session.
    pin::set_pin(state, None, new_pin.to_string()).await?;
    auth::initialize_identity(state, user_id.to_string()).await?;
    Ok(())
}

/// Returning-launch unlock: re-open the local SQLCipher DB under the PIN-derived
/// key. After this the normal sync loop (M2) can run.
pub async fn unlock(state: &Arc<AppState>, user_id: &str, pin_code: &str) -> Result<()> {
    pin::unlock(state, user_id.to_string(), pin_code.to_string()).await?;
    Ok(())
}
