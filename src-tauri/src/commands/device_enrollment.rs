// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::device_enrollment::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::device_enrollment::*;

#[tauri::command]
pub async fn start_device_enrollment(state: State<'_, Arc<AppState>>, user_id: String) -> Result<EnrollmentHandle> {
    pollis_core::commands::device_enrollment::start_device_enrollment(&state, user_id).await
}

#[tauri::command]
pub async fn poll_enrollment_status(state: State<'_, Arc<AppState>>, request_id: String) -> Result<EnrollmentStatus> {
    pollis_core::commands::device_enrollment::poll_enrollment_status(&state, request_id).await
}

#[tauri::command]
pub async fn list_pending_enrollment_requests(state: State<'_, Arc<AppState>>, user_id: String) -> Result<Vec<PendingEnrollmentRequest>> {
    pollis_core::commands::device_enrollment::list_pending_enrollment_requests(&state, user_id).await
}

#[tauri::command]
pub async fn approve_device_enrollment(state: State<'_, Arc<AppState>>, request_id: String, verification_code: String) -> Result<()> {
    pollis_core::commands::device_enrollment::approve_device_enrollment(&state, request_id, verification_code).await
}

#[tauri::command]
pub async fn reject_device_enrollment(state: State<'_, Arc<AppState>>, request_id: String) -> Result<()> {
    pollis_core::commands::device_enrollment::reject_device_enrollment(&state, request_id).await
}

#[tauri::command]
pub async fn recover_with_secret_key(state: State<'_, Arc<AppState>>, user_id: String, secret_key: String) -> Result<()> {
    pollis_core::commands::device_enrollment::recover_with_secret_key(&state, user_id, secret_key).await
}

#[tauri::command]
pub async fn reset_identity_and_recover(state: State<'_, Arc<AppState>>, user_id: String, confirm_email: String) -> Result<String> {
    pollis_core::commands::device_enrollment::reset_identity_and_recover(&state, user_id, confirm_email).await
}

#[tauri::command]
pub async fn finalize_device_enrollment(state: State<'_, Arc<AppState>>, user_id: String) -> Result<()> {
    pollis_core::commands::device_enrollment::finalize_device_enrollment(&state, user_id).await
}

#[tauri::command]
pub async fn list_security_events(state: State<'_, Arc<AppState>>, user_id: String, limit: Option<i64>) -> Result<Vec<SecurityEvent>> {
    pollis_core::commands::device_enrollment::list_security_events(&state, user_id, limit).await
}
