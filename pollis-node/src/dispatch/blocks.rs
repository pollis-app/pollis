// Phase 2: port of `src-tauri/src/commands/blocks.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "block_user" => Some(block_user(args).await),
        "unblock_user" => Some(unblock_user(args).await),
        "list_blocked_users" => Some(list_blocked_users(args).await),
        _ => None,
    }
}

async fn block_user(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        blocker_id: String,
        blocked_id: String,
    }
    let Args {
        blocker_id,
        blocked_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::blocks::block_user(blocker_id, blocked_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn unblock_user(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        blocker_id: String,
        blocked_id: String,
    }
    let Args {
        blocker_id,
        blocked_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::blocks::unblock_user(blocker_id, blocked_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn list_blocked_users(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::blocks::list_blocked_users(user_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}
