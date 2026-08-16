// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::emoji::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::emoji::*;

#[tauri::command]
pub async fn list_usable_emoji(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<CustomEmoji>> {
    pollis_core::commands::emoji::list_usable_emoji(user_id, &state).await
}

#[tauri::command]
pub async fn list_group_emoji(group_id: String, requester_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<CustomEmoji>> {
    pollis_core::commands::emoji::list_group_emoji(group_id, requester_id, &state).await
}

#[tauri::command]
pub async fn upload_group_emoji(group_id: String, shortcode: String, path: String, state: State<'_, Arc<AppState>>) -> Result<CustomEmoji> {
    pollis_core::commands::emoji::upload_group_emoji(group_id, shortcode, path, &state).await
}

#[tauri::command]
pub async fn remove_group_emoji(group_id: String, shortcode: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::emoji::remove_group_emoji(group_id, shortcode, &state).await
}

#[tauri::command]
pub async fn get_emoji_url(content_hash: String, state: State<'_, Arc<AppState>>) -> Result<String> {
    pollis_core::commands::emoji::get_emoji_url(content_hash, &state).await
}

#[tauri::command]
pub async fn prepare_emoji_text(user_id: String, text: String, state: State<'_, Arc<AppState>>) -> Result<String> {
    pollis_core::commands::emoji::prepare_emoji_text(user_id, text, &state).await
}
