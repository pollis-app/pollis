uniffi::setup_scaffolding!();

pub mod accounts;
#[cfg(target_os = "android")]
pub mod android_tls;
#[cfg(target_os = "ios")]
pub mod ios_storage;
pub mod bridge;
pub mod client_policy;
pub mod commands;
pub mod config;
pub mod db;
pub mod diag;
pub mod error;
pub mod keystore;
pub mod media_server;
pub mod net;
pub mod notify;
pub mod downloads;
pub mod private_fs;
pub mod realtime;
pub mod signal;
pub mod sink;
pub mod state;
pub mod util;

pub use db::local::set_data_dir_override;

// Re-exported so downstream crates can name screenshare types without a direct dep.
pub use pollis_capture_proto;

#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
