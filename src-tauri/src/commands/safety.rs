// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::safety::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::safety::*;

#[tauri::command]
pub async fn get_safety_number(my_user_id: String, peer_user_id: String, state: State<'_, Arc<AppState>>) -> Result<SafetyNumberInfo> {
    pollis_core::commands::safety::get_safety_number(my_user_id, peer_user_id, &state).await
}

#[tauri::command]
pub async fn set_contact_verified(peer_user_id: String, verified: bool, state: State<'_, Arc<AppState>>) -> Result<()> {
    pollis_core::commands::safety::set_contact_verified(peer_user_id, verified, &state).await
}

#[tauri::command]
pub async fn list_peer_verifications(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<pollis_core::commands::safety::PeerVerificationEntry>> {
    pollis_core::commands::safety::list_peer_verifications(&state).await
}
