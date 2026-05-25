// Port of `src-tauri/src/commands/sfx.rs`. These are sync `pub fn`s in
// pollis-core that spawn detached threads internally for playback via the
// `rodio` crate (cpal under the hood). cpal opens the host audio device from
// any process — Electron's main process is fine, no preload-layer detour
// needed. No state, no Channel<T>.

use napi::bindgen_prelude::*;

use crate::state::json_err;

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "play_sfx" => Some(play_sfx(args).await),
        "start_ring" => Some(start_ring(args).await),
        "stop_ring" => Some(stop_ring(args).await),
        _ => None,
    }
}

async fn play_sfx(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        sound: String,
    }
    let Args { sound } = serde_json::from_value(args.clone()).map_err(json_err)?;
    pollis_core::commands::sfx::play_sfx(&sound);
    Ok(serde_json::Value::Null)
}

async fn start_ring(_args: &serde_json::Value) -> Result<serde_json::Value> {
    pollis_core::commands::sfx::start_ring();
    Ok(serde_json::Value::Null)
}

async fn stop_ring(_args: &serde_json::Value) -> Result<serde_json::Value> {
    pollis_core::commands::sfx::stop_ring();
    Ok(serde_json::Value::Null)
}
