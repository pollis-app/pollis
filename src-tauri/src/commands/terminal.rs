// Shim layer. Each #[tauri::command] forwards to pollis_core::commands::terminal::*.
// `terminal_write` is hand-rolled to consume the IPC raw-byte body symmetric
// with the output Channel; the rest are straight pass-throughs — edit
// pollis-core, not here.

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

#[tauri::command]
pub async fn terminal_ack(terminal_id: String, bytes: usize, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::terminal::terminal_ack(terminal_id, bytes, &state).await
}

/// Forward a keystroke chunk to the PTY using a binary IPC body — symmetric
/// with the output Channel that already pushes raw bytes via
/// `InvokeResponseBody::Raw` (issue #282). The frontend hands us the
/// `Uint8Array` from `TextEncoder.encode(data)` as the request body, and
/// the terminal id rides in the `x-terminal-id` header. This avoids the
/// `Array.from(uint8) -> JSON number-array -> serde Vec<u8>` roundtrip
/// previously paid on *every* keypress — the latency that made the
/// terminal feel laggy on WebKitGTK/X11.
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
