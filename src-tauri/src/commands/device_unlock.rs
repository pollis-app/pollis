// Shims for pollis_core::commands::device_unlock (#1685), registered ONLY in the integration-test
// harness. Desktop has a parity exemption: the `keyring` apple-native backend cannot attach
// biometric access control to an item, and Linux/Windows have no equivalent, so the desktop app
// never offers biometric unlock and `lib.rs` does not register these. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
use pollis_core::commands::pin::UnlockOutcome;

#[tauri::command]
pub async fn device_unlock_begin(state: State<'_, Arc<AppState>>, pin: Option<String>) -> Result<String> {
    pollis_core::commands::device_unlock::device_unlock_begin(&state, pin).await
}

#[tauri::command]
pub async fn device_unlock_confirm(state: State<'_, Arc<AppState>>, device_key: String) -> Result<()> {
    pollis_core::commands::device_unlock::device_unlock_confirm(&state, device_key).await
}

#[tauri::command]
pub async fn unlock_with_device_key(
    state: State<'_, Arc<AppState>>,
    user_id: String,
    device_key: String,
) -> Result<UnlockOutcome> {
    pollis_core::commands::device_unlock::unlock_with_device_key(&state, user_id, device_key).await
}

#[tauri::command]
pub async fn disable_device_unlock(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::device_unlock::disable_device_unlock(&state).await
}
