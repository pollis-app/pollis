#[macro_use]
extern crate napi_derive;

use std::sync::Arc;

use napi::bindgen_prelude::*;
use tokio::sync::OnceCell;

use pollis_core::config::Config;
use pollis_core::error::Error as CoreError;
use pollis_core::state::AppState;

static APP_STATE: OnceCell<Arc<AppState>> = OnceCell::const_new();

fn core_err(e: CoreError) -> Error {
    Error::from_reason(e.to_string())
}

fn json_err(e: serde_json::Error) -> Error {
    Error::from_reason(format!("json: {e}"))
}

async fn ensure_state() -> Result<Arc<AppState>> {
    APP_STATE
        .get_or_try_init(|| async {
            let config = Config::from_env().map_err(core_err)?;
            let state = AppState::new(config).await.map_err(core_err)?;
            Ok::<Arc<AppState>, Error>(Arc::new(state))
        })
        .await
        .cloned()
}

/// Synchronous smoke test — no AppState, no async, no DB. Survives even
/// when env vars are missing. Kept for `node -e require('pollis-node').ping()`
/// during install / CI verification.
#[napi]
pub fn ping() -> String {
    "pong from pollis-core".to_string()
}

/// Load environment from a .env file (dev) and bootstrap AppState. Call
/// once from Electron main at startup so config errors fail fast instead
/// of surfacing on the first `invoke()`. Safe to call repeatedly — second
/// call is a no-op because of `OnceCell`.
#[napi]
pub async fn init(env_file: Option<String>) -> Result<()> {
    if let Some(path) = env_file {
        // Failure is non-fatal: in prod we expect option_env! to have baked
        // the values in at compile time, and there's no .env file to load.
        let _ = dotenvy::from_filename(&path);
    }
    ensure_state().await?;
    Ok(())
}

/// Single entry point for every pollis-core command, mirroring Tauri's
/// `invoke_handler!` macro. Phase 2 expands the match arm by arm; the
/// JS-side preload stays unchanged regardless of how many commands land.
///
/// `args` is the JSON object the renderer would have passed to Tauri's
/// `invoke(cmd, args)`. Null/undefined is fine for nullary commands.
#[napi]
pub async fn invoke(cmd: String, args: Option<serde_json::Value>) -> Result<serde_json::Value> {
    let args = args.unwrap_or(serde_json::Value::Null);
    dispatch(&cmd, args).await
}

async fn dispatch(cmd: &str, args: serde_json::Value) -> Result<serde_json::Value> {
    use pollis_core::commands;

    match cmd {
        // ── infrastructure ──────────────────────────────────────────────
        "ping" => Ok(serde_json::Value::String("pong".into())),

        // ── user ────────────────────────────────────────────────────────
        "get_user_profile" => {
            #[derive(serde::Deserialize)]
            struct Args { user_id: String }
            let Args { user_id } = serde_json::from_value(args).map_err(json_err)?;
            let state = ensure_state().await?;
            let result = commands::user::get_user_profile(user_id, &state)
                .await
                .map_err(core_err)?;
            serde_json::to_value(result).map_err(json_err)
        }

        // Phase 2 fills in the remaining 130+ commands here, one arm each.

        _ => Err(Error::from_reason(format!("unknown command: {cmd}"))),
    }
}
