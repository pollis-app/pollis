// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::r2::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::r2::*;

#[tauri::command]
pub async fn upload_file(key: String, data: Vec<u8>, content_type: String, state: State<'_, Arc<AppState>>) -> Result<UploadResult> {
    pollis_core::commands::r2::upload_file(key, data, content_type, &state).await
}

#[tauri::command]
pub async fn upload_media(path: String, filename: String, content_type: String, state: State<'_, Arc<AppState>>) -> Result<MediaUploadResult> {
    pollis_core::commands::r2::upload_media(path, filename, content_type, &state).await
}

#[tauri::command]
pub async fn download_file(key: String, state: State<'_, Arc<AppState>>) -> Result<Vec<u8>> {
    pollis_core::commands::r2::download_file(key, &state).await
}

#[tauri::command]
pub async fn download_media(r2_key: String, content_hash: String, state: State<'_, Arc<AppState>>) -> Result<Vec<u8>> {
    pollis_core::commands::r2::download_media(r2_key, content_hash, &state).await
}

#[tauri::command]
pub async fn get_media_path(r2_key: String, content_hash: String, content_type: String, state: State<'_, Arc<AppState>>) -> Result<String> {
    pollis_core::commands::r2::get_media_path(r2_key, content_hash, content_type, &state).await
}
