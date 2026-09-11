// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::export::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::export::*;

#[tauri::command]
pub async fn export_archive(path: String, conversation_id: Option<String>, state: State<'_, Arc<AppState>>) -> Result<ExportSummary> {
    pollis_core::commands::export::export_archive(path, conversation_id, &state).await
}
