// Shim file. Each #[tauri::command] forwards to pollis_core::commands::device_link::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::device_link::*;
use pollis_core::commands::auth::UserProfile;

#[tauri::command]
pub async fn create_device_link(state: State<'_, Arc<AppState>>, user_id: String, pin: String) -> Result<DeviceLinkHandle> {
    pollis_core::commands::device_link::create_device_link(&state, user_id, pin).await
}

#[tauri::command]
pub async fn poll_device_link(state: State<'_, Arc<AppState>>, user_id: String, link_id: String) -> Result<DeviceLinkStatus> {
    pollis_core::commands::device_link::poll_device_link(&state, user_id, link_id).await
}

#[tauri::command]
pub async fn await_device_link(state: State<'_, Arc<AppState>>, user_id: String, link_id: String, since: String) -> Result<DeviceLinkStatus> {
    pollis_core::commands::device_link::await_device_link(&state, user_id, link_id, since).await
}

#[tauri::command]
pub async fn approve_device_link(state: State<'_, Arc<AppState>>, user_id: String, link_id: String) -> Result<()> {
    pollis_core::commands::device_link::approve_device_link(&state, user_id, link_id).await
}

#[tauri::command]
pub async fn cancel_device_link(state: State<'_, Arc<AppState>>, link_id: String) -> Result<()> {
    pollis_core::commands::device_link::cancel_device_link(&state, link_id).await
}

#[tauri::command]
pub async fn claim_device_link(state: State<'_, Arc<AppState>>, payload: String, device_name: Option<String>) -> Result<UserProfile> {
    pollis_core::commands::device_link::claim_device_link(&state, payload, device_name).await
}
