//! SHA-256 hashing primitives for an RFC 6962-style Merkle tree.
//!
//! RFC 6962 domain-separates leaves from interior nodes so that no leaf can
//! ever be confused with an internal node (a second-preimage defence):
//!
//! * leaf hash     = SHA-256(0x00 || entry_bytes)
//! * interior node = SHA-256(0x01 || left_hash || right_hash)

use sha2::{Digest, Sha256};

use crate::error::Error;

/// A 32-byte SHA-256 digest. The fundamental currency of the tree: leaf
/// hashes, interior nodes, and roots are all `Hash` values.
pub type Hash = [u8; 32];

/// Domain-separation prefix for leaf hashes (RFC 6962 §2.1).
const LEAF_PREFIX: u8 = 0x00;
/// Domain-separation prefix for interior node hashes (RFC 6962 §2.1).
const NODE_PREFIX: u8 = 0x01;

/// Hash of the empty string. Per RFC 6962 the Merkle tree hash of an empty
/// list of entries is `SHA-256()`.
pub fn empty_root() -> Hash {
    Sha256::digest([]).into()
}

/// Compute the leaf hash for an entry's canonical bytes:
/// `SHA-256(0x00 || entry_bytes)`.
pub fn leaf_hash(entry_bytes: &[u8]) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update([LEAF_PREFIX]);
    hasher.update(entry_bytes);
    hasher.finalize().into()
}

/// Combine two child hashes into their parent:
/// `SHA-256(0x01 || left || right)`.
pub fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update([NODE_PREFIX]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// Lowercase-hex encode a hash for the JSON wire contract.
pub fn to_hex(hash: &Hash) -> String {
    hex::encode(hash)
}

/// Decode a lowercase-hex string into a 32-byte hash, rejecting anything that
/// is not exactly 32 bytes.
pub fn from_hex(s: &str) -> Result<Hash, Error> {
    let bytes = hex::decode(s)?;
    if bytes.len() != 32 {
        return Err(Error::BadHashLength(bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}
