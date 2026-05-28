#[macro_use]
extern crate napi_derive;

mod dispatch;
mod events;
mod state;

pub use events::{register_event_emitters, JsRawFrame};

use std::path::PathBuf;

use napi::bindgen_prelude::*;

use crate::state::ensure_state;

/// Synchronous smoke test — no AppState, no async, no DB. Survives even
/// when env vars are missing. Kept for `node -e require('pollis-node').ping()`
/// during install / CI verification.
#[napi]
pub fn ping() -> String {
    "pong from pollis-core".to_string()
}

/// Load a .env file (dev) and bootstrap AppState. Call once from Electron
/// main at startup so config errors fail fast instead of surfacing on the
/// first `invoke()`. Idempotent — second call is a no-op.
#[napi]
pub async fn init(env_file: Option<String>) -> Result<()> {
    if let Some(path) = env_file {
        // Non-fatal: in prod, values are baked in at compile time via
        // option_env! (see pollis-core/src/config.rs) and there's no .env.
        let _ = dotenvy::from_filename(&path);
    }
    ensure_state().await?;
    // Open the host audio device in a background thread now so the first
    // user-facing sound (typically an incoming-call ringtone) doesn't pay
    // the cold-open cost. Linux/PulseAudio/PipeWire and idle Windows audio
    // can take seconds to wake on first `try_default()`; doing it eagerly
    // means the IPC for `start_ring` returns near-instantly.
    pollis_core::commands::sfx::prewarm_audio();
    Ok(())
}

/// Single entry point for every pollis-core command. Mirrors Tauri's
/// `invoke_handler!` macro shape so the JS-side `invoke()` call survives
/// the migration unchanged. `args` is the JSON object the renderer would
/// have passed to Tauri.
#[napi]
pub async fn invoke(
    cmd: String,
    args: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let args = args.unwrap_or(serde_json::Value::Null);
    dispatch::route(&cmd, args).await
}

/// Binary-body entry point. Used by commands that ship raw bytes on the
/// hot path — today only `terminal_write`, which is keystroke-rate so
/// JSON-encoding each call would re-introduce the typing latency that
/// commits `2b877d0` and `850661b` fixed under Tauri.
///
/// The renderer-side bridge auto-routes invoke() calls with Uint8Array
/// args through here (see electron/src/main.ts `ipcMain.handle("invoke")`),
/// so callers don't need to know about it. `body` is a napi Buffer —
/// zero-copy wrapper around the JS ArrayBuffer. `headers` is the
/// `options.headers` object the JS side passed (e.g.
/// `{ "x-terminal-id": "<id>" }`).
#[napi]
pub async fn invoke_raw(
    cmd: String,
    body: napi::bindgen_prelude::Buffer,
    headers: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let headers = headers.unwrap_or(serde_json::Value::Null);
    dispatch::route_raw(&cmd, body.as_ref(), &headers).await
}

/// Bootstrap the local loopback HTTP media server. Mirrors the boot pattern
/// in `src-tauri/src/lib.rs:347-354` — sets the on-disk media cache dir on
/// the r2 commands module, spawns the axum server on an OS-assigned port,
/// and parks the port on AppState so `get_media_url` returns valid URLs.
///
/// Call once from Electron main after `init()`. Returns the bound port.
#[napi]
pub async fn start_media_server(cache_dir: String) -> Result<u32> {
    let cache_dir = PathBuf::from(cache_dir);
    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        return Err(Error::from_reason(format!(
            "create_dir_all({}): {e}",
            cache_dir.display()
        )));
    }
    pollis_core::commands::r2::set_media_cache_dir(cache_dir);
    let state = ensure_state().await?;
    let port = pollis_core::media_server::spawn(state.clone())
        .await
        .map_err(|e| Error::from_reason(format!("spawn media server: {e}")))?;
    *state.media_server_port.lock().await = Some(port);
    Ok(port as u32)
}

/// Gracefully tear down long-lived backend tasks so the host process
/// can exit cleanly. Call from Electron's `before-quit` (covers Cmd+Q,
/// dock quit, OS shutdown) and from the updater's `update-downloaded`
/// handler before `quitAndInstall` (so Squirrel.Mac's ShipIt isn't
/// stranded waiting for the parent PID to die).
///
/// Idempotent. If `init()` was never called (e.g. dev crash before
/// state init), this is a no-op and returns `Ok`. Errors from the
/// underlying `AppState::shutdown` are swallowed-with-log — failing to
/// quit cleanly should not block the host from exiting.
#[napi]
pub async fn shutdown() -> Result<()> {
    if let Some(state) = crate::state::try_state() {
        state.shutdown().await;
    }
    Ok(())
}
