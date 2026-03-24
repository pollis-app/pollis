use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;

#[tauri::command]
pub async fn mark_update_required(state: State<'_, Arc<AppState>>) -> Result<()> {
    state.update_required.store(true, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn is_update_required(state: State<'_, Arc<AppState>>) -> Result<bool> {
    Ok(state.update_required.load(Ordering::Relaxed))
}
