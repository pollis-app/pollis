// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::pin::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::pin::*;

#[tauri::command]
pub async fn set_pin(state: State<'_, Arc<AppState>>, old_pin: Option<String>, new_pin: String) -> Result<()> {
    pollis_core::commands::pin::set_pin(&state, old_pin, new_pin).await
}

#[tauri::command]
pub async fn unlock(state: State<'_, Arc<AppState>>, user_id: String, pin: String) -> Result<UnlockOutcome> {
    pollis_core::commands::pin::unlock(&state, user_id, pin).await
}

#[tauri::command]
pub async fn lock(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::pin::lock(&state).await
}

#[tauri::command]
pub async fn get_unlock_state(state: State<'_, Arc<AppState>>) -> Result<UnlockStateSnapshot> {
    pollis_core::commands::pin::get_unlock_state(&state).await
}
