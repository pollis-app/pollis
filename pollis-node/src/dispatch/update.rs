// Phase 2: port of `src-tauri/src/commands/update.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.
//
// Phase 2 agent: replace the stub with match arms for every command in
// docs/electron-migration-inventory.md under the `update` section. Channel-
// based commands stay stubbed (returning a Phase 3 TODO) — they need the
// NapiSink work in Phase 3.
//
// Both commands here are pure pollis-core wrappers — they touch `AppState`
// only and do not call `tauri_plugin_updater`. The actual update-check flow
// (currently `check` from `@tauri-apps/plugin-updater` on the frontend) will
// be swapped for `electron-updater` in Phase 7.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "mark_update_required" => Some(mark_update_required(args).await),
        "is_update_required" => Some(is_update_required(args).await),
        _ => None,
    }
}

async fn mark_update_required(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    pollis_core::commands::update::mark_update_required(&state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn is_update_required(_args: &serde_json::Value) -> Result<serde_json::Value> {
    let state = ensure_state().await?;
    let out = pollis_core::commands::update::is_update_required(&state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}
