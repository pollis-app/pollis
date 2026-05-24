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

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "subscribe_screen_share_events" => Some(Err(Error::from_reason(
            "Phase 3: NapiSink not yet wired for subscribe_screen_share_events".to_string(),
        ))),
        "subscribe_screen_share_frames" => Some(Err(Error::from_reason(
            "Phase 3: NapiSink (raw-bytes) not yet wired for subscribe_screen_share_frames"
                .to_string(),
        ))),
        "start_screen_share" => Some(Err(Error::from_reason(
            "Phase 3: pollis_capture_proto::Selection needs a re-export through pollis-core before start_screen_share can be ported (pollis-node may not add the dep directly)".to_string(),
        ))),
        "enumerate_screen_sources" => Some(enumerate_screen_sources(args).await),
        "cancel_screen_share_picker" => Some(cancel_screen_share_picker(args).await),
        "stop_screen_share" => Some(stop_screen_share(args).await),
        _ => None,
    }
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
