// Per-module dispatch. Each `<module>.rs` owns the match arms for the
// commands its Tauri shim used to expose. `route()` walks the modules in
// order until one claims the command name.
//
// Phase 2 fills in each stub. Phase 3 wires `Channel<T>`-based commands
// once `NapiSink` lands. Until then, Channel-using commands stay in their
// module's `dispatch()` returning a clear "TODO Phase 3" error.

use napi::bindgen_prelude::*;

pub mod auth;
pub mod blocks;
pub mod device_enrollment;
pub mod dm;
pub mod groups;
pub mod install_kind;
pub mod livekit;
pub mod messages;
pub mod mls;
pub mod pin;
pub mod r2;
pub mod safety;
pub mod screenshare;
pub mod sfx;
pub mod terminal;
pub mod update;
pub mod user;
pub mod voice;
pub mod voice_test;

pub async fn route(cmd: &str, args: serde_json::Value) -> Result<serde_json::Value> {
    if cmd == "ping" {
        return Ok(serde_json::Value::String("pong".into()));
    }

    macro_rules! try_dispatch {
        ($module:ident) => {
            if let Some(r) = $module::dispatch(cmd, &args).await {
                return r;
            }
        };
    }

    try_dispatch!(auth);
    try_dispatch!(pin);
    try_dispatch!(device_enrollment);
    try_dispatch!(safety);
    try_dispatch!(user);
    try_dispatch!(groups);
    try_dispatch!(dm);
    try_dispatch!(blocks);
    try_dispatch!(messages);
    try_dispatch!(mls);
    try_dispatch!(livekit);
    try_dispatch!(voice);
    try_dispatch!(voice_test);
    try_dispatch!(screenshare);
    try_dispatch!(r2);
    try_dispatch!(sfx);
    try_dispatch!(terminal);
    try_dispatch!(update);
    try_dispatch!(install_kind);

    Err(Error::from_reason(format!("unknown command: {cmd}")))
}
