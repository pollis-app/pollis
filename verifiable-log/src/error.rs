//! Error and result types. The verification paths never panic — every
//! fallible operation returns a `Result` so a monitor can fail a proof
//! cleanly instead of crashing.

use thiserror::Error;

/// A per-tenant invariant rejected an append. Carries the tenant id and a
/// human-readable reason so a monitor can produce an actionable report.
#[derive(Debug, Clone, Error)]
#[error("tenant `{tenant}` invariant violated: {message}")]
pub struct InvariantViolation {
    pub tenant: String,
    pub message: String,
}

impl InvariantViolation {
    pub fn new(tenant: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            tenant: tenant.into(),
            message: message.into(),
        }
    }
}

/// All errors produced by the library.
#[derive(Debug, Error)]
pub enum Error {
    #[error("leaf index {index} out of range for tree of size {size}")]
    IndexOutOfRange { index: usize, size: usize },

    #[error("invalid tree sizes for consistency proof: first={first}, second={second}")]
    InvalidTreeSizes { first: usize, second: usize },

    #[error(transparent)]
    Invariant(#[from] InvariantViolation),

    #[error("invalid hex: {0}")]
    Hex(#[from] hex::FromHexError),

    #[error("malformed hash: expected 32 bytes, got {0}")]
    BadHashLength(usize),

    #[error("malformed signature: expected 64 bytes, got {0}")]
    BadSignatureLength(usize),

    #[error("malformed public key: expected 32 bytes, got {0}")]
    BadPublicKeyLength(usize),

    #[error("invalid ed25519 public key")]
    BadPublicKey,
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
