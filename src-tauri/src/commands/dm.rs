// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::dm::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::dm::*;

#[tauri::command]
pub async fn create_dm_channel(creator_id: String, member_ids: Vec<String>, state: State<'_, Arc<AppState>>) -> Result<DmChannel> {
    pollis_core::commands::dm::create_dm_channel(creator_id, member_ids, &state).await
}

#[tauri::command]
pub async fn list_dm_channels(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<DmChannel>> {
    pollis_core::commands::dm::list_dm_channels(user_id, &state).await
}

#[tauri::command]
pub async fn list_dm_requests(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<DmChannel>> {
    pollis_core::commands::dm::list_dm_requests(user_id, &state).await
}

#[tauri::command]
pub async fn accept_dm_request(dm_channel_id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::dm::accept_dm_request(dm_channel_id, user_id, &state).await
}

#[tauri::command]
pub async fn get_dm_channel(dm_channel_id: String, state: State<'_, Arc<AppState>>) -> Result<DmChannel> {
    pollis_core::commands::dm::get_dm_channel(dm_channel_id, &state).await
}

#[tauri::command]
pub async fn add_user_to_dm_channel(dm_channel_id: String, user_id: String, added_by: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::dm::add_user_to_dm_channel(dm_channel_id, user_id, added_by, &state).await
}

#[tauri::command]
pub async fn remove_user_from_dm_channel(dm_channel_id: String, user_id: String, requester_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::dm::remove_user_from_dm_channel(dm_channel_id, user_id, requester_id, &state).await
}

#[tauri::command]
pub async fn leave_dm_channel(dm_channel_id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::dm::leave_dm_channel(dm_channel_id, user_id, &state).await
}
