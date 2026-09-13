// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::export::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::pathscope::PathScope;
use crate::state::AppState;
pub use pollis_core::commands::export::*;

// `path` is where a plaintext archive of the account's whole message history
// gets written, so it has to be a location the user picked in the save panel —
// see `crate::pathscope`.
#[tauri::command]
pub async fn export_archive(path: String, conversation_id: Option<String>, scope: State<'_, PathScope>, state: State<'_, Arc<AppState>>) -> Result<ExportSummary> {
    scope.require(std::path::Path::new(&path), "export_archive")?;
    pollis_core::commands::export::export_archive(path, conversation_id, &state).await
}

// `files_dir` is the archive's sidecar directory; it must sit under the folder
// the user chose for the archive itself.
#[tauri::command]
pub async fn fetch_export_attachments(files_dir: String, attachments: Vec<MissingAttachment>, scope: State<'_, PathScope>, state: State<'_, Arc<AppState>>) -> Result<pollis_core::commands::export_fetch::FetchSummary> {
    scope.require(std::path::Path::new(&files_dir), "fetch_export_attachments")?;
    pollis_core::commands::export_fetch::fetch_export_attachments(files_dir, attachments, &state).await
}
