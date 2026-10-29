//! Signed Tree Head (STH) — an ML-DSA-44 signature over the tree's state at a
//! point in time; proofs are checked relative to an STH root.

use ml_dsa::{EncodedSignature, EncodedVerifyingKey, Keypair, MlDsa44, Signer, Verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::hash::{from_hex, to_hex, Hash};

/// The log's signing key. An ML-DSA-44 private key IS its 32-byte seed.
pub type SigningKey = ml_dsa::SigningKey<MlDsa44>;
/// The published verifying key, served hex-encoded in `public_key.json`.
pub type VerifyingKey = ml_dsa::VerifyingKey<MlDsa44>;
type Signature = ml_dsa::Signature<MlDsa44>;

/// Byte length of a hex-decoded [`VerifyingKey`].
pub const STH_PUB_LEN: usize = 1312;
/// Byte length of a hex-decoded STH signature.
pub const STH_SIG_LEN: usize = 2420;

/// Default domain-separation tag for the signed message, so an STH signature can
/// never be confused with a signature over anything else. Frozen for the commit-log tree.
pub const STH_CONTEXT_COMMIT_LOG: &[u8] = b"pollis-verifiable-log:sth:v2";

/// Domain separation for the **account-key directory** tree.
pub const STH_CONTEXT_ACCOUNT_KEYS: &[u8] = b"pollis-verifiable-log:sth:v2:account-keys";

/// Domain separation for the **released-binaries** tree (binary transparency).
pub const STH_CONTEXT_BINARIES: &[u8] = b"pollis-verifiable-log:sth:v2:binaries";

/// Domain separation for the **second commit-log tree** (#1579).
/// Distinct from the retired first tree so heads can never cross-verify.
pub const STH_CONTEXT_COMMIT_LOG_2: &[u8] = b"pollis-verifiable-log:sth:v2:commit-log-2";

/// Every published tree, as `(cli-name, context)`; `commit-log` is retired, `commit-log-2` live.
pub const STH_CONTEXTS: [(&str, &[u8]); 4] = [
    ("commit-log", STH_CONTEXT_COMMIT_LOG),
    ("commit-log-2", STH_CONTEXT_COMMIT_LOG_2),
    ("account-keys", STH_CONTEXT_ACCOUNT_KEYS),
    ("binaries", STH_CONTEXT_BINARIES),
];

/// The key-set context that authorises signers for `context`.
/// The successor tree inherits the commit-log tenant authority (#754).
pub fn key_set_authority_for(context: &[u8]) -> &[u8] {
    if context == STH_CONTEXT_COMMIT_LOG_2 {
        STH_CONTEXT_COMMIT_LOG
    } else {
        context
    }
}

/// The signing context for a tree name from [`STH_CONTEXTS`], or `None` for a
/// name this build does not know, never a silent fallback to the commit log.
pub fn sth_context_for_tree(name: &str) -> Option<&'static [u8]> {
    STH_CONTEXTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, ctx)| *ctx)
}

const STH_DOMAIN: &[u8] = STH_CONTEXT_COMMIT_LOG;

/// Length in hex characters of a [`key_id_for`] identifier.
pub const KEY_ID_HEX_LEN: usize = 16;

/// A short, stable identifier for a verifying key: the first 8 bytes of
/// `SHA-256(encoded_key)`, lowercase hex.
/// A selection hint, never a trust input: it sits outside the signed preimage.
pub fn key_id_for(verifying_key: &VerifyingKey) -> String {
    let digest = Sha256::digest(verifying_key.encode());
    hex::encode(&digest[..8])
}

/// A Signed Tree Head. Wire shape (see `README.md`): all binary fields are
/// lowercase hex.
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
    /// Which key signed this head ([`key_id_for`]), so a verifier holding several
    /// pinned keys during a rotation overlap selects rather than trial-verifies.
    /// Unsigned hint; absent on older heads, which must stay byte-identical.
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
    /// Sign a tree head under the default ([`STH_DOMAIN`]) context. The signature
    /// commits to size, root and timestamp together; its bytes must not change.
    pub fn create(signing_key: &SigningKey, tree_size: u64, root: Hash, timestamp: u64) -> Self {
        Self::create_with_context(signing_key, tree_size, root, timestamp, STH_DOMAIN)
    }

    /// Sign a tree head under an explicit domain-separation `context`. A second
    /// tenant signs with its own context so heads cannot cross between logs.
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

    /// [`Self::verify_any_with_context`] under the default ([`STH_DOMAIN`])
    /// context — the commit-log tree.
    pub fn verify_any<'k>(
        &self,
        candidates: &'k [(String, VerifyingKey)],
    ) -> Option<&'k (String, VerifyingKey)> {
        self.verify_any_with_context(candidates, STH_DOMAIN)
    }

    /// Verify against a **set** of candidate keys, returning the one that
    /// validated. A wrong or hostile `key_id` only reorders attempts.
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
    /// domain-separation `context`.
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

/// Two STHs that commit to the same `tree_size` but different roots are proof
/// of equivocation.
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
/// IS its seed.
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

    /// Wire fields are exactly ML-DSA-44 sized (the website JS checker hard-codes them).
    #[test]
    fn published_key_and_signature_are_ml_dsa_44_sized() {
        let sth = Sth::create(&key(), 3, [1u8; 32], TS);
        assert_eq!(sth.signature.len(), STH_SIG_LEN * 2);
        let pub_hex = hex::encode(key().verifying_key().encode());
        assert_eq!(pub_hex.len(), STH_PUB_LEN * 2);
        assert!(verifying_key_from_hex(&pub_hex).is_ok());
    }

    /// Ed25519-era keys and signatures are rejected on length.
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
        // Same key and tree state, different context: neither may verify as the other.
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

    /// Heads published before the key set existed carry no `key_id`. They must
    /// still verify and re-serialize byte-identically (heads are immutable).
    #[test]
    fn pre_key_set_sth_roundtrips_without_the_field() {
        let wire = r#"{"tree_size":3,"root_hash":"0101010101010101010101010101010101010101010101010101010101010101","timestamp":1700000000000,"signature":"00"}"#;
        let sth: Sth = serde_json::from_str(wire).expect("legacy STH must deserialize");
        assert_eq!(sth.key_id, None);
        let out = serde_json::to_string(&sth).unwrap();
        assert!(!out.contains("key_id"), "absent key_id must not be emitted");
    }

    /// Rotation overlap: a head signed by either valid key verifies.
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

    /// `key_id` is a hint outside the signed preimage, so tampering with it must
    /// not change any verdict.
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

    /// A head from an unpinned key verifies under none, whatever its `key_id`.
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

    /// #1579: a head of the retired commit-log tree and a head of its successor
    /// never verify as each other, even over the same size and root.
    #[test]
    fn the_second_commit_log_tree_is_separated_from_the_first() {
        let vk = key().verifying_key();
        let v1 = Sth::create(&key(), 7, [4u8; 32], TS);
        let v2 = Sth::create_with_context(&key(), 7, [4u8; 32], TS, STH_CONTEXT_COMMIT_LOG_2);
        assert!(v1.verify(&vk));
        assert!(!v1.verify_with_context(&vk, STH_CONTEXT_COMMIT_LOG_2));
        assert!(v2.verify_with_context(&vk, STH_CONTEXT_COMMIT_LOG_2));
        assert!(!v2.verify(&vk));
        assert_eq!(sth_context_for_tree("commit-log-2"), Some(STH_CONTEXT_COMMIT_LOG_2));
        assert_eq!(key_set_authority_for(STH_CONTEXT_COMMIT_LOG_2), STH_CONTEXT_COMMIT_LOG);
        assert_eq!(key_set_authority_for(STH_CONTEXT_BINARIES), STH_CONTEXT_BINARIES);
    }

    #[test]
    fn binaries_context_is_separated_from_the_other_trees() {
        let vk = key().verifying_key();
        // Same key and tree state across three trees: each verifies only under its own context.
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
