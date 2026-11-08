//! Generated-style shim. Forwards Tauri commands into
//! pollis_core::commands::history_transfer (#1735). Edit pollis-core, not here.

use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::history_transfer::*;

#[tauri::command]
pub async fn list_history_transfer_sources(state: State<'_, Arc<AppState>>) -> Result<Vec<TransferSource>> {
    pollis_core::commands::history_transfer::list_history_transfer_sources(&state).await
}

#[tauri::command]
pub async fn request_history_transfer(
    state: State<'_, Arc<AppState>>,
    source_device_id: String,
    via: String,
) -> Result<HistoryTransfer> {
    pollis_core::commands::history_transfer::request_history_transfer(&state, source_device_id, via).await
}

#[tauri::command]
pub async fn list_pending_history_transfer_requests(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<PendingTransferRequest>> {
    pollis_core::commands::history_transfer::list_pending_history_transfer_requests(&state).await
}

#[tauri::command]
pub async fn approve_history_transfer(
    state: State<'_, Arc<AppState>>,
    transfer_id: String,
    pin: Option<String>,
) -> Result<HistoryTransfer> {
    pollis_core::commands::history_transfer::approve_history_transfer(&state, transfer_id, pin).await
}

#[tauri::command]
pub async fn decline_history_transfer(state: State<'_, Arc<AppState>>, transfer_id: String) -> Result<()> {
    pollis_core::commands::history_transfer::decline_history_transfer(&state, transfer_id).await
}

#[tauri::command]
pub async fn list_history_transfers(state: State<'_, Arc<AppState>>) -> Result<Vec<HistoryTransfer>> {
    pollis_core::commands::history_transfer::list_history_transfers(&state).await
}

#[tauri::command]
pub async fn run_history_transfer(
    state: State<'_, Arc<AppState>>,
    transfer_id: String,
    on_progress: Channel<HistoryTransfer>,
) -> Result<HistoryTransfer> {
    pollis_core::commands::history_transfer::run_history_transfer(
        &state,
        transfer_id,
        Arc::new(crate::sink::ChannelSink(on_progress)),
    )
    .await
}

#[tauri::command]
pub async fn await_history_transfer(
    state: State<'_, Arc<AppState>>,
    transfer_id: String,
    since: Option<String>,
) -> Result<HistoryTransfer> {
    pollis_core::commands::history_transfer::await_history_transfer(&state, transfer_id, since).await
}

#[tauri::command]
pub async fn cancel_history_transfer(state: State<'_, Arc<AppState>>, transfer_id: String) -> Result<()> {
    pollis_core::commands::history_transfer::cancel_history_transfer(&state, transfer_id).await
}
