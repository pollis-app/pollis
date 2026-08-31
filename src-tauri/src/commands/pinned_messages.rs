// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::pinned_messages::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::pinned_messages::*;

#[tauri::command]
pub async fn pin_message(conversation_id: String, message_id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::pinned_messages::pin_message(conversation_id, message_id, user_id, &state).await
}

#[tauri::command]
pub async fn unpin_message(conversation_id: String, message_id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::pinned_messages::unpin_message(conversation_id, message_id, user_id, &state).await
}

#[tauri::command]
pub async fn list_pinned_messages(conversation_id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<PinnedMessage>> {
    pollis_core::commands::pinned_messages::list_pinned_messages(conversation_id, user_id, &state).await
}
