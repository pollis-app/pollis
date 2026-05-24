// Phase 2: port of `src-tauri/src/commands/dm.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "create_dm_channel" => Some(create_dm_channel(args).await),
        "list_dm_channels" => Some(list_dm_channels(args).await),
        "list_dm_requests" => Some(list_dm_requests(args).await),
        "accept_dm_request" => Some(accept_dm_request(args).await),
        "get_dm_channel" => Some(get_dm_channel(args).await),
        "add_user_to_dm_channel" => Some(add_user_to_dm_channel(args).await),
        "remove_user_from_dm_channel" => Some(remove_user_from_dm_channel(args).await),
        "leave_dm_channel" => Some(leave_dm_channel(args).await),
        _ => None,
    }
}

async fn create_dm_channel(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        creator_id: String,
        member_ids: Vec<String>,
    }
    let Args {
        creator_id,
        member_ids,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::dm::create_dm_channel(creator_id, member_ids, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn list_dm_channels(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::dm::list_dm_channels(user_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn list_dm_requests(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::dm::list_dm_requests(user_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn accept_dm_request(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        dm_channel_id: String,
        user_id: String,
    }
    let Args {
        dm_channel_id,
        user_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::dm::accept_dm_request(dm_channel_id, user_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn get_dm_channel(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        dm_channel_id: String,
    }
    let Args { dm_channel_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::dm::get_dm_channel(dm_channel_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn add_user_to_dm_channel(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        dm_channel_id: String,
        user_id: String,
        added_by: String,
    }
    let Args {
        dm_channel_id,
        user_id,
        added_by,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::dm::add_user_to_dm_channel(dm_channel_id, user_id, added_by, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn remove_user_from_dm_channel(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        dm_channel_id: String,
        user_id: String,
        requester_id: String,
    }
    let Args {
        dm_channel_id,
        user_id,
        requester_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::dm::remove_user_from_dm_channel(
        dm_channel_id,
        user_id,
        requester_id,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn leave_dm_channel(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        dm_channel_id: String,
        user_id: String,
    }
    let Args {
        dm_channel_id,
        user_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::dm::leave_dm_channel(dm_channel_id, user_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}
