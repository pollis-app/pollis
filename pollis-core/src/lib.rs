uniffi::setup_scaffolding!();

pub mod accounts;
pub mod config;
pub mod db;
pub mod error;
pub mod keystore;
pub mod signal;

#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
