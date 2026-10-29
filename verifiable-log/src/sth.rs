//! Signed Tree Head: an ML-DSA-44 signature over the tree's state.
//!
//! Post-quantum (#668) because an STH is the only thing an auditor trusts; a
//! retroactive forgery would let an operator present a rewritten history.

use ml_dsa::{EncodedSignature, EncodedVerifyingKey, Keypair, MlDsa44, Signer, Verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::hash::{from_hex, to_hex, Hash};

/// The log's signing key. An ML-DSA-44 private key is its 32-byte seed.
pub type SigningKey = ml_dsa::SigningKey<MlDsa44>;
/// The published verifying key, served hex-encoded in `public_key.json`.
pub type VerifyingKey = ml_dsa::VerifyingKey<MlDsa44>;
type Signature = ml_dsa::Signature<MlDsa44>;

/// Byte length of a hex-decoded [`VerifyingKey`].
pub const STH_PUB_LEN: usize = 1312;
/// Byte length of a hex-decoded STH signature.
pub const STH_SIG_LEN: usize = 2420;

/// Frozen domain-separation context for the commit-log tree (the default).
///
/// `v2` is the ML-DSA-44 generation (#668); the Ed25519 `v1` contexts are
/// retired, never reused. Each tree signs under its own context so one log's
/// STH can't be replayed as another's. All contexts are defined only here: a
/// drifted context is indistinguishable from a forged head.
pub const STH_CONTEXT_COMMIT_LOG: &[u8] = b"pollis-verifiable-log:sth:v2";

/// Context for the account-key directory tree. See [`STH_CONTEXT_COMMIT_LOG`].
pub const STH_CONTEXT_ACCOUNT_KEYS: &[u8] = b"pollis-verifiable-log:sth:v2:account-keys";

/// Context for the released-binaries tree. See [`STH_CONTEXT_COMMIT_LOG`].
pub const STH_CONTEXT_BINARIES: &[u8] = b"pollis-verifiable-log:sth:v2:binaries";

/// Every published tree, as `(cli-name, context)`: the one list a verifier maps
/// a tree name through.
pub const STH_CONTEXTS: [(&str, &[u8]); 3] = [
    ("commit-log", STH_CONTEXT_COMMIT_LOG),
    ("account-keys", STH_CONTEXT_ACCOUNT_KEYS),
    ("binaries", STH_CONTEXT_BINARIES),
];

/// The context for a tree name, or `None` if unknown. Never falls back to the
/// commit log: the wrong domain reports a confident FAIL for an honest log.
pub fn sth_context_for_tree(name: &str) -> Option<&'static [u8]> {
    STH_CONTEXTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, ctx)| *ctx)
}

const STH_DOMAIN: &[u8] = STH_CONTEXT_COMMIT_LOG;

/// Length in hex characters of a [`key_id_for`] identifier.
pub const KEY_ID_HEX_LEN: usize = 16;

/// A stable key identifier: the first 8 bytes of `SHA-256(encoded_key)`, hex.
///
/// A selection hint, never a trust input: the signature must still verify, so
/// a wrong id costs only extra verifications. Hence [`Sth::key_id`] is outside
/// the signed preimage.
pub fn key_id_for(verifying_key: &VerifyingKey) -> String {
    let digest = Sha256::digest(verifying_key.encode());
    hex::encode(&digest[..8])
}

/// A Signed Tree Head. Frozen wire shape (`README.md`); binary fields are
/// lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sth {
    pub tree_size: u64,
    pub root_hash: String,
    /// Caller-supplied timestamp (milliseconds since epoch, by convention).
    pub timestamp: u64,
    pub signature: String,
    /// Which key signed this head ([`key_id_for`]). Unsigned, so only a hint:
    /// verifiers try it first, then every other pinned key. Omitted when absent
    /// so older heads stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
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
    /// Sign a tree head under the frozen commit-log context.
    pub fn create(signing_key: &SigningKey, tree_size: u64, root: Hash, timestamp: u64) -> Self {
        Self::create_with_context(signing_key, tree_size, root, timestamp, STH_DOMAIN)
    }

    /// Sign a tree head under an explicit domain-separation `context`.
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
            key_id: Some(key_id_for(&signing_key.verifying_key())),
        }
    }

    /// [`Self::verify_any_with_context`] under the commit-log context.
    pub fn verify_any<'k>(
        &self,
        candidates: &'k [(String, VerifyingKey)],
    ) -> Option<&'k (String, VerifyingKey)> {
        self.verify_any_with_context(candidates, STH_DOMAIN)
    }

    /// Verify against a set of candidate keys (the rotation overlap), returning
    /// the one that validated. [`Self::key_id`] only reorders the attempts, so a
    /// hostile id can never change the verdict.
    pub fn verify_any_with_context<'k>(
        &self,
        candidates: &'k [(String, VerifyingKey)],
        context: &[u8],
    ) -> Option<&'k (String, VerifyingKey)> {
        let hinted = self
            .key_id
            .as_deref()
            .and_then(|id| candidates.iter().find(|(kid, _)| kid == id));
        if let Some(c) = hinted {
            if self.verify_with_context(&c.1, context) {
                return Some(c);
            }
        }
        candidates
            .iter()
            .find(|c| self.verify_with_context(&c.1, context))
    }

    pub fn root_bytes(&self) -> Result<Hash> {
        from_hex(&self.root_hash)
    }

    /// Verify under the commit-log context. `false`, never a panic, on any
    /// malformed field.
    pub fn verify(&self, verifying_key: &VerifyingKey) -> bool {
        self.verify_with_context(verifying_key, STH_DOMAIN)
    }

    pub fn verify_with_context(&self, verifying_key: &VerifyingKey, context: &[u8]) -> bool {
        let Ok(root) = self.root_bytes() else {
            return false;
        };
        let Ok(signature) = sig_from_hex(&self.signature) else {
            return false;
        };
        let message = signing_message_with_context(context, self.tree_size, &root, self.timestamp);
        verifying_key.verify(&message, &signature).is_ok()
    }
}

/// Same `tree_size`, different roots: the log signed two conflicting views of
/// history. The caller must have verified both signatures.
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

/// Parse a 32-byte hex seed into the log's [`SigningKey`].
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

    /// The website's JS checker hard-codes these lengths.
    #[test]
    fn published_key_and_signature_are_ml_dsa_44_sized() {
        let sth = Sth::create(&key(), 3, [1u8; 32], TS);
        assert_eq!(sth.signature.len(), STH_SIG_LEN * 2);
        let pub_hex = hex::encode(key().verifying_key().encode());
        assert_eq!(pub_hex.len(), STH_PUB_LEN * 2);
        assert!(verifying_key_from_hex(&pub_hex).is_ok());
    }

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
        assert!(sth.verify_with_context(&vk, STH_DOMAIN));
    }

    #[test]
    fn context_separated_sth_is_not_cross_verifiable() {
        let vk = key().verifying_key();
        let default_sth = Sth::create(&key(), 5, [9u8; 32], TS);
        let account_sth = Sth::create_with_context(&key(), 5, [9u8; 32], TS, OTHER_CONTEXT);

        assert_ne!(default_sth.signature, account_sth.signature);

        assert!(default_sth.verify(&vk));
        assert!(!default_sth.verify_with_context(&vk, OTHER_CONTEXT));

        assert!(account_sth.verify_with_context(&vk, OTHER_CONTEXT));
        assert!(!account_sth.verify(&vk));
    }

    fn key_b() -> SigningKey {
        SigningKey::from_seed(&[9u8; 32].into())
    }

    fn candidates() -> Vec<(String, VerifyingKey)> {
        vec![
            (key_id_for(&key().verifying_key()), key().verifying_key()),
            (key_id_for(&key_b().verifying_key()), key_b().verifying_key()),
        ]
    }

    #[test]
    fn key_ids_are_derived_stable_and_distinct() {
        let a = key_id_for(&key().verifying_key());
        assert_eq!(a.len(), KEY_ID_HEX_LEN);
        assert_eq!(a, key_id_for(&key().verifying_key()));
        assert_ne!(a, key_id_for(&key_b().verifying_key()));
    }

    /// Per-size heads are immutable: a spurious `"key_id": null` would rewrite
    /// attested history.
    #[test]
    fn pre_key_set_sth_roundtrips_without_the_field() {
        let wire = r#"{"tree_size":3,"root_hash":"0101010101010101010101010101010101010101010101010101010101010101","timestamp":1700000000000,"signature":"00"}"#;
        let sth: Sth = serde_json::from_str(wire).expect("legacy STH must deserialize");
        assert_eq!(sth.key_id, None);
        let out = serde_json::to_string(&sth).unwrap();
        assert!(!out.contains("key_id"), "absent key_id must not be emitted");
    }

    #[test]
    fn verify_any_accepts_a_head_from_either_pinned_key() {
        let from_a = Sth::create(&key(), 5, [9u8; 32], TS);
        let from_b = Sth::create(&key_b(), 5, [9u8; 32], TS);
        let c = candidates();

        let hit_a = from_a.verify_any_with_context(&c, STH_DOMAIN).expect("A");
        assert_eq!(hit_a.0, key_id_for(&key().verifying_key()));

        let hit_b = from_b.verify_any_with_context(&c, STH_DOMAIN).expect("B");
        assert_eq!(hit_b.0, key_id_for(&key_b().verifying_key()));
    }

    #[test]
    fn a_lying_key_id_changes_nothing() {
        let c = candidates();
        let real = key_id_for(&key().verifying_key());

        let mut points_at_wrong_key = Sth::create(&key(), 5, [9u8; 32], TS);
        points_at_wrong_key.key_id = Some(key_id_for(&key_b().verifying_key()));
        assert_eq!(
            points_at_wrong_key
                .verify_any_with_context(&c, STH_DOMAIN)
                .map(|h| h.0.clone()),
            Some(real.clone())
        );

        let mut points_at_nothing = Sth::create(&key(), 5, [9u8; 32], TS);
        points_at_nothing.key_id = Some("ffffffffffffffff".into());
        assert_eq!(
            points_at_nothing
                .verify_any_with_context(&c, STH_DOMAIN)
                .map(|h| h.0.clone()),
            Some(real.clone())
        );

        let mut absent = Sth::create(&key(), 5, [9u8; 32], TS);
        absent.key_id = None;
        assert_eq!(
            absent
                .verify_any_with_context(&c, STH_DOMAIN)
                .map(|h| h.0.clone()),
            Some(real)
        );
    }

    #[test]
    fn verify_any_rejects_a_head_from_an_unpinned_key() {
        let stranger = SigningKey::from_seed(&[42u8; 32].into());
        let mut forged = Sth::create(&stranger, 5, [9u8; 32], TS);
        forged.key_id = Some(key_id_for(&key().verifying_key()));
        assert!(forged
            .verify_any_with_context(&candidates(), STH_DOMAIN)
            .is_none());
    }

    #[test]
    fn verify_any_still_honours_domain_separation() {
        let account = Sth::create_with_context(&key(), 5, [9u8; 32], TS, OTHER_CONTEXT);
        assert!(account
            .verify_any_with_context(&candidates(), STH_DOMAIN)
            .is_none());
        assert!(account
            .verify_any_with_context(&candidates(), OTHER_CONTEXT)
            .is_some());
    }

    #[test]
    fn binaries_context_is_separated_from_the_other_trees() {
        let vk = key().verifying_key();
        let commit_sth = Sth::create(&key(), 5, [9u8; 32], TS);
        let account_sth = Sth::create_with_context(&key(), 5, [9u8; 32], TS, OTHER_CONTEXT);
        let binaries_sth = Sth::create_with_context(&key(), 5, [9u8; 32], TS, BINARIES_CONTEXT);

        assert_ne!(binaries_sth.signature, commit_sth.signature);
        assert_ne!(binaries_sth.signature, account_sth.signature);

        assert!(binaries_sth.verify_with_context(&vk, BINARIES_CONTEXT));
        assert!(!binaries_sth.verify(&vk));
        assert!(!binaries_sth.verify_with_context(&vk, OTHER_CONTEXT));

        assert!(!commit_sth.verify_with_context(&vk, BINARIES_CONTEXT));
        assert!(!account_sth.verify_with_context(&vk, BINARIES_CONTEXT));
    }
}
