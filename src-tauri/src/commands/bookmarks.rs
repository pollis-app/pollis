// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::bookmarks::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::bookmarks::*;

#[tauri::command]
pub async fn save_message(message_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::bookmarks::save_message(message_id, &state).await
}

#[tauri::command]
pub async fn unsave_message(message_id: String, state: State<'_, Arc<AppState>>) -> Result<bool> {
    pollis_core::commands::bookmarks::unsave_message(message_id, &state).await
}

#[tauri::command]
pub async fn toggle_saved_message(message_id: String, state: State<'_, Arc<AppState>>) -> Result<bool> {
    pollis_core::commands::bookmarks::toggle_saved_message(message_id, &state).await
}

#[tauri::command]
pub async fn list_saved_messages(state: State<'_, Arc<AppState>>) -> Result<Vec<SavedMessage>> {
    pollis_core::commands::bookmarks::list_saved_messages(&state).await
}

#[tauri::command]
pub async fn resolve_message_permalink(conversation_id: String, message_id: String, state: State<'_, Arc<AppState>>) -> Result<PermalinkTarget> {
    pollis_core::commands::bookmarks::resolve_message_permalink(conversation_id, message_id, &state).await
}
