//! Transparency-log account anchoring (#813, design §11.1).
//!
//! The handshake only proves a device is self-consistent; account keys can be
//! minted offline in a loop, which would make per-account rate limits bound
//! nothing. An anchor proves the account key was published in the account-key
//! transparency log.
//!
//! The client presents leaf bytes, an RFC-6962 inclusion proof, and the signed
//! tree head; the relay verifies with zero I/O (a per-connection fetch would put
//! it back in the metadata plane). Checks: the head verifies under the pinned key
//! in the account-keys domain, the proof reaches its root, and the leaf names
//! exactly the handshake's account. Anchors are public, so stealing one buys
//! nothing without that account's device cert.
//!
//! Not proven: that `identity_version` is current (that needs the whole tree).
//! `max_age_secs` bounds head staleness, not key rotation.
//!
//! Off by default ([`AnchorPolicy`]): publish first, enforce second.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use verifiable_log::{verify_inclusion_proof, verifying_key_from_hex, Entry, InclusionProof, Sth};

/// Mirrors `verifiable_log_builder::account_key::TENANT` (not imported: that
/// crate pulls in libsql). The golden-vector test pins it.
pub const ACCOUNT_KEY_TENANT: &str = "account-key";

/// STH context for the account-keys tree. One key signs three trees, so without
/// it a genuine head from another tree could anchor an account proof.
pub use verifiable_log::STH_CONTEXT_ACCOUNT_KEYS as ACCOUNT_KEYS_STH_CONTEXT;

/// Max head age. The tree publishes daily; seven days rides out a publishing
/// outage without locking the fleet out.
pub const DEFAULT_ANCHOR_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

/// Future skew tolerated on a head's timestamp, same as the handshake.
const MAX_FUTURE_SKEW_SECS: u64 = crate::proto::MAX_SKEW_SECS as u64;

/// The account-key leaf, frozen: compact JSON in field declaration order.
/// `account_id_pub` is lowercase hex of the ML-DSA-44 account key.
///
/// Parsed only to bind it to the handshake; the hashed bytes are the client's
/// own, never a re-serialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyLeaf {
    pub user_id: String,
    pub identity_version: u64,
    pub account_id_pub: String,
    pub seq: i64,
}

/// A client-presented proof that its account key is in the transparency log.
/// `tree_size` is carried once because head and proof must agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountAnchor {
    pub leaf_bytes: Vec<u8>,
    pub leaf_index: u64,
    pub tree_size: u64,
    /// Sibling hashes, bottom-up (RFC 6962 audit path).
    pub audit_path: Vec<[u8; 32]>,
    pub sth_root: [u8; 32],
    pub sth_timestamp_ms: u64,
    pub sth_signature: Vec<u8>,
}

impl AccountAnchor {
    fn sth(&self) -> Sth {
        Sth {
            tree_size: self.tree_size,
            root_hash: hex::encode(self.sth_root),
            timestamp: self.sth_timestamp_ms,
            signature: hex::encode(&self.sth_signature),
            // Unsigned hint; the relay pins exactly one key.
            key_id: None,
        }
    }

    fn proof(&self) -> InclusionProof {
        InclusionProof {
            leaf_index: self.leaf_index,
            tree_size: self.tree_size,
            audit_path: self.audit_path.iter().map(hex::encode).collect(),
        }
    }

    fn entry(&self) -> Entry {
        Entry::new(ACCOUNT_KEY_TENANT, self.leaf_bytes.clone())
    }

    fn leaf(&self) -> Result<AccountKeyLeaf, AnchorError> {
        serde_json::from_slice(&self.leaf_bytes).map_err(|_| AnchorError::MalformedLeaf)
    }
}

/// Why an anchor was refused. Every variant is a refusal; there is no
/// "couldn't tell, allow it".
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnchorError {
    #[error("the pinned transparency-log key is not a valid ML-DSA-44 public key")]
    BadPinnedKey,
    #[error("leaf bytes are not a well-formed account-key leaf")]
    MalformedLeaf,
    #[error("leaf does not name the account this handshake presented")]
    LeafMismatch,
    #[error("signed tree head does not verify under the pinned key in the account-keys domain")]
    BadTreeHead,
    #[error("signed tree head is outside the accepted age window")]
    StaleTreeHead,
    #[error("inclusion proof does not carry this leaf into the head's root")]
    NotIncluded,
}

/// A configured, pinned account-anchor verifier. Pure: bytes in, verdict out.
pub struct AnchorVerifier {
    verifying_key: verifiable_log::VerifyingKey,
    pub max_age_secs: u64,
}

impl AnchorVerifier {
    /// Pin `log_pubkey_hex`, the account-key tree's ML-DSA-44 public key.
    pub fn new(log_pubkey_hex: &str, max_age_secs: u64) -> Result<AnchorVerifier, AnchorError> {
        let verifying_key =
            verifying_key_from_hex(log_pubkey_hex.trim()).map_err(|_| AnchorError::BadPinnedKey)?;
        Ok(AnchorVerifier {
            verifying_key,
            max_age_secs,
        })
    }

    /// Verify `anchor` against the handshake's account. Cheapest checks first,
    /// so a flood is dropped before the ML-DSA-44 verify.
    pub fn verify(
        &self,
        anchor: &AccountAnchor,
        user_id: &str,
        account_id_pub: &[u8],
        identity_version: u32,
        now_secs: i64,
    ) -> Result<(), AnchorError> {
        // Anchors are public, so one not bound to this handshake proves nothing.
        let leaf = anchor.leaf()?;
        if leaf.user_id != user_id
            || leaf.identity_version != u64::from(identity_version)
            || leaf.account_id_pub != hex::encode(account_id_pub)
        {
            return Err(AnchorError::LeafMismatch);
        }

        let head_secs = anchor.sth_timestamp_ms / 1000;
        let now = now_secs.max(0) as u64;
        if head_secs > now.saturating_add(MAX_FUTURE_SKEW_SECS) {
            return Err(AnchorError::StaleTreeHead);
        }
        if now.saturating_sub(head_secs) > self.max_age_secs {
            return Err(AnchorError::StaleTreeHead);
        }

        let sth = anchor.sth();
        if !sth.verify_with_context(&self.verifying_key, ACCOUNT_KEYS_STH_CONTEXT) {
            return Err(AnchorError::BadTreeHead);
        }

        if !verify_inclusion_proof(&anchor.entry(), &anchor.proof(), &sth) {
            return Err(AnchorError::NotIncluded);
        }
        Ok(())
    }
}

impl std::fmt::Debug for AnchorVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnchorVerifier")
            .field("key_id", &verifiable_log::key_id_for(&self.verifying_key))
            .field("max_age_secs", &self.max_age_secs)
            .finish()
    }
}

/// What this node does about account anchoring. `Require` holds a verifier, so
/// "require without a key" is unrepresentable.
#[derive(Debug, Clone, Default)]
pub enum AnchorPolicy {
    /// No key: no anchoring claim, and a presented anchor is not rejected.
    #[default]
    Ignore,
    /// A bad anchor is an auth failure; a client presenting none is served.
    Verify(Arc<AnchorVerifier>),
    /// A client presenting none gets [`crate::proto::RejectReason::AnchorRequired`].
    Require(Arc<AnchorVerifier>),
}

impl AnchorPolicy {
    pub fn configured(
        log_pubkey_hex: &str,
        max_age_secs: u64,
        require: bool,
    ) -> Result<AnchorPolicy, AnchorError> {
        let verifier = Arc::new(AnchorVerifier::new(log_pubkey_hex, max_age_secs)?);
        if require {
            Ok(AnchorPolicy::Require(verifier))
        } else {
            Ok(AnchorPolicy::Verify(verifier))
        }
    }

    pub fn verifier(&self) -> Option<&AnchorVerifier> {
        match self {
            AnchorPolicy::Ignore => None,
            AnchorPolicy::Verify(v) | AnchorPolicy::Require(v) => Some(v),
        }
    }

    pub fn is_required(&self) -> bool {
        matches!(self, AnchorPolicy::Require(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use verifiable_log::{SigningKey, VerifiableLog};

    const NOW: i64 = 1_800_000_000;
    const USER: &str = "u_anchor";

    fn log_key(seed: u8) -> SigningKey {
        SigningKey::from_seed(&[seed; 32].into())
    }

    fn pinned(sk: &SigningKey) -> String {
        use ml_dsa::Keypair;
        hex::encode(sk.verifying_key().encode())
    }

    fn leaf_bytes(user_id: &str, identity_version: u64, account_pub: &[u8], seq: i64) -> Vec<u8> {
        serde_json::to_vec(&AccountKeyLeaf {
            user_id: user_id.to_string(),
            identity_version,
            account_id_pub: hex::encode(account_pub),
            seq,
        })
        .unwrap()
    }

    fn anchor_for(
        sk: &SigningKey,
        leaves: &[Vec<u8>],
        index: usize,
        context: &[u8],
        timestamp_ms: u64,
    ) -> AccountAnchor {
        let mut log = VerifiableLog::new();
        for bytes in leaves {
            log.append(Entry::new(ACCOUNT_KEY_TENANT, bytes.clone())).unwrap();
        }
        let size = log.size() as u64;
        let sth = Sth::create_with_context(sk, size, log.root(), timestamp_ms, context);
        let proof = log.inclusion_proof(index).unwrap();
        let mut audit_path = Vec::new();
        for hexed in &proof.audit_path {
            let raw = hex::decode(hexed).unwrap();
            audit_path.push(<[u8; 32]>::try_from(raw.as_slice()).unwrap());
        }
        AccountAnchor {
            leaf_bytes: leaves[index].clone(),
            leaf_index: proof.leaf_index,
            tree_size: size,
            audit_path,
            sth_root: <[u8; 32]>::try_from(hex::decode(&sth.root_hash).unwrap().as_slice()).unwrap(),
            sth_timestamp_ms: timestamp_ms,
            sth_signature: hex::decode(&sth.signature).unwrap(),
        }
    }

    /// Pinned byte-for-byte: `verifiable-log-builder` can't be a dev-dep here
    /// (libsql), so this is what catches a drift in the account tenant.
    #[test]
    fn account_key_leaf_encoding_is_frozen() {
        let bytes = leaf_bytes("u_123", 2, &[0xab, 0xcd], 7);
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"user_id":"u_123","identity_version":2,"account_id_pub":"abcd","seq":7}"#
        );
        let entry = Entry::new(ACCOUNT_KEY_TENANT, bytes.clone());
        let encoded = entry.encode();
        assert_eq!(&encoded[..4], &(ACCOUNT_KEY_TENANT.len() as u32).to_be_bytes());
        assert_eq!(&encoded[4..4 + ACCOUNT_KEY_TENANT.len()], b"account-key");
        assert_eq!(&encoded[4 + ACCOUNT_KEY_TENANT.len()..], &bytes[..]);
    }

    #[test]
    fn a_genuine_anchor_verifies() {
        let sk = log_key(11);
        let account = vec![0x01, 0x02, 0x03];
        let leaves = vec![
            leaf_bytes("someone_else", 1, &[0xff], 1),
            leaf_bytes(USER, 3, &account, 2),
            leaf_bytes("another", 1, &[0xee], 3),
        ];
        let anchor = anchor_for(&sk, &leaves, 1, ACCOUNT_KEYS_STH_CONTEXT, NOW as u64 * 1000);
        let verifier = AnchorVerifier::new(&pinned(&sk), DEFAULT_ANCHOR_MAX_AGE_SECS).unwrap();
        verifier.verify(&anchor, USER, &account, 3, NOW).unwrap();
    }

    #[test]
    fn an_anchor_for_a_different_account_key_is_refused() {
        let sk = log_key(11);
        let account = vec![0x01, 0x02, 0x03];
        let leaves = vec![leaf_bytes(USER, 3, &account, 1)];
        let anchor = anchor_for(&sk, &leaves, 0, ACCOUNT_KEYS_STH_CONTEXT, NOW as u64 * 1000);
        let verifier = AnchorVerifier::new(&pinned(&sk), DEFAULT_ANCHOR_MAX_AGE_SECS).unwrap();

        assert_eq!(
            verifier.verify(&anchor, USER, &[0x09, 0x09], 3, NOW),
            Err(AnchorError::LeafMismatch)
        );
        assert_eq!(
            verifier.verify(&anchor, "someone_else", &account, 3, NOW),
            Err(AnchorError::LeafMismatch)
        );
        assert_eq!(
            verifier.verify(&anchor, USER, &account, 4, NOW),
            Err(AnchorError::LeafMismatch)
        );
    }

    #[test]
    fn a_head_from_another_tree_cannot_anchor_an_account() {
        let sk = log_key(11);
        let account = vec![0x01];
        let leaves = vec![leaf_bytes(USER, 1, &account, 1)];
        // Right key and root, but the commit-log context.
        let anchor = anchor_for(
            &sk,
            &leaves,
            0,
            b"pollis-verifiable-log:sth:v2",
            NOW as u64 * 1000,
        );
        let verifier = AnchorVerifier::new(&pinned(&sk), DEFAULT_ANCHOR_MAX_AGE_SECS).unwrap();
        assert_eq!(
            verifier.verify(&anchor, USER, &account, 1, NOW),
            Err(AnchorError::BadTreeHead)
        );
    }

    #[test]
    fn a_head_signed_by_another_key_is_refused() {
        let real = log_key(11);
        let attacker = log_key(12);
        let account = vec![0x01];
        let leaves = vec![leaf_bytes(USER, 1, &account, 1)];
        let anchor = anchor_for(&attacker, &leaves, 0, ACCOUNT_KEYS_STH_CONTEXT, NOW as u64 * 1000);
        let verifier = AnchorVerifier::new(&pinned(&real), DEFAULT_ANCHOR_MAX_AGE_SECS).unwrap();
        assert_eq!(
            verifier.verify(&anchor, USER, &account, 1, NOW),
            Err(AnchorError::BadTreeHead)
        );
    }

    #[test]
    fn a_tampered_audit_path_or_leaf_breaks_inclusion() {
        let sk = log_key(11);
        let account = vec![0x01];
        let leaves = vec![
            leaf_bytes("first", 1, &[0xaa], 1),
            leaf_bytes(USER, 1, &account, 2),
        ];
        let good = anchor_for(&sk, &leaves, 1, ACCOUNT_KEYS_STH_CONTEXT, NOW as u64 * 1000);
        let verifier = AnchorVerifier::new(&pinned(&sk), DEFAULT_ANCHOR_MAX_AGE_SECS).unwrap();
        verifier.verify(&good, USER, &account, 1, NOW).unwrap();

        // The head still verifies, so only the merkle step can catch this.
        let mut tampered = good.clone();
        tampered.audit_path[0][0] ^= 0xff;
        assert_eq!(
            verifier.verify(&tampered, USER, &account, 1, NOW),
            Err(AnchorError::NotIncluded)
        );

        let mut reindexed = good.clone();
        reindexed.leaf_index = 0;
        assert_eq!(
            verifier.verify(&reindexed, USER, &account, 1, NOW),
            Err(AnchorError::NotIncluded)
        );
    }

    #[test]
    fn an_unpublished_account_cannot_borrow_a_genuine_head() {
        let sk = log_key(11);
        let real = vec![0x01];
        let leaves = vec![leaf_bytes("published_user", 1, &real, 1)];
        let genuine = anchor_for(&sk, &leaves, 0, ACCOUNT_KEYS_STH_CONTEXT, NOW as u64 * 1000);
        let verifier = AnchorVerifier::new(&pinned(&sk), DEFAULT_ANCHOR_MAX_AGE_SECS).unwrap();

        let forged_account = vec![0xde, 0xad];
        let mut forged = genuine.clone();
        forged.leaf_bytes = leaf_bytes("attacker", 1, &forged_account, 1);
        assert_eq!(
            verifier.verify(&forged, "attacker", &forged_account, 1, NOW),
            Err(AnchorError::NotIncluded)
        );
    }

    #[test]
    fn a_stale_or_future_head_is_refused() {
        let sk = log_key(11);
        let account = vec![0x01];
        let leaves = vec![leaf_bytes(USER, 1, &account, 1)];
        let verifier = AnchorVerifier::new(&pinned(&sk), 3600).unwrap();

        let old = anchor_for(
            &sk,
            &leaves,
            0,
            ACCOUNT_KEYS_STH_CONTEXT,
            (NOW as u64 - 7200) * 1000,
        );
        assert_eq!(
            verifier.verify(&old, USER, &account, 1, NOW),
            Err(AnchorError::StaleTreeHead)
        );

        let future = anchor_for(
            &sk,
            &leaves,
            0,
            ACCOUNT_KEYS_STH_CONTEXT,
            (NOW as u64 + 100_000) * 1000,
        );
        assert_eq!(
            verifier.verify(&future, USER, &account, 1, NOW),
            Err(AnchorError::StaleTreeHead)
        );
    }

    #[test]
    fn malformed_leaves_and_keys_are_refused_not_skipped() {
        let sk = log_key(11);
        let leaves = vec![leaf_bytes(USER, 1, &[0x01], 1)];
        let mut anchor = anchor_for(&sk, &leaves, 0, ACCOUNT_KEYS_STH_CONTEXT, NOW as u64 * 1000);
        anchor.leaf_bytes = b"not a leaf".to_vec();
        let verifier = AnchorVerifier::new(&pinned(&sk), DEFAULT_ANCHOR_MAX_AGE_SECS).unwrap();
        assert_eq!(
            verifier.verify(&anchor, USER, &[0x01], 1, NOW),
            Err(AnchorError::MalformedLeaf)
        );

        assert_eq!(
            AnchorVerifier::new("not hex", 60).unwrap_err(),
            AnchorError::BadPinnedKey
        );
        assert_eq!(
            AnchorVerifier::new("abcd", 60).unwrap_err(),
            AnchorError::BadPinnedKey
        );
    }

    #[test]
    fn the_policy_ladder_cannot_require_without_a_key() {
        assert!(matches!(AnchorPolicy::default(), AnchorPolicy::Ignore));
        assert!(AnchorPolicy::default().verifier().is_none());
        assert!(!AnchorPolicy::default().is_required());

        let sk = log_key(11);
        let verify = AnchorPolicy::configured(&pinned(&sk), 60, false).unwrap();
        assert!(verify.verifier().is_some());
        assert!(!verify.is_required());

        let require = AnchorPolicy::configured(&pinned(&sk), 60, true).unwrap();
        assert!(require.verifier().is_some());
        assert!(require.is_required());

        assert!(AnchorPolicy::configured("zzz", 60, true).is_err());
    }
}
