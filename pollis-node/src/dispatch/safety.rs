// Phase 2: port of `src-tauri/src/commands/safety.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "get_safety_number" => Some(get_safety_number(args).await),
        "set_contact_verified" => Some(set_contact_verified(args).await),
        "list_peer_verifications" => Some(list_peer_verifications(args).await),
        _ => None,
    }
}

async fn get_safety_number(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        my_user_id: String,
        peer_user_id: String,
    }
    let Args {
        my_user_id,
        peer_user_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out =
        pollis_core::commands::safety::get_safety_number(my_user_id, peer_user_id, &state)
            .await
            .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn set_contact_verified(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        peer_user_id: String,
        verified: bool,
    }
    let Args {
        peer_user_id,
        verified,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::safety::set_contact_verified(peer_user_id, verified, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn list_peer_verifications(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    let out = pollis_core::commands::safety::list_peer_verifications(&state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}
