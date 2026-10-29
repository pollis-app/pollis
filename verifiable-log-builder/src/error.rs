//! Builder error type. The build/verify path returns `Result` and never
//! panics; the only `panic!`-shaped exit is the CLI mapping an error to a
//! non-zero process code.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BuilderError {
    /// Only reachable with `db-source` on — the feature that compiles the
    /// database reader at all.
    #[cfg(feature = "db-source")]
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

    /// The persisted commit-log leaves (#1579) are missing, shrunk, rewritten,
    /// or disagree with the live rows below their head. Always fails closed.
    #[error("commit-log leaves refused: {0}")]
    PersistedLeaves(String),

    /// The commit-log reset statement (#1579) could not be minted: the committed
    /// retirement facts disagree with what is served, or the successor has no
    /// genesis yet.
    #[error("commit-log reset refused: {0}")]
    Reset(String),

    #[error("invalid signing key: {0}")]
    SigningKey(String),

    #[error("no database source: pass --db <url-or-path> or set TURSO_DATABASE_URL")]
    NoDbSource,

    #[error("commit log is empty: nothing to build a bundle from (db connected OK)")]
    EmptyCommitLog,

    #[error("account-key log is empty: nothing to build a bundle from (db connected OK)")]
    EmptyAccountKeyLog,

    #[error(
        "no signing key: set env `{0}` to 32-byte hex, or pass --signing-key-file <path>. \
         Refusing to invent a key. Use `builder keygen` to mint a throwaway dev key."
    )]
    NoSigningKey(String),
}

pub type Result<T> = std::result::Result<T, BuilderError>;
