// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::overlay::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::overlay::*;

#[tauri::command]
pub async fn get_overlay_mode(state: State<'_, Arc<AppState>>) -> Result<String> {
    pollis_core::commands::overlay::get_overlay_mode(&state).await
}

#[tauri::command]
pub async fn set_overlay_mode(mode: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::overlay::set_overlay_mode(&state, mode).await
}
