//! Builder error type. The build/verify path returns `Result` and never
//! panics; the only `panic!`-shaped exit is the CLI mapping an error to a
//! non-zero process code.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BuilderError {
    #[error("database error: {0}")]
    Db(#[from] libsql::Error),

    #[error("verifiable-log error: {0}")]
    Log(#[from] verifiable_log::Error),

    #[error("tenant invariant violated: {0}")]
    Invariant(#[from] verifiable_log::InvariantViolation),

    #[error("leaf encoding error: {0}")]
    Encode(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid signing key: {0}")]
    SigningKey(String),

    #[error("no database source: pass --db <url-or-path> or set TURSO_DATABASE_URL")]
    NoDbSource,

    #[error(
        "no signing key: set env `{0}` to 32-byte hex, or pass --signing-key-file <path>. \
         Refusing to invent a key. Use `builder keygen` to mint a throwaway dev key."
    )]
    NoSigningKey(String),
}

pub type Result<T> = std::result::Result<T, BuilderError>;
