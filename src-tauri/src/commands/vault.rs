// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::vault::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::vault::*;

#[tauri::command]
pub async fn get_vault_messages(user_id: String, state: State<'_, Arc<AppState>>) -> Result<Vec<VaultMessage>> {
    pollis_core::commands::vault::get_vault_messages(user_id, &state).await
}

/// Durable (#2100): `message_id` makes a repeat save once; `attachments` go through the queue.
#[tauri::command]
pub async fn send_vault_message(
    user_id: String,
    content: String,
    message_id: Option<String>,
    attachments: Option<Vec<pollis_core::commands::messages::OutgoingAttachment>>,
    scope: State<'_, crate::pathscope::PathScope>,
    state: State<'_, Arc<AppState>>,
) -> Result<VaultMessage> {
    // The queue reads the files itself, so gate them like `send_message` does.
    let attachments = attachments.unwrap_or_default();
    for att in &attachments {
        if let Some(path) = &att.path {
            scope.require(std::path::Path::new(path), "send_vault_message")?;
        }
    }
    pollis_core::commands::vault::send_vault_message_with(user_id, message_id, content, attachments, &state).await
}

#[tauri::command]
pub async fn retry_vault_message(id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<VaultMessage> {
    pollis_core::commands::messages::vault_queue::retry_vault_message(id, user_id, &state).await
}

#[tauri::command]
pub async fn discard_unsent_vault_message(id: String, state: State<'_, Arc<AppState>>) -> Result<bool> {
    pollis_core::commands::messages::vault_queue::discard_unsent_vault_message(id, &state).await
}

#[tauri::command]
pub async fn edit_vault_message(id: String, new_content: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<VaultMessage> {
    pollis_core::commands::vault::edit_vault_message(id, new_content, user_id, &state).await
}

#[tauri::command]
pub async fn set_vault_message_pinned(id: String, pinned: bool, user_id: String, state: State<'_, Arc<AppState>>) -> Result<VaultMessage> {
    pollis_core::commands::vault::set_vault_message_pinned(id, pinned, user_id, &state).await
}

#[tauri::command]
pub async fn delete_vault_message(id: String, user_id: String, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::vault::delete_vault_message(id, user_id, &state).await
}
