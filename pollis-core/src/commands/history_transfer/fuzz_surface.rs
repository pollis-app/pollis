//! History transfer (#1735) for the detached `fuzz/` crate: the sealing pair
//! the receiver trusts nothing past, re-exported unchanged.

pub use super::crypto::{open, CryptoError};
pub use super::wire::*;

/// Seal with a caller-chosen key (production seals under the derived key).
pub fn seal(key: &[u8; 32], transfer_id: &str, index: u32, is_final: bool, plain: &[u8]) -> Vec<u8> {
    super::crypto::seal(key, transfer_id, index, is_final, plain)
}

/// SHA-256, as the chain uses it.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    super::crypto::sha256(bytes)
}
