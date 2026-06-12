// Dispatch arms for the account-key transparency client commands (#330). Each
// arm mirrors the safety module's pattern: deserialize args, grab the global
// AppState, forward into `pollis_core::commands::transparency`, serialize back.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "self_audit_account_key" => Some(self_audit_account_key(args).await),
        "audit_peer_account_key" => Some(audit_peer_account_key(args).await),
        _ => None,
    }
}

async fn self_audit_account_key(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        my_user_id: String,
    }
    let Args { my_user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::transparency::self_audit_account_key(&state, my_user_id)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn audit_peer_account_key(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        peer_user_id: String,
    }
    let Args { peer_user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::transparency::audit_peer_account_key(&state, peer_user_id)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}
