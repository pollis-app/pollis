// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::client_policy::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
use pollis_core::client_policy::ClientPolicyStatus;

/// The minimum-version check (#1611); `force` skips the re-check throttle.
#[tauri::command]
pub async fn check_client_policy(state: State<'_, Arc<AppState>>, force: Option<bool>) -> Result<ClientPolicyStatus> {
    pollis_core::commands::client_policy::check_client_policy(&state, force.unwrap_or(false)).await
}

#[tauri::command]
pub async fn get_client_policy_status(state: State<'_, Arc<AppState>>) -> Result<ClientPolicyStatus> {
    pollis_core::commands::client_policy::get_client_policy_status(&state).await
}

/// The cached verdict, no network: launch waits for the check only when this is not "allowed".
#[tauri::command]
pub async fn peek_client_policy(state: State<'_, Arc<AppState>>) -> Result<ClientPolicyStatus> {
    pollis_core::commands::client_policy::peek_client_policy(&state).await
}
