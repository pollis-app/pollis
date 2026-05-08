// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::user::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::user::*;

#[tauri::command]
pub async fn get_user_profile(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Option<UserProfile>> {
    pollis_core::commands::user::get_user_profile(user_id, &state).await
}

#[tauri::command]
pub async fn update_user_profile(user_id: String, username: Option<String>, preferred_name: Option<String>, phone: Option<String>, avatar_url: Option<String>, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::user::update_user_profile(user_id, username, preferred_name, phone, avatar_url, &state).await
}

#[tauri::command]
pub async fn search_user_by_username(username: String, state: State<'_, Arc<AppState>>) -> Result<Option<UserProfile>> {
    pollis_core::commands::user::search_user_by_username(username, &state).await
}

#[tauri::command]
pub async fn get_preferences(user_id: String, state: State<'_, Arc<AppState>>) -> Result<String> {
    pollis_core::commands::user::get_preferences(user_id, &state).await
}

#[tauri::command]
pub async fn save_preferences(user_id: String, preferences_json: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::user::save_preferences(user_id, preferences_json, &state).await
}
