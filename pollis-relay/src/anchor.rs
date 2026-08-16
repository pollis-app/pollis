//! **Transparency-log account-anchoring** (#813 Phase E2, design §11.1).
//!
//! # The gap this closes
//!
//! The v0 handshake proves a device is *cryptographically self-consistent*: the
//! connecting party holds a device key, and some account key certified it
//! ([`crate::proto::verify_handshake`]). It does **not** prove that account key
//! is a real, published Pollis account — an attacker can mint an account keypair
//! and a matching device cert entirely offline, in a loop.
//!
//! That matters for one concrete reason beyond tidiness: the rate limiter keys
//! per-account limits on `sha256(account_id_pub)`
//! ([`crate::proto::account_fingerprint`]). An adversary who can generate an
//! unlimited supply of self-consistent account keys has an unlimited supply of
//! *distinct* rate-limiting identities, so the per-account limits bound nothing.
//! Anchoring makes each identity cost a real signup that is publicly visible in
//! an append-only log.
//!
//! # How it is done without a fetch
//!
//! `docs/relay-operations.md` names the two candidate mechanisms — "either a
//! fetch or a client-presented inclusion proof" — and rules the first one out:
//! a relay that queries the log per connection is back inside the metadata
//! plane, which is the exact coupling this whole design removes (§11.1). So the
//! **client presents the proof** and the relay verifies it with zero I/O:
//!
//! 1. the leaf bytes — the account-key tenant's frozen `AccountKeyLeaf` encoding;
//! 2. an RFC-6962 inclusion proof for that leaf;
//! 3. the ML-DSA-44 **signed tree head** the proof is anchored against.
//!
//! The relay holds one pinned log public key, compiled in or configured, and
//! checks: the head verifies under it **in the account-keys domain**, the proof
//! carries the leaf into that head's root, and the leaf's
//! `(user_id, identity_version, account_id_pub)` are **exactly** the ones the
//! handshake presented. All of it is CPU: a signature verify plus a `log₂(N)`
//! SHA-256 audit path. No socket, no Turso, no DS.
//!
//! # What an anchor does and does not prove
//!
//! **Proves:** the account key this device chains to was published in the
//! account-key transparency log at a head the relay's pinned key signed. Combined
//! with the handshake (which proves possession of a device key that key
//! certified), the pair means "a device of a *published* account". Stealing
//! someone else's anchor buys nothing — anchors are public data, and without
//! that account's device cert the handshake still fails.
//!
//! **Does not prove:** that the presented `identity_version` is the account's
//! *current* one. Proving that needs the whole tree, which needs a fetch. A
//! rotated-away key keeps verifying, exactly as its device certs do — this is
//! the same "no live device revocation" limit already documented in
//! `docs/relay-operations.md`, not a new one. [`AnchorVerifier::max_age_secs`]
//! bounds how stale a *head* may be; it does not and cannot bound key rotation.
//!
//! # Rollout
//!
//! Enforcement is per-node and **off by default** ([`AnchorPolicy`]). A node
//! with no log key configured makes no anchoring claim at all; a keyed node
//! always verifies what is presented and treats a bad anchor as an auth failure;
//! only a node explicitly set to require one rejects a client that presents
//! none. Requiring anchors before clients ship the frame — or before a
//! just-signed-up account appears in the daily publish — is a self-inflicted
//! outage, so the mechanism ships enforceable and the policy is switched on
//! afterwards. Same publish-then-enforce ordering as relay revocation (#813
//! Phase C).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use verifiable_log::{verify_inclusion_proof, verifying_key_from_hex, Entry, InclusionProof, Sth};

/// The tenant name the account-key tree's entries carry.
///
/// Mirrors `verifiable_log_builder::account_key::TENANT`. It is duplicated
/// rather than imported because `verifiable-log-builder` carries a libsql
/// dependency and a Turso client has no business in a relay binary; the golden
/// vector in this module's tests pins the resulting leaf hash so a drift on
/// either side fails a test rather than silently un-anchoring everyone.
pub const ACCOUNT_KEY_TENANT: &str = "account-key";

/// The STH domain-separation context for the **account-keys** tree.
///
/// One log key signs three trees (commit log, account keys, binaries) and only
/// the context tells them apart, so verifying in the wrong context would let a
/// genuinely-signed head from a *different* tree anchor an account proof. This
/// is the same cross-artifact confusion guard `REVOCATION_TYPE` provides for the
/// directory and revocation list.
///
/// Re-exported from `verifiable-log` rather than restated: a copy of these bytes
/// that drifts does not fail loudly, it just stops agreeing with the tree.
pub use verifiable_log::STH_CONTEXT_ACCOUNT_KEYS as ACCOUNT_KEYS_STH_CONTEXT;

/// Default bound on how old a presented signed tree head may be, in seconds.
///
/// The account tree publishes daily, and a head is byte-stable forever once
/// published, so a tight bound buys little and a missed publish must not lock
/// the fleet out. Seven days keeps a cached anchor usable across a publishing
/// outage while still requiring the presenter to have met the live log at some
/// point in the recent past.
pub const DEFAULT_ANCHOR_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

/// Tolerated clock skew on a head's timestamp, in seconds. A head from the
/// future beyond this is rejected — the publisher's clock and ours should not
/// disagree by more than the handshake already tolerates.
const MAX_FUTURE_SKEW_SECS: u64 = crate::proto::MAX_SKEW_SECS as u64;

/// The account-key tree's leaf, in its **frozen** encoding.
///
/// The on-the-wire leaf is compact JSON of this struct with fields in exactly
/// this declared order (`user_id`, `identity_version`, `account_id_pub`, `seq`);
/// serde emits declaration order with no insignificant whitespace, so the
/// encoding is deterministic. `account_id_pub` is lowercase hex of the raw
/// 1312-byte ML-DSA-44 account identity key — the same bytes the handshake
/// carries in `DeviceCertMaterial::account_id_pub`.
///
/// The relay parses the leaf only to *bind* it to the handshake. The bytes it
/// hashes are the ones the client transmitted, never a re-serialization of this
/// struct, so a client that encodes the leaf slightly differently fails the
/// inclusion proof rather than silently anchoring to something else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyLeaf {
    pub user_id: String,
    pub identity_version: u64,
    pub account_id_pub: String,
    pub seq: i64,
}

/// A client-presented proof that its account key is in the transparency log.
///
/// The head's `tree_size` is carried **once** and used for both the head and the
/// proof: `verify_inclusion_proof` requires them to agree, so representing them
/// separately would only create a mismatch that has no legitimate meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountAnchor {
    /// The exact leaf bytes that were hashed into the tree.
    pub leaf_bytes: Vec<u8>,
    /// This leaf's index in the tree.
    pub leaf_index: u64,
    /// The tree size of the head below — and of the inclusion proof.
    pub tree_size: u64,
    /// Sibling hashes, bottom-up (RFC 6962 audit path).
    pub audit_path: Vec<[u8; 32]>,
    /// The head's Merkle root.
    pub sth_root: [u8; 32],
    /// The head's timestamp, milliseconds since the epoch (signed).
    pub sth_timestamp_ms: u64,
    /// The head's ML-DSA-44 signature.
    pub sth_signature: Vec<u8>,
}

impl AccountAnchor {
    /// Rebuild the `verifiable-log` head this anchor carries.
    fn sth(&self) -> Sth {
        Sth {
            tree_size: self.tree_size,
            root_hash: hex::encode(self.sth_root),
            timestamp: self.sth_timestamp_ms,
            signature: hex::encode(&self.sth_signature),
            // The key id is an unsigned hint the relay has no use for: it pins
            // exactly one key and verifies against that.
            key_id: None,
        }
    }

    /// Rebuild the `verifiable-log` inclusion proof this anchor carries.
    fn proof(&self) -> InclusionProof {
        InclusionProof {
            leaf_index: self.leaf_index,
            tree_size: self.tree_size,
            audit_path: self.audit_path.iter().map(hex::encode).collect(),
        }
    }

    /// The tree entry these leaf bytes represent, under the account-key tenant.
    fn entry(&self) -> Entry {
        Entry::new(ACCOUNT_KEY_TENANT, self.leaf_bytes.clone())
    }

    /// Parse the leaf. Failure is a rejection, never a skip.
    fn leaf(&self) -> Result<AccountKeyLeaf, AnchorError> {
        serde_json::from_slice(&self.leaf_bytes).map_err(|_| AnchorError::MalformedLeaf)
    }
}

/// Why an anchor was refused. **Every variant is a refusal** — there is no
/// "couldn't tell, allow it" outcome, because a client that went to the trouble
/// of presenting an anchor has no legitimate reason to present a bad one.
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
    /// How old a head may be, in seconds. See [`DEFAULT_ANCHOR_MAX_AGE_SECS`].
    pub max_age_secs: u64,
}

impl AnchorVerifier {
    /// Build a verifier pinned to `log_pubkey_hex` — the hex of the account-key
    /// tree's ML-DSA-44 public key, i.e. what `/v1/account-keys/public_key.json`
    /// serves and what `PINNED_LOG_PUBLIC_KEYS` pins in the client.
    pub fn new(log_pubkey_hex: &str, max_age_secs: u64) -> Result<AnchorVerifier, AnchorError> {
        let verifying_key =
            verifying_key_from_hex(log_pubkey_hex.trim()).map_err(|_| AnchorError::BadPinnedKey)?;
        Ok(AnchorVerifier {
            verifying_key,
            max_age_secs,
        })
    }

    /// Verify `anchor` against the account this handshake claims.
    ///
    /// Checks run cheapest-first so junk is dropped before the ML-DSA-44
    /// signature verify — the only expensive step, and the one an unauthenticated
    /// flood would want to reach.
    pub fn verify(
        &self,
        anchor: &AccountAnchor,
        user_id: &str,
        account_id_pub: &[u8],
        identity_version: u32,
        now_secs: i64,
    ) -> Result<(), AnchorError> {
        // 1. Binding, before anything cryptographic. An anchor is public data —
        //    anyone can fetch anyone's — so a proof that is not bound to THIS
        //    handshake's account proves nothing about the connecting device.
        let leaf = anchor.leaf()?;
        if leaf.user_id != user_id
            || leaf.identity_version != u64::from(identity_version)
            || leaf.account_id_pub != hex::encode(account_id_pub)
        {
            return Err(AnchorError::LeafMismatch);
        }

        // 2. Head age. Signed, so it cannot be backdated by an on-path attacker;
        //    it bounds how stale a cached anchor may be, nothing more (see the
        //    module docs on what anchoring does NOT prove).
        let head_secs = anchor.sth_timestamp_ms / 1000;
        let now = now_secs.max(0) as u64;
        if head_secs > now.saturating_add(MAX_FUTURE_SKEW_SECS) {
            return Err(AnchorError::StaleTreeHead);
        }
        if now.saturating_sub(head_secs) > self.max_age_secs {
            return Err(AnchorError::StaleTreeHead);
        }

        // 3. The head is genuinely the log's, IN THE ACCOUNT-KEYS DOMAIN. Without
        //    the context a real head of the commit-log or binaries tree — same
        //    key, same signature scheme — would anchor an account proof.
        let sth = anchor.sth();
        if !sth.verify_with_context(&self.verifying_key, ACCOUNT_KEYS_STH_CONTEXT) {
            return Err(AnchorError::BadTreeHead);
        }

        // 4. And the leaf really is under that head's root.
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

/// What this node does about account-anchoring.
///
/// The ladder is deliberately three-rung, and [`AnchorPolicy::Require`] cannot
/// be constructed without a verifier — "require an anchor but have no key to
/// check it with" is not a representable state.
#[derive(Debug, Clone)]
pub enum AnchorPolicy {
    /// No transparency-log key configured. This node makes **no anchoring
    /// claim**: it neither requires an anchor nor pretends to have checked one.
    /// It must not reject a presented anchor either — it has no basis to.
    Ignore,
    /// Verify every anchor presented; a bad one is an authentication failure.
    /// A client that presents none is still served (the shipped fleet, and any
    /// account not yet in the daily publish).
    Verify(Arc<AnchorVerifier>),
    /// Verify, **and** require one. A client presenting none is refused with
    /// [`crate::proto::RejectReason::AnchorRequired`].
    Require(Arc<AnchorVerifier>),
}

impl AnchorPolicy {
    /// Build a policy from a pinned key. `require` promotes it to
    /// [`AnchorPolicy::Require`].
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

    /// The verifier, if this node has one.
    pub fn verifier(&self) -> Option<&AnchorVerifier> {
        match self {
            AnchorPolicy::Ignore => None,
            AnchorPolicy::Verify(v) | AnchorPolicy::Require(v) => Some(v),
        }
    }

    /// Is a client obliged to present an anchor?
    pub fn is_required(&self) -> bool {
        matches!(self, AnchorPolicy::Require(_))
    }
}

impl Default for AnchorPolicy {
    fn default() -> AnchorPolicy {
        AnchorPolicy::Ignore
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

    /// Build a real tree containing `leaves`, and return an anchor for the leaf
    /// at `index` under a head signed by `sk` in `context`.
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

    /// The frozen leaf encoding, pinned byte-for-byte.
    ///
    /// This is the one thing that cannot be checked against
    /// `verifiable-log-builder` from here (it would drag libsql into the relay's
    /// dev graph). If the account tenant ever reorders a field, renames one, or
    /// stops emitting compact JSON, this assertion fails — instead of every
    /// anchor in the fleet silently failing its inclusion proof.
    #[test]
    fn account_key_leaf_encoding_is_frozen() {
        let bytes = leaf_bytes("u_123", 2, &[0xab, 0xcd], 7);
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"user_id":"u_123","identity_version":2,"account_id_pub":"abcd","seq":7}"#
        );
        // And the tenant length-prefix the entry encoding puts in front of it.
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

        // Genuine head, genuine proof, genuine leaf — but the handshake presents
        // a DIFFERENT account key. This is the check that makes an anchor mean
        // anything: without it, any published leaf would anchor any handshake.
        assert_eq!(
            verifier.verify(&anchor, USER, &[0x09, 0x09], 3, NOW),
            Err(AnchorError::LeafMismatch)
        );
        // Same for the user id and the identity version.
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
        // Signed by the RIGHT key, over the RIGHT root, with the commit-log
        // domain context instead of the account-keys one.
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
        // A perfectly self-consistent tree — the attacker's own log.
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

        // Flip a sibling hash: the recomputed root no longer matches the signed
        // one, and the head's signature still checks out — so ONLY the merkle
        // step can catch this.
        let mut tampered = good.clone();
        tampered.audit_path[0][0] ^= 0xff;
        assert_eq!(
            verifier.verify(&tampered, USER, &account, 1, NOW),
            Err(AnchorError::NotIncluded)
        );

        // Claim a different index for the same leaf.
        let mut reindexed = good.clone();
        reindexed.leaf_index = 0;
        assert_eq!(
            verifier.verify(&reindexed, USER, &account, 1, NOW),
            Err(AnchorError::NotIncluded)
        );
    }

    /// A leaf that was never in the tree cannot be smuggled in by pairing it
    /// with a genuine head — the whole point of the audit path.
    #[test]
    fn an_unpublished_account_cannot_borrow_a_genuine_head() {
        let sk = log_key(11);
        let real = vec![0x01];
        let leaves = vec![leaf_bytes("published_user", 1, &real, 1)];
        let genuine = anchor_for(&sk, &leaves, 0, ACCOUNT_KEYS_STH_CONTEXT, NOW as u64 * 1000);
        let verifier = AnchorVerifier::new(&pinned(&sk), DEFAULT_ANCHOR_MAX_AGE_SECS).unwrap();

        // An attacker mints an account key offline and swaps its own leaf under
        // the real head. Everything else is untouched and genuinely signed.
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

        // A bad key yields no policy at all, so a node can never come up
        // "requiring" anchors it has no way to check.
        assert!(AnchorPolicy::configured("zzz", 60, true).is_err());
    }
}
