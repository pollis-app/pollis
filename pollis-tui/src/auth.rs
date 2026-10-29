//! Auth orchestration (M1).
//!
//! Thin wrappers over `pollis_core::commands::*`, in the order the flows harness uses.
//!
//! ## The critical order gotcha (spec §7)
//!
//! `set_pin` (first device) or `unlock` (returning) opens it, so the first-device order is:
//!
//! ```text
//! request_otp -> verify_otp -> set_pin(new, old=None) -> initialize_identity
//! ```
//!
//! `set_pin` calls `load_user_db_with_key`; skipping or reordering it breaks the session.

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
/// index, without opening the local DB; also sets `state.device_id`.
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
/// still **closed**; proceed to [`set_pin`].
pub async fn verify_otp(state: &Arc<AppState>, email: &str, code: &str) -> Result<UserProfile> {
    let profile = auth::verify_otp(state, email.to_string(), code.to_string()).await?;
    Ok(profile)
}

/// Steps 3+4 of signup: set the PIN (opens the SQLCipher DB) and then publish
/// the MLS key package; `initialize_identity` needs the DB open.
pub async fn set_pin_and_init(state: &Arc<AppState>, user_id: &str, new_pin: &str) -> Result<()> {
    // First-time PIN: initial keys come from the post-verify bootstrap session.
    pin::set_pin(state, None, new_pin.to_string()).await?;
    auth::initialize_identity(state, user_id.to_string()).await?;
    Ok(())
}

/// Returning-launch unlock: re-open the local SQLCipher DB under the PIN-derived
/// key.
pub async fn unlock(state: &Arc<AppState>, user_id: &str, pin_code: &str) -> Result<()> {
    pin::unlock(state, user_id.to_string(), pin_code.to_string()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// GUARD (#879 / #882) — the shipped `pollis` binary must keep the OS
    /// keychain backend compiled in.
    ///
    /// Without `os-keystore` the CLI would silently fall back to a machine-bound file.
    #[test]
    fn guard_cli_keeps_the_os_keychain_backend() {
        assert!(
            pollis_core::keystore::os_keychain_compiled(),
            "pollis-tui must depend on pollis-core with the `os-keystore` feature: \
             the shipped CLI has to prefer the OS keychain wherever one exists. \
             On a host without one it now falls back to the machine-bound \
             encrypted file at runtime (#882) — that fallback is not a reason to \
             drop the feature."
        );
    }
}
