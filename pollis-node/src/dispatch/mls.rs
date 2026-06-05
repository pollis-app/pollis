// Port of `src-tauri/src/commands/mls.rs`. All eight commands are
// request/response (no Channel<T> in this module per
// docs/electron-migration-inventory.md). Realtime "welcomes arrived" /
// "group changed" notifications ride the `RealtimeEvent` channel owned by
// `livekit` — not by mls itself — so nothing here needs the Phase 3 NapiSink.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "generate_mls_key_package" => Some(generate_mls_key_package(args).await),
        "publish_mls_key_package" => Some(publish_mls_key_package(args).await),
        "fetch_mls_key_package" => Some(fetch_mls_key_package(args).await),
        "create_mls_group" => Some(create_mls_group(args).await),
        "process_welcome" => Some(process_welcome(args).await),
        "poll_mls_welcomes" => Some(poll_mls_welcomes(args).await),
        "reconcile_group_mls" => Some(reconcile_group_mls(args).await),
        "process_pending_commits" => Some(process_pending_commits(args).await),
        "catch_up_all_mls_groups" => Some(catch_up_all_mls_groups(args).await),
        _ => None,
    }
}

async fn generate_mls_key_package(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::mls::generate_mls_key_package(&state, user_id)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn publish_mls_key_package(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        ref_hex: String,
        key_package_bytes: Vec<u8>,
    }
    let Args {
        user_id,
        ref_hex,
        key_package_bytes,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::mls::publish_mls_key_package(
        &state,
        user_id,
        ref_hex,
        key_package_bytes,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn fetch_mls_key_package(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        target_user_id: String,
    }
    let Args { target_user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::mls::fetch_mls_key_package(&state, target_user_id)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn create_mls_group(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        conversation_id: String,
        creator_user_id: String,
    }
    let Args {
        conversation_id,
        creator_user_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::mls::create_mls_group(&state, conversation_id, creator_user_id)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn process_welcome(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        welcome_bytes: Vec<u8>,
    }
    let Args { welcome_bytes } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::mls::process_welcome(&state, welcome_bytes)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn poll_mls_welcomes(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::mls::poll_mls_welcomes(&state, user_id)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn reconcile_group_mls(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        conversation_id: String,
        actor_user_id: String,
    }
    let Args {
        conversation_id,
        actor_user_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::mls::reconcile_group_mls(&state, conversation_id, actor_user_id)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn process_pending_commits(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        conversation_id: String,
        user_id: String,
    }
    let Args {
        conversation_id,
        user_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::mls::process_pending_commits(&state, conversation_id, user_id)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn catch_up_all_mls_groups(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::mls::catch_up_all_mls_groups(&state, &user_id)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}
