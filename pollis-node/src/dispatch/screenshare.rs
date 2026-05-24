// Port of `src-tauri/src/commands/screenshare.rs`. Channel-using commands
// (`subscribe_screen_share_events`, `subscribe_screen_share_frames`) stay
// stubbed pending Phase 3 NapiSink — the frames channel additionally needs
// the raw-bytes path equivalent to `InvokeResponseBody::Raw`.
//
// `start_screen_share` is also stubbed because its `Selection` arg is the
// `pollis_capture_proto::Selection` type, which is not re-exported through
// pollis-core. Naming it directly would require adding `pollis-capture-proto`
// to pollis-node/Cargo.toml (forbidden by the Phase 2 constraint) or editing
// pollis-core to re-export it (out of scope for this dispatch file). Phase 3
// should either add the re-export or surface the type through a thin
// dispatch-friendly wrapper.

use std::sync::Arc;

use napi::bindgen_prelude::*;

use crate::events::{extract_channel_id, NapiSink, RawNapiSink};
use crate::state::{core_err, ensure_state, json_err};
use pollis_core::commands::screenshare::ScreenShareEvent;
use pollis_core::sink::{EventSink, RawSink};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "subscribe_screen_share_events" => Some(subscribe_screen_share_events(args).await),
        "subscribe_screen_share_frames" => Some(subscribe_screen_share_frames(args).await),
        "start_screen_share" => Some(start_screen_share(args).await),
        "enumerate_screen_sources" => Some(enumerate_screen_sources(args).await),
        "cancel_screen_share_picker" => Some(cancel_screen_share_picker(args).await),
        "stop_screen_share" => Some(stop_screen_share(args).await),
        _ => None,
    }
}

async fn start_screen_share(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        #[serde(default)]
        selection: Option<pollis_core::pollis_capture_proto::Selection>,
    }
    let Args { selection } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::screenshare::start_screen_share(&state, selection)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn subscribe_screen_share_events(args: &serde_json::Value) -> Result<serde_json::Value> {
    let channel_id = extract_channel_id(args, "on_event")?;
    let sink: Arc<dyn EventSink<ScreenShareEvent>> = Arc::new(NapiSink::new(channel_id));
    let state = ensure_state().await?;
    pollis_core::commands::screenshare::subscribe_screen_share_events(sink, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn subscribe_screen_share_frames(args: &serde_json::Value) -> Result<serde_json::Value> {
    let channel_id = extract_channel_id(args, "on_frame")?;
    let sink: Arc<dyn RawSink> = Arc::new(RawNapiSink::new(channel_id));
    let state = ensure_state().await?;
    pollis_core::commands::screenshare::subscribe_screen_share_frames(sink, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn enumerate_screen_sources(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    let out = pollis_core::commands::screenshare::enumerate_screen_sources(&state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn cancel_screen_share_picker(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    pollis_core::commands::screenshare::cancel_screen_share_picker(&state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn stop_screen_share(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    pollis_core::commands::screenshare::stop_screen_share(&state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}
