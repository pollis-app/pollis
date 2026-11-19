// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::import::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::pathscope::PathScope;
use crate::state::AppState;
pub use pollis_core::commands::import::*;

// `path` is read and its images go to a public bucket, so it must be user-chosen (`crate::pathscope`).
#[tauri::command]
pub async fn import_preview(source: SourceKind, path: String, user_id: String, target: Option<String>, include_archived: Option<bool>, scope: State<'_, PathScope>, state: State<'_, Arc<AppState>>) -> Result<ImportPreview> {
    scope.require(std::path::Path::new(&path), "import_preview")?;
    pollis_core::commands::import::import_preview(source, path, user_id, target, include_archived, &state).await
}

#[tauri::command]
pub async fn import_apply(source: SourceKind, path: String, user_id: String, target: Option<String>, options: Option<ImportApplyOptions>, scope: State<'_, PathScope>, state: State<'_, Arc<AppState>>) -> Result<ImportResult> {
    scope.require(std::path::Path::new(&path), "import_apply")?;
    pollis_core::commands::import::import_apply(source, path, user_id, target, options, &state).await
}
