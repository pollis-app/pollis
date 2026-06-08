//! Serve-layer error type. Every path here returns `Result` and never panics;
//! the only `panic!`-shaped exit is the CLI mapping an error to a non-zero
//! process code.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("verifiable-log error: {0}")]
    Log(#[from] verifiable_log::Error),

    #[error("http error: {0}")]
    Http(String),

    #[error("malformed bundle: {0}")]
    BadBundle(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("builder error: {0}")]
    Builder(#[from] verifiable_log_builder::BuilderError),
}

pub type Result<T> = std::result::Result<T, ServeError>;
