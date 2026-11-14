// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::poster::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::poster::*;

#[tauri::command]
pub async fn get_local_video_poster(content_hash: String, state: State<'_, Arc<AppState>>) -> Result<Option<SavedVideoPoster>> {
    pollis_core::commands::poster::get_local_video_poster(content_hash, &state).await
}

#[tauri::command]
pub async fn save_local_video_poster(content_hash: String, frame: String, state: State<'_, Arc<AppState>>) -> Result<SavedVideoPoster> {
    pollis_core::commands::poster::save_local_video_poster(content_hash, frame, &state).await
}
