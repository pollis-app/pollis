// Shim over pollis_core::commands::link_preview (#1987). Edit pollis-core, not here.

use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::link_preview::LinkPreviewDraft;

#[tauri::command]
pub async fn fetch_link_preview(url: String, state: State<'_, Arc<AppState>>) -> Result<Option<LinkPreviewDraft>> {
    pollis_core::commands::link_preview::fetch_link_preview(url, &state).await
}

#[tauri::command]
pub async fn get_link_previews_enabled(state: State<'_, Arc<AppState>>) -> Result<bool> {
    pollis_core::commands::link_preview::get_link_previews_enabled(&state).await
}

#[tauri::command]
pub async fn set_link_previews_enabled(enabled: bool, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::link_preview::set_link_previews_enabled(enabled, &state).await
}
