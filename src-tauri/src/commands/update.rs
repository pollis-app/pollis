// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::update::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::update::*;

#[tauri::command]
pub async fn mark_update_required(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::update::mark_update_required(&state).await
}

#[tauri::command]
pub async fn is_update_required(state: State<'_, Arc<AppState>>) -> Result<bool> {
    pollis_core::commands::update::is_update_required(&state).await
}
