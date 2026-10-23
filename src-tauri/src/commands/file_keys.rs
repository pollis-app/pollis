// Shim for the test-harness-only probe of the attachment key ring (#1382). Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;

#[cfg(feature = "test-harness")]
#[tauri::command]
pub async fn test_file_key_version(conversation_id: String, state: State<'_, Arc<AppState>>) -> Result<Option<i64>> {
    pollis_core::commands::file_keys::test_file_key_version(conversation_id, &state).await
}
