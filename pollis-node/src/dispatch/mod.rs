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

    // Tauri's `invoke` auto-converts JS camelCase arg keys (`deleteData`) to
    // Rust snake_case (`delete_data`) before deserializing into command args.
    // Reproduce that here so the frontend's existing call sites work
    // unmodified — without this, every multi-word arg would need either a
    // serde rename or a frontend rewrite, and we'd have ~140 of them.
    let args = camel_keys_to_snake(args);

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

/// Recursively rewrite object keys from camelCase to snake_case. Mirrors
/// Tauri's invoke arg convention so frontend code that sends e.g.
/// `{ deleteData: true }` lands on `delete_data: bool` Rust fields.
/// Values inside arrays / nested objects are walked too. Non-key strings
/// (string-valued fields, e.g. the `__CHANNEL__:<id>` markers) are
/// untouched.
fn camel_keys_to_snake(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(k, v)| (camel_to_snake_key(&k), camel_keys_to_snake(v)))
                .collect(),
        ),
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(camel_keys_to_snake).collect())
        }
        other => other,
    }
}

fn camel_to_snake_key(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}
