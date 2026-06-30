// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::mls::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::mls::*;

#[tauri::command]
pub async fn poll_mls_welcomes(state: State<'_, Arc<AppState>>, user_id: String) -> Result<()> {
    pollis_core::commands::mls::poll_mls_welcomes(&state, user_id).await
}

#[tauri::command]
pub async fn process_pending_commits(state: State<'_, Arc<AppState>>, conversation_id: String, user_id: String) -> crate::error::Result<()> {
    pollis_core::commands::mls::process_pending_commits(&state, conversation_id, user_id).await
}

#[tauri::command]
pub async fn catch_up_all_mls_groups(state: State<'_, Arc<AppState>>, user_id: String) -> crate::error::Result<()> {
    pollis_core::commands::mls::catch_up_all_mls_groups(&state, &user_id).await
}
