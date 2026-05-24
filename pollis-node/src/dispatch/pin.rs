// Phase 2: port of `src-tauri/src/commands/pin.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "set_pin" => Some(set_pin(args).await),
        "unlock" => Some(unlock(args).await),
        "lock" => Some(lock(args).await),
        "get_unlock_state" => Some(get_unlock_state(args).await),
        _ => None,
    }
}

async fn set_pin(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        old_pin: Option<String>,
        new_pin: String,
    }
    let Args { old_pin, new_pin } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::pin::set_pin(&state, old_pin, new_pin)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn unlock(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        pin: String,
    }
    let Args { user_id, pin } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::pin::unlock(&state, user_id, pin)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn lock(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    pollis_core::commands::pin::lock(&state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn get_unlock_state(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    let out = pollis_core::commands::pin::get_unlock_state(&state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}
