// Shim layer for pollis_core::commands::staging. Edit pollis-core, not here.
// `stage_attachment` takes the raw IPC body so the bytes never become a file.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::{Error, Result};
use crate::state::AppState;
pub use pollis_core::commands::staging::*;

/// Take custody of an attachment's bytes (the raw IPC `Uint8Array` body) and
/// return the handle the renderer passes to `upload_media_staged`.
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

/// Release one staged attachment (card removed, or composer unmounted).
#[tauri::command]
pub fn discard_staged_attachment(staged_id: String) -> bool {
    pollis_core::commands::staging::discard_staged(&staged_id)
}
