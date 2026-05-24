// Port of `src-tauri/src/commands/voice_test.rs`. Channel-using commands
// (`subscribe_voice_test_events`) stay stubbed pending Phase 3 NapiSink.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "subscribe_voice_test_events" => Some(Err(Error::from_reason(
            "Phase 3: NapiSink not yet wired for subscribe_voice_test_events".to_string(),
        ))),
        "start_mic_test" => Some(start_mic_test(args).await),
        "set_mic_test_monitor" => Some(set_mic_test_monitor(args).await),
        "stop_mic_test" => Some(stop_mic_test(args).await),
        "record_and_play_back" => Some(record_and_play_back(args).await),
        "play_test_tone" => Some(play_test_tone(args).await),
        "stop_test_playback" => Some(stop_test_playback(args).await),
        _ => None,
    }
}

async fn start_mic_test(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        input_device_id: String,
        output_device_id: String,
        monitor: bool,
    }
    let Args {
        input_device_id,
        output_device_id,
        monitor,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::voice_test::start_mic_test(
        input_device_id,
        output_device_id,
        monitor,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn set_mic_test_monitor(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        enabled: bool,
        output_device_id: String,
    }
    let Args {
        enabled,
        output_device_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::voice_test::set_mic_test_monitor(enabled, output_device_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn stop_mic_test(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    pollis_core::commands::voice_test::stop_mic_test(&state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn record_and_play_back(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        input_device_id: String,
        output_device_id: String,
        duration_ms: u32,
    }
    let Args {
        input_device_id,
        output_device_id,
        duration_ms,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::voice_test::record_and_play_back(
        input_device_id,
        output_device_id,
        duration_ms,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn play_test_tone(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        output_device_id: String,
        kind: String,
    }
    let Args {
        output_device_id,
        kind,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::voice_test::play_test_tone(output_device_id, kind, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn stop_test_playback(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    pollis_core::commands::voice_test::stop_test_playback(&state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}
