// AppState bootstrap + shared error converters. Hot-path module — every
// dispatch arm calls `ensure_state()` to grab the global Arc<AppState>.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use tokio::sync::OnceCell;

use pollis_core::config::Config;
use pollis_core::error::Error as CoreError;
use pollis_core::state::AppState;

static APP_STATE: OnceCell<Arc<AppState>> = OnceCell::const_new();

pub fn core_err(e: CoreError) -> Error {
    Error::from_reason(e.to_string())
}

pub fn json_err(e: serde_json::Error) -> Error {
    Error::from_reason(format!("json: {e}"))
}

pub async fn ensure_state() -> Result<Arc<AppState>> {
    APP_STATE
        .get_or_try_init(|| async {
            let config = Config::from_env().map_err(core_err)?;
            let state = AppState::new(config).await.map_err(core_err)?;
            Ok::<Arc<AppState>, Error>(Arc::new(state))
        })
        .await
        .cloned()
}

/// Best-effort access to the global AppState without initializing it.
/// Returns `None` when `init()` was never called (or failed). Used by
/// the napi `shutdown()` export so a host that imports the module but
/// never calls `init()` can still call `shutdown()` without panicking.
pub fn try_state() -> Option<Arc<AppState>> {
    APP_STATE.get().cloned()
}
