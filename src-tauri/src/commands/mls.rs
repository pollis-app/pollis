// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::mls::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::mls::*;

#[tauri::command]
pub async fn generate_mls_key_package(state: State<'_, Arc<AppState>>, user_id: String) -> Result<serde_json::Value> {
    pollis_core::commands::mls::generate_mls_key_package(&state, user_id).await
}

#[tauri::command]
pub async fn publish_mls_key_package(state: State<'_, Arc<AppState>>, user_id: String, ref_hex: String, key_package_bytes: Vec<u8>) -> Result<()> {
    pollis_core::commands::mls::publish_mls_key_package(&state, user_id, ref_hex, key_package_bytes).await
}

#[tauri::command]
pub async fn fetch_mls_key_package(state: State<'_, Arc<AppState>>, target_user_id: String) -> Result<Option<Vec<u8>>> {
    pollis_core::commands::mls::fetch_mls_key_package(&state, target_user_id).await
}

#[tauri::command]
pub async fn create_mls_group(state: State<'_, Arc<AppState>>, conversation_id: String, creator_user_id: String) -> Result<()> {
    pollis_core::commands::mls::create_mls_group(&state, conversation_id, creator_user_id).await
}

#[tauri::command]
pub async fn process_welcome(state: State<'_, Arc<AppState>>, welcome_bytes: Vec<u8>) -> Result<()> {
    pollis_core::commands::mls::process_welcome(&state, welcome_bytes).await
}

#[tauri::command]
pub async fn poll_mls_welcomes(state: State<'_, Arc<AppState>>, user_id: String) -> Result<()> {
    pollis_core::commands::mls::poll_mls_welcomes(&state, user_id).await
}

#[tauri::command]
pub async fn reconcile_group_mls(state: State<'_, Arc<AppState>>, conversation_id: String, actor_user_id: String) -> crate::error::Result<()> {
    pollis_core::commands::mls::reconcile_group_mls(&state, conversation_id, actor_user_id).await
}

#[tauri::command]
pub async fn process_pending_commits(state: State<'_, Arc<AppState>>, conversation_id: String, user_id: String) -> crate::error::Result<()> {
    pollis_core::commands::mls::process_pending_commits(&state, conversation_id, user_id).await
}
