uniffi::setup_scaffolding!();

pub mod accounts;
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

#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
