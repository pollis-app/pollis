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

    /// The served log is published in a wire format newer than this build
    /// understands. This is **version skew, not tampering**: the verifier is too
    /// old for the log and must be upgraded. Kept distinct from every other
    /// variant precisely so `pollis-verify` can report "upgrade your verifier"
    /// with its own exit code instead of "verification failed" — the same
    /// false-alarm failure mode #668 ruled out for the missing log pin.
    #[error(
        "log format v{served} is newer than this verifier understands (up to v{supported}) — \
         your pollis-verify is too old for this log; upgrade it"
    )]
    VersionSkew { served: u32, supported: u32 },

    #[error("malformed bundle: {0}")]
    BadBundle(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("builder error: {0}")]
    Builder(#[from] verifiable_log_builder::BuilderError),
}

pub type Result<T> = std::result::Result<T, ServeError>;
