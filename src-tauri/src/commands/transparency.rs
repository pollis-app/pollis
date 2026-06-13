// Generated shim file. Each #[tauri::command] forwards to pollis_core::commands::transparency::*. Edit pollis-core, not here.

#![allow(unused_imports)]
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;
pub use pollis_core::commands::transparency::*;

#[tauri::command]
pub async fn self_audit_account_key(
    my_user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<SelfAuditReport> {
    pollis_core::commands::transparency::self_audit_account_key(&state, my_user_id).await
}

#[tauri::command]
pub async fn audit_peer_account_key(
    peer_user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<PeerAuditReport> {
    pollis_core::commands::transparency::audit_peer_account_key(&state, peer_user_id).await
}
