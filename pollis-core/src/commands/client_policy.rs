//! The minimum-version check as commands (#1611): desktop's Tauri shims and mobile's bridge both
//! call these, so the rule, the cache and the throttle live once, in [`crate::client_policy`].

use std::sync::Arc;

use crate::client_policy::{self, ClientPolicyStatus, PolicyOverride};
use crate::error::Result;
use crate::state::AppState;

/// Fetch the policy and decide. `force` skips the re-check throttle (launch, the Retry button);
/// window focus and app resume pass `false`.
pub async fn check_client_policy(state: &Arc<AppState>, force: bool) -> Result<ClientPolicyStatus> {
    Ok(client_policy::check_state(state, force).await)
}

/// The last verdict, with no network: what a 426 or the launch check decided.
pub async fn get_client_policy_status(state: &Arc<AppState>) -> Result<ClientPolicyStatus> {
    Ok(client_policy::current(state))
}

/// Dev builds only: use `url` or the inline `policy` JSON instead of the published file, until
/// cleared (both `None`). Refused for a build that talks to the prod DS.
pub async fn set_client_policy_override(
    state: &Arc<AppState>,
    url: Option<String>,
    policy: Option<String>,
) -> Result<()> {
    let value = (url.is_some() || policy.is_some()).then_some(PolicyOverride { url, policy });
    client_policy::set_override(&state.config, &client_policy::data_dir(), value.as_ref())
}
