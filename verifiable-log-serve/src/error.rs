//! Serve-layer error type.

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

    /// The served log's wire format is newer than this build understands:
    /// version skew, not tampering. Distinct so `pollis-verify` reports "upgrade"
    /// with its own exit code instead of "verification failed" (#668).
    #[error(
        "log format v{served} is newer than this verifier understands (up to v{supported}) — \
         your pollis-verify is too old for this log; upgrade it"
    )]
    VersionSkew { served: u32, supported: u32 },

    #[error(
        "log format v{served} is older than this verifier can interpret per-conversation \
         (needs v{required}+) — the log has not been republished since the commit-log leaf \
         encoding changed, so a conversation lookup here would report an empty result rather \
         than the truth. Re-run once the log is republished, or use a verifier from before \
         the change"
    )]
    LogTooOld { served: u32, required: u32 },

    #[error("malformed bundle: {0}")]
    BadBundle(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("builder error: {0}")]
    Builder(#[from] verifiable_log_builder::BuilderError),
}

pub type Result<T> = std::result::Result<T, ServeError>;
