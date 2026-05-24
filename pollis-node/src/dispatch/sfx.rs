// Phase 2: port of `src-tauri/src/commands/sfx.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.
//
// Phase 2 agent: replace the stub with match arms for every command in
// docs/electron-migration-inventory.md under the `sfx` section. Channel-
// based commands stay stubbed (returning a Phase 3 TODO) — they need the
// NapiSink work in Phase 3.

use napi::bindgen_prelude::*;

pub async fn dispatch(
    _cmd: &str,
    _args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    None
}
