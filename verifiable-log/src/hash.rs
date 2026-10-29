//! SHA-256 primitives for an RFC 6962 Merkle tree. Leaves and interior nodes
//! are domain-separated (0x00 / 0x01 prefix) as a second-preimage defence.

use sha2::{Digest, Sha256};

use crate::error::Error;

/// A 32-byte SHA-256 digest: leaf hashes, interior nodes and roots.
pub type Hash = [u8; 32];

/// Domain-separation prefix for leaf hashes (RFC 6962 §2.1).
const LEAF_PREFIX: u8 = 0x00;
/// Domain-separation prefix for interior node hashes (RFC 6962 §2.1).
const NODE_PREFIX: u8 = 0x01;

/// The RFC 6962 root of an empty tree: `SHA-256()`.
pub fn empty_root() -> Hash {
    Sha256::digest([]).into()
}

/// `SHA-256(0x00 || entry_bytes)`.
pub fn leaf_hash(entry_bytes: &[u8]) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update([LEAF_PREFIX]);
    hasher.update(entry_bytes);
    hasher.finalize().into()
}

/// `SHA-256(0x01 || left || right)`.
pub fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update([NODE_PREFIX]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

pub fn to_hex(hash: &Hash) -> String {
    hex::encode(hash)
}

/// Decode hex into a hash, rejecting anything not exactly 32 bytes.
pub fn from_hex(s: &str) -> Result<Hash, Error> {
    let bytes = hex::decode(s)?;
    if bytes.len() != 32 {
        return Err(Error::BadHashLength(bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}
