// Phase 2: port of `src-tauri/src/commands/livekit.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.
//
// Phase 2 agent: replace the stub with match arms for every command in
// docs/electron-migration-inventory.md under the `livekit` section. Channel-
// based commands stay stubbed (returning a Phase 3 TODO) — they need the
// NapiSink work in Phase 3.

use std::sync::Arc;

use napi::bindgen_prelude::*;

use crate::events::{extract_channel_id, NapiSink};
use crate::state::{core_err, ensure_state, json_err};
use pollis_core::realtime::RealtimeEvent;
use pollis_core::sink::EventSink;

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "get_livekit_token" => Some(get_livekit_token(args).await),
        "get_livekit_url" => Some(get_livekit_url(args).await),
        "subscribe_realtime" => Some(subscribe_realtime(args).await),
        "connect_rooms" => Some(connect_rooms(args).await),
        "publish_ping" => Some(publish_ping(args).await),
        "publish_typing" => Some(publish_typing(args).await),
        "publish_voice_presence" => Some(publish_voice_presence(args).await),
        "list_voice_participants" => Some(list_voice_participants(args).await),
        "list_voice_room_counts" => Some(list_voice_room_counts(args).await),
        "start_call" => Some(start_call(args).await),
        "cancel_call" => Some(cancel_call(args).await),
        _ => None,
    }
}

async fn subscribe_realtime(args: &serde_json::Value) -> Result<serde_json::Value> {
    let channel_id = extract_channel_id(args, "onEvent")?;
    let sink: Arc<dyn EventSink<RealtimeEvent>> = Arc::new(NapiSink::new(channel_id));
    let state = ensure_state().await?;
    pollis_core::commands::livekit::subscribe_realtime(sink, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn get_livekit_token(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        room_name: String,
        identity: String,
        display_name: String,
    }
    let Args {
        room_name,
        identity,
        display_name,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::livekit::get_livekit_token(
        room_name,
        identity,
        display_name,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn get_livekit_url(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    let out = pollis_core::commands::livekit::get_livekit_url(&state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn connect_rooms(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        room_ids: Vec<String>,
        user_id: String,
        username: String,
    }
    let Args {
        room_ids,
        user_id,
        username,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::livekit::connect_rooms(room_ids, user_id, username, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn publish_ping(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        room_id: String,
        channel_id: Option<String>,
        conversation_id: Option<String>,
        sender_id: String,
        sender_username: Option<String>,
    }
    let Args {
        room_id,
        channel_id,
        conversation_id,
        sender_id,
        sender_username,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::livekit::publish_ping(
        room_id,
        channel_id,
        conversation_id,
        sender_id,
        sender_username,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn publish_typing(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        room_id: String,
        channel_id: Option<String>,
        conversation_id: Option<String>,
        user_id: String,
        username: Option<String>,
        is_typing: bool,
    }
    let Args {
        room_id,
        channel_id,
        conversation_id,
        user_id,
        username,
        is_typing,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::livekit::publish_typing(
        room_id,
        channel_id,
        conversation_id,
        user_id,
        username,
        is_typing,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn publish_voice_presence(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        channel_id: String,
        user_id: String,
        display_name: String,
        joined: bool,
    }
    let Args {
        group_id,
        channel_id,
        user_id,
        display_name,
        joined,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::livekit::publish_voice_presence(
        group_id,
        channel_id,
        user_id,
        display_name,
        joined,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn list_voice_participants(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        channel_id: String,
    }
    let Args { channel_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::livekit::list_voice_participants(channel_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn list_voice_room_counts(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        channel_ids: Vec<String>,
    }
    let Args { channel_ids } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::livekit::list_voice_room_counts(channel_ids, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn start_call(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        callee_id: String,
        caller_id: String,
        caller_username: String,
    }
    let Args {
        callee_id,
        caller_id,
        caller_username,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::livekit::start_call(
        callee_id,
        caller_id,
        caller_username,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn cancel_call(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        other_user_id: String,
        call_id: String,
    }
    let Args {
        other_user_id,
        call_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::livekit::cancel_call(other_user_id, call_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}
