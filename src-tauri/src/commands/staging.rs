// Shim layer for pollis_core::commands::staging. `stage_attachment` is
// hand-rolled to consume the IPC raw-byte body — the same shape
// `terminal_write` uses — because the whole point of the module is that these
// bytes never become a file. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::{Error, Result};
use crate::state::AppState;
pub use pollis_core::commands::staging::*;

/// Take custody of an attachment's bytes and hand back the opaque handle the
/// renderer passes to `upload_media_staged`.
///
/// The payload is the raw IPC body — the frontend invokes with the
/// `Uint8Array` itself — so a pasted file crosses the boundary as bytes rather
/// than as the `Array.from(uint8) -> JSON number-array -> serde Vec<u8>`
/// round trip, which for a multi-megabyte screenshot is the difference between
/// a paste and a stall.
#[tauri::command]
pub async fn stage_attachment(request: tauri::ipc::Request<'_>) -> Result<StagedAttachment> {
    let bytes: Vec<u8> = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        tauri::ipc::InvokeBody::Json(_) => {
            return Err(Error::Other(anyhow::anyhow!(
                "stage_attachment: expected raw body, got json — the frontend must invoke \
                 with a Uint8Array"
            )));
        }
    };
    pollis_core::commands::staging::stage_attachment(bytes)
}

/// Release one staged attachment. Called when the user removes an attachment
/// card, and when a composer unmounts still holding staged bytes.
#[tauri::command]
pub fn discard_staged_attachment(staged_id: String) -> bool {
    pollis_core::commands::staging::discard_staged(&staged_id)
}
