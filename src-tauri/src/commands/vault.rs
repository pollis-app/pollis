// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::vault::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::vault::*;

#[tauri::command]
pub async fn get_vault_messages(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<VaultMessage>> {
    pollis_core::commands::vault::get_vault_messages(user_id, &state).await
}

#[tauri::command]
pub async fn send_vault_message(user_id: String, content: String, state: State<'_, Arc<AppState>>) -> Result<VaultMessage> {
    pollis_core::commands::vault::send_vault_message(user_id, content, &state).await
}

#[tauri::command]
pub async fn edit_vault_message(id: String, new_content: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<VaultMessage> {
    pollis_core::commands::vault::edit_vault_message(id, new_content, user_id, &state).await
}

#[tauri::command]
pub async fn set_vault_message_pinned(id: String, pinned: bool, user_id: String, state: State<'_, Arc<AppState>>) -> Result<VaultMessage> {
    pollis_core::commands::vault::set_vault_message_pinned(id, pinned, user_id, &state).await
}

#[tauri::command]
pub async fn delete_vault_message(id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::vault::delete_vault_message(id, user_id, &state).await
}

#[tauri::command]
pub async fn search_vault_messages(query: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<VaultMessage>> {
    pollis_core::commands::vault::search_vault_messages(query, user_id, &state).await
}
