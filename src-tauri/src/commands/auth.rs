// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::auth::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::auth::*;

#[tauri::command]
pub async fn initialize_identity(state: State<'_, Arc<AppState>>, user_id: String) -> Result<IdentityInfo> {
    pollis_core::commands::auth::initialize_identity(&state, user_id).await
}

#[tauri::command]
pub async fn get_identity() -> Result<Option<IdentityInfo>> {
    pollis_core::commands::auth::get_identity().await
}

#[tauri::command]
pub async fn request_otp(state: State<'_, Arc<AppState>>, email: String) -> Result<()> {
    pollis_core::commands::auth::request_otp(&state, email).await
}

#[tauri::command]
pub async fn verify_otp(state: State<'_, Arc<AppState>>, email: String, code: String) -> Result<UserProfile> {
    pollis_core::commands::auth::verify_otp(&state, email, code).await
}

#[tauri::command]
pub async fn request_email_change_otp(state: State<'_, Arc<AppState>>, user_id: String, new_email: String) -> Result<()> {
    pollis_core::commands::auth::request_email_change_otp(&state, user_id, new_email).await
}

#[tauri::command]
pub async fn verify_email_change(state: State<'_, Arc<AppState>>, user_id: String, new_email: String, code: String) -> Result<()> {
    pollis_core::commands::auth::verify_email_change(&state, user_id, new_email, code).await
}

#[tauri::command]
pub async fn dev_login(state: State<'_, Arc<AppState>>, _email: String) -> Result<UserProfile> {
    pollis_core::commands::auth::dev_login(&state, _email).await
}

#[tauri::command]
pub async fn get_session(state: State<'_, Arc<AppState>>) -> Result<Option<UserProfile>> {
    pollis_core::commands::auth::get_session(&state).await
}

#[tauri::command]
pub async fn logout(state: State<'_, Arc<AppState>>, delete_data: bool) -> Result<()> {
    pollis_core::commands::auth::logout(&state, delete_data).await
}

#[tauri::command]
pub async fn delete_account(state: State<'_, Arc<AppState>>, user_id: String) -> Result<()> {
    pollis_core::commands::auth::delete_account(&state, user_id).await
}

#[tauri::command]
pub fn list_known_accounts() -> Result<crate::accounts::AccountsIndex> {
    pollis_core::commands::auth::list_known_accounts()
}

#[tauri::command]
pub async fn wipe_local_data(state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::auth::wipe_local_data(&state).await
}

#[tauri::command]
pub async fn list_user_devices(state: State<'_, Arc<AppState>>, user_id: String) -> Result<Vec<serde_json::Value>> {
    pollis_core::commands::auth::list_user_devices(&state, user_id).await
}

#[tauri::command]
pub async fn revoke_device(state: State<'_, Arc<AppState>>, user_id: String, device_id: String) -> Result<()> {
    pollis_core::commands::auth::revoke_device(&state, user_id, device_id).await
}
