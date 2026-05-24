uniffi::setup_scaffolding!();

pub mod accounts;
pub mod bridge;
pub mod commands;
pub mod config;
pub mod db;
pub mod error;
pub mod keystore;
pub mod media_server;
pub mod realtime;
pub mod signal;
pub mod sink;
pub mod state;

// Re-export so downstream crates (pollis-node, src-tauri shims) can name
// types like `Selection`/`SourceList` that appear in screenshare command
// signatures without adding pollis-capture-proto as a direct dep.
pub use pollis_capture_proto;

#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
