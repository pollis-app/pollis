//! Signed Tree Head (STH) — an Ed25519 signature over the tree's state at a
//! point in time. This is the log's accountable commitment: a monitor that
//! collects STHs can detect equivocation, and inclusion/consistency proofs are
//! checked relative to an STH's root.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::{from_hex, to_hex, Hash};

/// Default domain-separation tag for the signed message, so an STH signature can
/// never be confused with a signature over anything else. This is the **frozen**
/// context for the original (MLS commit-log) tree — already-published STHs were
/// signed under it, so it must never change.
///
/// A second tenant that wants its **own** tree (e.g. the account-key directory)
/// must sign with a *different* context via [`Sth::create_with_context`] /
/// [`Sth::verify_with_context`], so an STH minted for one log can never be
/// replayed as the other's. See the builder's account-key module for the
/// concrete account-keys context string.
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

/// Canonical bytes signed by an STH, under the given domain-separation context:
/// `CONTEXT || tree_size(u64 BE) || root_hash(32) || timestamp(u64 BE)`.
fn signing_message_with_context(
    context: &[u8],
    tree_size: u64,
    root: &Hash,
    timestamp: u64,
) -> Vec<u8> {
    let mut m = Vec::with_capacity(context.len() + 8 + 32 + 8);
    m.extend_from_slice(context);
    m.extend_from_slice(&tree_size.to_be_bytes());
    m.extend_from_slice(root);
    m.extend_from_slice(&timestamp.to_be_bytes());
    m
}

impl Sth {
    /// Sign a tree head under the default ([`STH_DOMAIN`]) context. The signature
    /// commits to size, root, and timestamp together, so none can be altered
    /// without detection. This is the frozen commit-log path — its bytes must not
    /// change.
    pub fn create(signing_key: &SigningKey, tree_size: u64, root: Hash, timestamp: u64) -> Self {
        Self::create_with_context(signing_key, tree_size, root, timestamp, STH_DOMAIN)
    }

    /// Sign a tree head under an explicit domain-separation `context`. A second
    /// tenant's tree signs with its own context so an STH from one log can never
    /// be presented as the other's; the wire shape is identical, only the signed
    /// preimage differs.
    pub fn create_with_context(
        signing_key: &SigningKey,
        tree_size: u64,
        root: Hash,
        timestamp: u64,
        context: &[u8],
    ) -> Self {
        let message = signing_message_with_context(context, tree_size, &root, timestamp);
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

    /// Verify this STH's signature against `verifying_key` under the default
    /// ([`STH_DOMAIN`]) context. Returns `false` (never panics) if any field is
    /// malformed or the signature is invalid.
    pub fn verify(&self, verifying_key: &VerifyingKey) -> bool {
        self.verify_with_context(verifying_key, STH_DOMAIN)
    }

    /// Verify this STH's signature against `verifying_key` under an explicit
    /// domain-separation `context`. An STH signed for one tenant's tree fails
    /// verification under another tenant's context, even with the same key.
    pub fn verify_with_context(&self, verifying_key: &VerifyingKey, context: &[u8]) -> bool {
        let root = match self.root_bytes() {
            Ok(r) => r,
            Err(_) => return false,
        };
        let signature = match sig_from_hex(&self.signature) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let message = signing_message_with_context(context, self.tree_size, &root, self.timestamp);
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

#[cfg(test)]
mod tests {
    use super::*;

    const TS: u64 = 1_700_000_000_000;
    const OTHER_CONTEXT: &[u8] = b"pollis-verifiable-log:sth:v1:account-keys";

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn default_create_and_verify_roundtrip_unchanged() {
        let sth = Sth::create(&key(), 3, [1u8; 32], TS);
        let vk = key().verifying_key();
        assert!(sth.verify(&vk));
        // The default path is exactly the default context.
        assert!(sth.verify_with_context(&vk, STH_DOMAIN));
    }

    #[test]
    fn context_separated_sth_is_not_cross_verifiable() {
        let vk = key().verifying_key();
        // Same key, same (size, root, timestamp), but a different domain context:
        // the two signatures are over different preimages and must not be
        // interchangeable in either direction.
        let default_sth = Sth::create(&key(), 5, [9u8; 32], TS);
        let account_sth = Sth::create_with_context(&key(), 5, [9u8; 32], TS, OTHER_CONTEXT);

        assert_ne!(default_sth.signature, account_sth.signature);

        // Each verifies under its own context only.
        assert!(default_sth.verify(&vk));
        assert!(!default_sth.verify_with_context(&vk, OTHER_CONTEXT));

        assert!(account_sth.verify_with_context(&vk, OTHER_CONTEXT));
        assert!(!account_sth.verify(&vk));
    }
}
