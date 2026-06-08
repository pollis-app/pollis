//! Signed Tree Head (STH) — an Ed25519 signature over the tree's state at a
//! point in time. This is the log's accountable commitment: a monitor that
//! collects STHs can detect equivocation, and inclusion/consistency proofs are
//! checked relative to an STH's root.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::{from_hex, to_hex, Hash};

/// Domain-separation tag for the signed message, so an STH signature can never
/// be confused with a signature over anything else.
const STH_DOMAIN: &[u8] = b"pollis-verifiable-log:sth:v1";

/// A Signed Tree Head. Wire shape (see `README.md`): all binary fields are
/// lowercase hex. This is part of the frozen contract a future serve layer
/// must emit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sth {
    /// Number of leaves committed by this head.
    pub tree_size: u64,
    /// Merkle root over those leaves, hex-encoded (32 bytes).
    pub root_hash: String,
    /// Caller-supplied timestamp (milliseconds since epoch, by convention).
    pub timestamp: u64,
    /// Ed25519 signature over the canonical message, hex-encoded (64 bytes).
    pub signature: String,
}

/// Canonical bytes signed by an STH:
/// `DOMAIN || tree_size(u64 BE) || root_hash(32) || timestamp(u64 BE)`.
fn signing_message(tree_size: u64, root: &Hash, timestamp: u64) -> Vec<u8> {
    let mut m = Vec::with_capacity(STH_DOMAIN.len() + 8 + 32 + 8);
    m.extend_from_slice(STH_DOMAIN);
    m.extend_from_slice(&tree_size.to_be_bytes());
    m.extend_from_slice(root);
    m.extend_from_slice(&timestamp.to_be_bytes());
    m
}

impl Sth {
    /// Sign a tree head. The signature commits to size, root, and timestamp
    /// together, so none can be altered without detection.
    pub fn create(signing_key: &SigningKey, tree_size: u64, root: Hash, timestamp: u64) -> Self {
        let message = signing_message(tree_size, &root, timestamp);
        let signature: Signature = signing_key.sign(&message);
        Self {
            tree_size,
            root_hash: to_hex(&root),
            timestamp,
            signature: to_hex_sig(&signature),
        }
    }

    /// Decode the root hash field into bytes.
    pub fn root_bytes(&self) -> Result<Hash> {
        from_hex(&self.root_hash)
    }

    /// Verify this STH's signature against `verifying_key`. Returns `false`
    /// (never panics) if any field is malformed or the signature is invalid.
    pub fn verify(&self, verifying_key: &VerifyingKey) -> bool {
        let root = match self.root_bytes() {
            Ok(r) => r,
            Err(_) => return false,
        };
        let signature = match sig_from_hex(&self.signature) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let message = signing_message(self.tree_size, &root, self.timestamp);
        verifying_key.verify(&message, &signature).is_ok()
    }
}

/// Two STHs that commit to the same `tree_size` but different roots are proof
/// of equivocation — the log signed two conflicting views of history. (The
/// caller is expected to have already verified both signatures.)
pub fn is_equivocation(a: &Sth, b: &Sth) -> bool {
    a.tree_size == b.tree_size && a.root_hash != b.root_hash
}

/// Parse a hex-encoded Ed25519 public key (32 bytes).
pub fn verifying_key_from_hex(s: &str) -> Result<VerifyingKey> {
    let bytes = hex::decode(s)?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::BadPublicKeyLength(bytes.len()))?;
    VerifyingKey::from_bytes(&arr).map_err(|_| Error::BadPublicKey)
}

fn to_hex_sig(sig: &Signature) -> String {
    hex::encode(sig.to_bytes())
}

fn sig_from_hex(s: &str) -> Result<Signature> {
    let bytes = hex::decode(s)?;
    let arr: [u8; 64] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::BadSignatureLength(bytes.len()))?;
    Ok(Signature::from_bytes(&arr))
}
