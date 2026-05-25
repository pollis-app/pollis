// Port of `src-tauri/src/commands/voice.rs`. Channel-using commands
// (`subscribe_voice_events`) stay stubbed pending Phase 3 NapiSink.

use std::sync::Arc;

use napi::bindgen_prelude::*;

use crate::events::{extract_channel_id, NapiSink};
use crate::state::{core_err, ensure_state, json_err};
use pollis_core::commands::voice::VoiceEvent;
use pollis_core::sink::EventSink;

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "subscribe_voice_events" => Some(subscribe_voice_events(args).await),
        "list_audio_devices" => Some(list_audio_devices(args).await),
        "prepare_voice_connection" => Some(prepare_voice_connection(args).await),
        "join_voice_channel" => Some(join_voice_channel(args).await),
        "leave_voice_channel" => Some(leave_voice_channel(args).await),
        "toggle_voice_mute" => Some(toggle_voice_mute(args).await),
        "set_remote_user_volume" => Some(set_remote_user_volume(args).await),
        "set_voice_input_device" => Some(set_voice_input_device(args).await),
        "set_voice_output_device" => Some(set_voice_output_device(args).await),
        "set_voice_audio_processing" => Some(set_voice_audio_processing(args).await),
        "get_last_join_timings" => Some(get_last_join_timings(args).await),
        "get_voice_e2ee_key" => Some(get_voice_e2ee_key(args).await),
        _ => None,
    }
}

async fn get_voice_e2ee_key(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        channel_id: String,
        user_id: String,
        #[serde(default)]
        counterparty_user_id: Option<String>,
    }
    let Args {
        channel_id,
        user_id,
        counterparty_user_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let info = pollis_core::commands::voice_e2ee::get_voice_e2ee_key(
        channel_id,
        user_id,
        counterparty_user_id,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(info).map_err(json_err)
}

async fn subscribe_voice_events(args: &serde_json::Value) -> Result<serde_json::Value> {
    let channel_id = extract_channel_id(args, "on_event")?;
    let sink: Arc<dyn EventSink<VoiceEvent>> = Arc::new(NapiSink::new(channel_id));
    let state = ensure_state().await?;
    pollis_core::commands::voice::subscribe_voice_events(sink, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn list_audio_devices(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let out = pollis_core::commands::voice::list_audio_devices()
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn prepare_voice_connection(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        channel_id: String,
        user_id: String,
        display_name: String,
    }
    let Args {
        channel_id,
        user_id,
        display_name,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::voice::prepare_voice_connection(
        channel_id,
        user_id,
        display_name,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn join_voice_channel(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        channel_id: String,
        user_id: String,
        display_name: String,
        input_device: Option<String>,
        output_device: Option<String>,
        audio_processing: pollis_core::commands::voice_apm::ApmConfig,
        counterparty_user_id: Option<String>,
    }
    let Args {
        channel_id,
        user_id,
        display_name,
        input_device,
        output_device,
        audio_processing,
        counterparty_user_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::voice::join_voice_channel(
        channel_id,
        user_id,
        display_name,
        input_device,
        output_device,
        audio_processing,
        counterparty_user_id,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn leave_voice_channel(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    pollis_core::commands::voice::leave_voice_channel(&state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn toggle_voice_mute(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    let out = pollis_core::commands::voice::toggle_voice_mute(&state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn set_remote_user_volume(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        volume: f32,
    }
    let Args { user_id, volume } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::voice::set_remote_user_volume(user_id, volume, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn set_voice_input_device(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        device_name: String,
    }
    let Args { device_name } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::voice::set_voice_input_device(device_name, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn set_voice_output_device(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        device_name: String,
    }
    let Args { device_name } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::voice::set_voice_output_device(device_name, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn set_voice_audio_processing(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        config: pollis_core::commands::voice_apm::ApmConfig,
    }
    let Args { config } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::voice::set_voice_audio_processing(config, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn get_last_join_timings(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    let out = pollis_core::commands::voice::get_last_join_timings(&state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}
