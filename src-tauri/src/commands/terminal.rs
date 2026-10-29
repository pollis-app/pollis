// Shim layer: forwards to pollis_core::commands::terminal::*. Edit pollis-core,
// not here. `terminal_write` takes the raw IPC body.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::{Error, Result};
use crate::state::AppState;
pub use pollis_core::commands::terminal::*;

#[tauri::command]
pub async fn terminal_open(rows: u16, cols: u16, on_output: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>, state: State<'_, Arc<AppState>>) -> Result<String> {
    pollis_core::commands::terminal::terminal_open(rows, cols, std::sync::Arc::new(crate::sink::RawChannelSink(on_output)), &state).await
}

/// Whether the in-app terminal is switched on for this device.
#[tauri::command]
pub async fn get_terminal_enabled() -> Result<bool> {
    pollis_core::commands::terminal_gate::get_terminal_enabled().await
}

/// Flip the device-local terminal switch. `terminal_open` reads the persisted
/// value, not an IPC argument, so the renderer cannot bypass it.
#[tauri::command]
pub async fn set_terminal_enabled(enabled: bool) -> Result<()> {
    pollis_core::commands::terminal_gate::set_terminal_enabled(enabled).await
}

#[tauri::command]
pub async fn terminal_ack(terminal_id: String, bytes: usize, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::terminal::terminal_ack(terminal_id, bytes, &state).await
}

/// Forward a keystroke chunk as a raw binary IPC body (#282), the terminal id
/// in the `x-terminal-id` header. A JSON number array per keypress made the
/// terminal lag on WebKitGTK.
#[tauri::command]
pub async fn terminal_write(
    request: tauri::ipc::Request<'_>,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let terminal_id = request
        .headers()
        .get("x-terminal-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "terminal_write: missing or invalid x-terminal-id header"
            ))
        })?;
    let data: &[u8] = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes,
        tauri::ipc::InvokeBody::Json(_) => {
            return Err(Error::Other(anyhow::anyhow!(
                "terminal_write: expected raw body, got json — frontend must invoke with a Uint8Array"
            )));
        }
    };
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
