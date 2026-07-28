//! Signed Tree Head (STH) — an ML-DSA-44 signature over the tree's state at a
//! point in time. This is the log's accountable commitment: a monitor that
//! collects STHs can detect equivocation, and inclusion/consistency proofs are
//! checked relative to an STH's root.
//!
//! The scheme is post-quantum (#668) because an STH is the *only* thing an
//! auditor trusts: forging one retroactively would let an operator present a
//! rewritten history that still verifies. A signature that has to be sound
//! decades after it was minted cannot be Ed25519.

use ml_dsa::{EncodedSignature, EncodedVerifyingKey, MlDsa44, Signer, Verifier};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::{from_hex, to_hex, Hash};

/// The log's signing key. An ML-DSA-44 private key IS its 32-byte seed, so key
/// custody (a 32-byte hex env var / key file) is unchanged from the Ed25519 era.
pub type SigningKey = ml_dsa::SigningKey<MlDsa44>;
/// The published verifying key — 1312 bytes, served hex-encoded in
/// `public_key.json`.
pub type VerifyingKey = ml_dsa::VerifyingKey<MlDsa44>;
type Signature = ml_dsa::Signature<MlDsa44>;

/// Byte length of a hex-decoded [`VerifyingKey`].
pub const STH_PUB_LEN: usize = 1312;
/// Byte length of a hex-decoded STH signature.
pub const STH_SIG_LEN: usize = 2420;

/// Default domain-separation tag for the signed message, so an STH signature can
/// never be confused with a signature over anything else. This is the **frozen**
/// context for the original (MLS commit-log) tree.
///
/// `v2` (#668) is the ML-DSA-44 generation. The `v1` contexts belong to the
/// Ed25519 era and are retired rather than reused: a v1 STH and a v2 STH over
/// the same tree state must be unmistakable for one another, and a verifier
/// holding a 1312-byte key must never even attempt a v1 preimage. Every tree is
/// republished under v2 — a rebuild from source data, not a re-signing.
///
/// A second (or third) tenant that wants its **own** tree must sign with a
/// *different* context via [`Sth::create_with_context`] /
/// [`Sth::verify_with_context`], so an STH minted for one log can never be
/// replayed as another's. The concrete context strings live next to their
/// tenants in the builder: the account-key directory signs under
/// `…:sth:v2:account-keys`, and the released-binaries tree (binary transparency)
/// under `…:sth:v2:binaries`. One key, three trees, three contexts.
const STH_DOMAIN: &[u8] = b"pollis-verifiable-log:sth:v2";

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
    /// ML-DSA-44 signature over the canonical message, hex-encoded (2420 bytes).
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

/// Parse a hex-encoded ML-DSA-44 public key ([`STH_PUB_LEN`] bytes).
pub fn verifying_key_from_hex(s: &str) -> Result<VerifyingKey> {
    let bytes = hex::decode(s)?;
    let encoded: &EncodedVerifyingKey<MlDsa44> = bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::BadPublicKeyLength(bytes.len()))?;
    Ok(VerifyingKey::decode(encoded))
}

/// Parse a 32-byte hex seed into the log's [`SigningKey`]. An ML-DSA private key
/// IS its seed, so this is the whole key — the same custody shape the Ed25519
/// era used.
pub fn signing_key_from_seed_hex(s: &str) -> Result<SigningKey> {
    let bytes = hex::decode(s.trim())?;
    let seed: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::BadPublicKeyLength(bytes.len()))?;
    Ok(SigningKey::from_seed(&seed.into()))
}

fn to_hex_sig(sig: &Signature) -> String {
    hex::encode(sig.encode())
}

fn sig_from_hex(s: &str) -> Result<Signature> {
    let bytes = hex::decode(s)?;
    let encoded: &EncodedSignature<MlDsa44> = bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::BadSignatureLength(bytes.len()))?;
    Signature::decode(encoded).ok_or(Error::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ml_dsa::Keypair;

    const TS: u64 = 1_700_000_000_000;
    const OTHER_CONTEXT: &[u8] = b"pollis-verifiable-log:sth:v2:account-keys";
    const BINARIES_CONTEXT: &[u8] = b"pollis-verifiable-log:sth:v2:binaries";

    fn key() -> SigningKey {
        SigningKey::from_seed(&[7u8; 32].into())
    }

    /// The published wire fields are exactly ML-DSA-44 sized. A verifier that
    /// hard-codes these lengths (the website's JS checker does) must not drift.
    #[test]
    fn published_key_and_signature_are_ml_dsa_44_sized() {
        let sth = Sth::create(&key(), 3, [1u8; 32], TS);
        assert_eq!(sth.signature.len(), STH_SIG_LEN * 2);
        let pub_hex = hex::encode(key().verifying_key().encode());
        assert_eq!(pub_hex.len(), STH_PUB_LEN * 2);
        assert!(verifying_key_from_hex(&pub_hex).is_ok());
    }

    /// An Ed25519-era key or signature can no longer be mistaken for a v2 one —
    /// both are rejected on length before any verification is attempted.
    #[test]
    fn ed25519_sized_key_and_signature_are_rejected() {
        assert!(verifying_key_from_hex(&hex::encode([0u8; 32])).is_err());
        let mut sth = Sth::create(&key(), 3, [1u8; 32], TS);
        sth.signature = hex::encode([0u8; 64]);
        assert!(!sth.verify(&key().verifying_key()));
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

    #[test]
    fn binaries_context_is_separated_from_the_other_trees() {
        let vk = key().verifying_key();
        // Same key + same (size, root, timestamp) across all three trees: each
        // STH verifies ONLY under its own domain context.
        let commit_sth = Sth::create(&key(), 5, [9u8; 32], TS);
        let account_sth = Sth::create_with_context(&key(), 5, [9u8; 32], TS, OTHER_CONTEXT);
        let binaries_sth = Sth::create_with_context(&key(), 5, [9u8; 32], TS, BINARIES_CONTEXT);

        // The three signatures are over three different preimages.
        assert_ne!(binaries_sth.signature, commit_sth.signature);
        assert_ne!(binaries_sth.signature, account_sth.signature);

        // The binaries head verifies under the binaries context only.
        assert!(binaries_sth.verify_with_context(&vk, BINARIES_CONTEXT));
        assert!(!binaries_sth.verify(&vk));
        assert!(!binaries_sth.verify_with_context(&vk, OTHER_CONTEXT));

        // ...and neither a commit-log nor an account head can stand in for it.
        assert!(!commit_sth.verify_with_context(&vk, BINARIES_CONTEXT));
        assert!(!account_sth.verify_with_context(&vk, BINARIES_CONTEXT));
    }
}
