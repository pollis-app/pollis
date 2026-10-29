//! Order-enforcing auth wrappers over `pollis_core::commands::*`, in the same
//! order as the flows harness (`TestClient::sign_up`).
//!
//! `verify_otp` leaves the local DB closed; until `set_pin` or `unlock` opens
//! it, every DB-touching command fails with `"Not signed in"`
//! (`.codesight/wiki/pollis-tui.md`, spec §7). First-device order:
//!
//! ```text
//! request_otp -> verify_otp -> set_pin(new, old=None) -> initialize_identity
//! ```

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

/// Returning-launch probe. Does not open the local DB (that is [`unlock`]);
/// (re)registers the device, which sets `state.device_id` for later calls.
pub async fn boot(state: &Arc<AppState>) -> Result<Boot> {
    match auth::get_session(state).await? {
        Some(profile) => Ok(Boot::Returning(profile)),
        None => Ok(Boot::Fresh),
    }
}

pub async fn request_otp(state: &Arc<AppState>, email: &str) -> Result<()> {
    auth::request_otp(state, email.to_string()).await?;
    Ok(())
}

/// The local DB is still closed afterwards; the caller must proceed to
/// [`set_pin_and_init`].
pub async fn verify_otp(state: &Arc<AppState>, email: &str, code: &str) -> Result<UserProfile> {
    let profile = auth::verify_otp(state, email.to_string(), code.to_string()).await?;
    Ok(profile)
}

/// `set_pin` opens the DB, so it must land before `initialize_identity`.
pub async fn set_pin_and_init(state: &Arc<AppState>, user_id: &str, new_pin: &str) -> Result<()> {
    // old_pin = None: initial keys come from the post-verify bootstrap session.
    pin::set_pin(state, None, new_pin.to_string()).await?;
    auth::initialize_identity(state, user_id.to_string()).await?;
    Ok(())
}

/// Re-opens the local DB under the PIN-derived key.
pub async fn unlock(state: &Arc<AppState>, user_id: &str, pin_code: &str) -> Result<()> {
    pin::unlock(state, user_id.to_string(), pin_code.to_string()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The shipped CLI must keep the OS keychain backend compiled in; without
    /// `os-keystore` it would silently fall back to the weaker machine-bound
    /// file (#879, #882).
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
