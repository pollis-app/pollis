//! Error and result types. Verification paths return errors, never panic.

use thiserror::Error;

/// A per-tenant invariant rejected an append.
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

    #[error("malformed signature: expected 2420 bytes, got {0}")]
    BadSignatureLength(usize),

    #[error("malformed public key: expected 1312 bytes, got {0}")]
    BadPublicKeyLength(usize),

    #[error("invalid ML-DSA-44 public key")]
    BadPublicKey,

    #[error("invalid ML-DSA-44 signature encoding")]
    BadSignature,

    /// A key-set statement that is malformed, unsupported, or does not verify.
    #[error("invalid key-set statement: {0}")]
    KeySet(String),

    /// A reset statement (#1579) that is malformed, contradicts itself, or fails.
    #[error("invalid reset statement: {0}")]
    Reset(String),
}

pub type Result<T> = std::result::Result<T, Error>;
