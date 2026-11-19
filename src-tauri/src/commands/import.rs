// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::import::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::pathscope::PathScope;
use crate::state::AppState;
pub use pollis_core::commands::import::*;

// `dir` is read and its images go to a public bucket, so it must be user-chosen (`crate::pathscope`).
#[tauri::command]
pub async fn discord_import_preview(dir: String, user_id: String, target: Option<String>, scope: State<'_, PathScope>, state: State<'_, Arc<AppState>>) -> Result<DiscordImportPreview> {
    scope.require(std::path::Path::new(&dir), "discord_import_preview")?;
    pollis_core::commands::import::discord_import_preview(dir, user_id, target, &state).await
}

#[tauri::command]
pub async fn discord_import_apply(dir: String, user_id: String, target: Option<String>, scope: State<'_, PathScope>, state: State<'_, Arc<AppState>>) -> Result<DiscordImportResult> {
    scope.require(std::path::Path::new(&dir), "discord_import_apply")?;
    pollis_core::commands::import::discord_import_apply(dir, user_id, target, &state).await
}
