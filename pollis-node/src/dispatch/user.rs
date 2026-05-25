// Port of `src-tauri/src/commands/user.rs`. Pattern reference for the rest
// of Phase 2 — every other dispatch module follows this same shape.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "get_user_profile" => Some(get_user_profile(args).await),
        "update_user_profile" => Some(update_user_profile(args).await),
        "search_user_by_username" => Some(search_user_by_username(args).await),
        "get_preferences" => Some(get_preferences(args).await),
        "save_preferences" => Some(save_preferences(args).await),
        _ => None,
    }
}

async fn get_user_profile(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::user::get_user_profile(user_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn update_user_profile(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        username: Option<String>,
        preferred_name: Option<String>,
        phone: Option<String>,
        avatar_url: Option<String>,
    }
    let Args {
        user_id,
        username,
        preferred_name,
        phone,
        avatar_url,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::user::update_user_profile(
        user_id,
        username,
        preferred_name,
        phone,
        avatar_url,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn search_user_by_username(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        username: String,
    }
    let Args { username } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::user::search_user_by_username(username, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn get_preferences(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::user::get_preferences(user_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn save_preferences(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        preferences_json: String,
    }
    let Args {
        user_id,
        preferences_json,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::user::save_preferences(user_id, preferences_json, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}
