#[macro_use]
extern crate napi_derive;

mod dispatch;
mod state;

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
