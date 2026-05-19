// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::terminal::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::terminal::*;

#[tauri::command]
pub async fn terminal_open(rows: u16, cols: u16, on_output: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>, state: State<'_, Arc<AppState>>) -> Result<String> {
    pollis_core::commands::terminal::terminal_open(rows, cols, std::sync::Arc::new(crate::sink::RawChannelSink(on_output)), &state).await
}

#[tauri::command]
pub async fn terminal_ack(terminal_id: String, bytes: usize, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::terminal::terminal_ack(terminal_id, bytes, &state).await
}

#[tauri::command]
pub async fn terminal_write(terminal_id: String, data: Vec<u8>, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::terminal::terminal_write(terminal_id, data, &state).await
}

#[tauri::command]
pub async fn terminal_resize(terminal_id: String, rows: u16, cols: u16, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::terminal::terminal_resize(terminal_id, rows, cols, &state).await
}

#[tauri::command]
pub async fn terminal_close(terminal_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::terminal::terminal_close(terminal_id, &state).await
}
