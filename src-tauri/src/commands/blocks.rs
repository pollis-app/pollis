// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::blocks::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::blocks::*;

#[tauri::command]
pub async fn block_user(blocker_id: String, blocked_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::blocks::block_user(blocker_id, blocked_id, &state).await
}

#[tauri::command]
pub async fn unblock_user(blocker_id: String, blocked_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::blocks::unblock_user(blocker_id, blocked_id, &state).await
}

#[tauri::command]
pub async fn list_blocked_users(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<BlockedUser>> {
    pollis_core::commands::blocks::list_blocked_users(user_id, &state).await
}
