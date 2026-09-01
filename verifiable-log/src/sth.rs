//! Signed Tree Head (STH) — an ML-DSA-44 signature over the tree's state at a
//! point in time. This is the log's accountable commitment: a monitor that
//! collects STHs can detect equivocation, and inclusion/consistency proofs are
//! checked relative to an STH's root.
//!
//! The scheme is post-quantum (#668) because an STH is the *only* thing an
//! auditor trusts: forging one retroactively would let an operator present a
//! rewritten history that still verifies. A signature that has to be sound
//! decades after it was minted cannot be Ed25519.

use ml_dsa::{EncodedSignature, EncodedVerifyingKey, Keypair, MlDsa44, Signer, Verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
/// replayed as another's. One key, three trees, three contexts — all three live
/// here, next to the default, because a context string that is spelled out in
/// several crates is a context string that can drift, and a drifted context is
/// indistinguishable from a forged head. The tenant modules in
/// `verifiable-log-builder` re-export these rather than restating the bytes.
pub const STH_CONTEXT_COMMIT_LOG: &[u8] = b"pollis-verifiable-log:sth:v2";

/// Domain separation for the **account-key directory** tree. See
/// [`STH_CONTEXT_COMMIT_LOG`].
pub const STH_CONTEXT_ACCOUNT_KEYS: &[u8] = b"pollis-verifiable-log:sth:v2:account-keys";

/// Domain separation for the **released-binaries** tree (binary transparency).
/// See [`STH_CONTEXT_COMMIT_LOG`].
pub const STH_CONTEXT_BINARIES: &[u8] = b"pollis-verifiable-log:sth:v2:binaries";

/// Every published tree, as `(cli-name, context)`. The single list a verifier
/// walks when it has to turn a human-supplied tree name into a signing context,
/// so adding a fourth tree cannot leave one entry point behind.
pub const STH_CONTEXTS: [(&str, &[u8]); 3] = [
    ("commit-log", STH_CONTEXT_COMMIT_LOG),
    ("account-keys", STH_CONTEXT_ACCOUNT_KEYS),
    ("binaries", STH_CONTEXT_BINARIES),
];

/// The signing context for a tree name from [`STH_CONTEXTS`], or `None` for a
/// name this build does not know — never a silent fallback to the commit log,
/// which would verify an account-key head under the wrong domain and report a
/// confident FAIL for an honest log.
pub fn sth_context_for_tree(name: &str) -> Option<&'static [u8]> {
    STH_CONTEXTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, ctx)| *ctx)
}

/// Internal alias for the frozen commit-log context, kept so the default-path
/// code below reads as "the default" rather than naming one tenant.
const STH_DOMAIN: &[u8] = STH_CONTEXT_COMMIT_LOG;

/// Length in hex characters of a [`key_id_for`] identifier.
pub const KEY_ID_HEX_LEN: usize = 16;

/// A short, stable identifier for a verifying key: the first 8 bytes of
/// `SHA-256(encoded_key)`, lowercase hex.
///
/// Derived rather than assigned, so there is no registry to keep in sync and no
/// way for two parties to disagree about which key an id names — anyone holding
/// the key can recompute it.
///
/// **This is a selection hint, never a trust input.** It tells a verifier which
/// of several pinned keys to try first; the signature still has to verify under
/// that key. A wrong or attacker-supplied id costs at most a few extra
/// verifications, which is why [`Sth::key_id`] is deliberately outside the
/// signed preimage.
pub fn key_id_for(verifying_key: &VerifyingKey) -> String {
    let digest = Sha256::digest(verifying_key.encode());
    hex::encode(&digest[..8])
}

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
    /// Which key signed this head ([`key_id_for`]), so a verifier holding several
    /// pinned keys during a rotation overlap selects rather than trial-verifies.
    ///
    /// Additive and **unsigned**: absent on every head published before the key
    /// set existed, and `skip_serializing_if` keeps those heads byte-identical
    /// when they are not re-signed. Because it is outside the signed preimage a
    /// verifier must treat it as a hint only — try the named key first, then fall
    /// back to every other non-expired pinned key before concluding anything.
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
    /// validated. This is the rotation-overlap path: during a changeover two keys
    /// are simultaneously valid, and a head is honest if it verifies under any of
    /// them. Only when it verifies under *none* is something actually wrong.
    ///
    /// [`Self::key_id`] just reorders the attempts — every candidate is tried
    /// regardless, so a wrong or hostile id can slow this down but can never make
    /// a bad head verify or a good one fail.
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
    /// domain-separation `context`. An STH signed for one tenant's tree fails
    /// verification under another tenant's context, even with the same key.
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

    fn key_b() -> SigningKey {
        SigningKey::from_seed(&[9u8; 32].into())
    }

    fn candidates() -> Vec<(String, VerifyingKey)> {
        vec![
            (key_id_for(&key().verifying_key()), key().verifying_key()),
            (key_id_for(&key_b().verifying_key()), key_b().verifying_key()),
        ]
    }

    /// A key id is derived from the key, so it is stable and unambiguous without
    /// any registry — and distinct keys get distinct ids.
    #[test]
    fn key_ids_are_derived_stable_and_distinct() {
        let a = key_id_for(&key().verifying_key());
        assert_eq!(a.len(), KEY_ID_HEX_LEN);
        assert_eq!(a, key_id_for(&key().verifying_key()));
        assert_ne!(a, key_id_for(&key_b().verifying_key()));
    }

    /// Heads published before the key set existed carry no `key_id`. They must
    /// still deserialize and verify — and re-serialize byte-identically, because
    /// per-size heads are immutable and a spurious `"key_id": null` would rewrite
    /// attested history.
    #[test]
    fn pre_key_set_sth_roundtrips_without_the_field() {
        let wire = r#"{"tree_size":3,"root_hash":"0101010101010101010101010101010101010101010101010101010101010101","timestamp":1700000000000,"signature":"00"}"#;
        let sth: Sth = serde_json::from_str(wire).expect("legacy STH must deserialize");
        assert_eq!(sth.key_id, None);
        let out = serde_json::to_string(&sth).unwrap();
        assert!(!out.contains("key_id"), "absent key_id must not be emitted");
    }

    /// The overlap case: two keys are simultaneously valid and a head signed by
    /// either one verifies. This is what makes a rotation invisible to clients.
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
    /// not change any verdict — a head with a wrong or unknown id still verifies
    /// under the key that actually signed it.
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

    /// The alarm case survives the key set: a head signed by a key that is in no
    /// pinned set verifies under none of them, however friendly its `key_id`.
    #[test]
    fn verify_any_rejects_a_head_from_an_unpinned_key() {
        let stranger = SigningKey::from_seed(&[42u8; 32].into());
        let mut forged = Sth::create(&stranger, 5, [9u8; 32], TS);
        forged.key_id = Some(key_id_for(&key().verifying_key()));
        assert!(forged
            .verify_any_with_context(&candidates(), STH_DOMAIN)
            .is_none());
    }

    /// Domain separation still binds across the key set: a head from a valid key
    /// but the wrong tree verifies under no candidate.
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
