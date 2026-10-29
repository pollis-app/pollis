//! Root-signed key-set statements: what a client pins instead of a signer key (#754).
//!
//! The online signer's seed is a CI secret, and a leak could sign the very
//! binaries tree that verifies the re-pinning fix. So an offline root signs only
//! "these key ids may sign these trees, until this date", and clients pin the
//! root. Signers are sorted and deduplicated, checked on verification, so one
//! set has exactly one signed encoding.

use ml_dsa::{EncodedSignature, EncodedVerifyingKey, Keypair, MlDsa44, Signer, Verifier};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::sth::{key_id_for, SigningKey, VerifyingKey};

/// Distinct from every STH context, so a signature cannot replay across them.
pub const KEYSET_DOMAIN: &[u8] = b"pollis-verifiable-log:keyset:v1";

/// Bumped when the signed preimage changes; an unknown version is refused.
pub const KEYSET_FORMAT_VERSION: u32 = 1;

const PUBLIC_KEY_LEN: usize = 1312;

/// One signer the root vouches for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignerEntry {
    /// [`key_id_for`] of `public_key`; never a trust input on its own.
    pub key_id: String,
    pub public_key: String,
    /// STH contexts this key may sign under. A commit-log signer must not gain the
    /// binaries tree, which verifies the fix for its own compromise.
    pub trees: Vec<String>,
    /// Ms since epoch after which this signer is no longer accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after_ms: Option<u64>,
}

/// A root-signed assertion about which keys may sign which trees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySetStatement {
    pub format_version: u32,
    pub issued_at_ms: u64,
    /// Bounds how long a client cut off from updates trusts a stale set, so an
    /// attacker blocking updates cannot freeze a compromised signer in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after_ms: Option<u64>,
    /// Sorted by `key_id`, no duplicates.
    pub signers: Vec<SignerEntry>,
    /// Root signature over [`signing_message`], hex.
    pub signature: String,
    /// Which root signed it; an unsigned hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_key_id: Option<String>,
}

/// Canonical signed bytes; every variable-length field is length-prefixed so no
/// two statements share a preimage.
fn signing_message(
    format_version: u32,
    issued_at_ms: u64,
    not_after_ms: Option<u64>,
    signers: &[SignerEntry],
) -> Result<Vec<u8>> {
    let mut m = Vec::with_capacity(KEYSET_DOMAIN.len() + 32 + signers.len() * (PUBLIC_KEY_LEN + 64));
    m.extend_from_slice(KEYSET_DOMAIN);
    m.extend_from_slice(&format_version.to_be_bytes());
    m.extend_from_slice(&issued_at_ms.to_be_bytes());
    // 0 = no expiry; an epoch-0 expiry would already be expired, so no collision.
    m.extend_from_slice(&not_after_ms.unwrap_or(0).to_be_bytes());
    m.extend_from_slice(&(signers.len() as u32).to_be_bytes());
    for s in signers {
        m.extend_from_slice(&(s.key_id.len() as u32).to_be_bytes());
        m.extend_from_slice(s.key_id.as_bytes());
        // Raw bytes, not hex: one canonical spelling of each key.
        let raw = decode_public_key(&s.public_key)?;
        m.extend_from_slice(&raw);
        m.extend_from_slice(&s.not_after_ms.unwrap_or(0).to_be_bytes());
        m.extend_from_slice(&(s.trees.len() as u32).to_be_bytes());
        for t in &s.trees {
            m.extend_from_slice(&(t.len() as u32).to_be_bytes());
            m.extend_from_slice(t.as_bytes());
        }
    }
    Ok(m)
}

fn decode_public_key(hex: &str) -> Result<Vec<u8>> {
    if hex.len() != PUBLIC_KEY_LEN * 2 {
        return Err(Error::KeySet(format!(
            "public key must be {} hex chars, got {}",
            PUBLIC_KEY_LEN * 2,
            hex.len()
        )));
    }
    // Fetched input: byte-slicing would panic on a multi-byte char.
    hex::decode(hex).map_err(|_| Error::KeySet("public key is not lowercase hex".into()))
}

/// Sorted by `key_id`, no duplicates, non-empty, every key well-formed. Run on
/// verify too: a construction-time check binds only honest producers.
fn validate(signers: &[SignerEntry]) -> Result<()> {
    if signers.is_empty() {
        return Err(Error::KeySet(
            "a key-set statement naming no signers would retire the log; refusing".into(),
        ));
    }
    for w in signers.windows(2) {
        if w[0].key_id >= w[1].key_id {
            return Err(Error::KeySet(
                "signers must be sorted by key_id with no duplicates".into(),
            ));
        }
    }
    for s in signers {
        decode_public_key(&s.public_key)?;
        if s.trees.is_empty() {
            return Err(Error::KeySet(format!(
                "signer {} names no trees, so it may sign nothing; omit it instead",
                s.key_id
            )));
        }
    }
    Ok(())
}

impl KeySetStatement {
    /// Sign a key set with the offline root. Sorts and validates first, so a
    /// statement this produced always verifies.
    pub fn create(
        root: &SigningKey,
        issued_at_ms: u64,
        not_after_ms: Option<u64>,
        mut signers: Vec<SignerEntry>,
    ) -> Result<Self> {
        signers.sort_by(|a, b| a.key_id.cmp(&b.key_id));
        validate(&signers)?;
        let message = signing_message(KEYSET_FORMAT_VERSION, issued_at_ms, not_after_ms, &signers)?;
        let signature: ml_dsa::Signature<MlDsa44> = root.sign(&message);
        Ok(Self {
            format_version: KEYSET_FORMAT_VERSION,
            issued_at_ms,
            not_after_ms,
            signers,
            signature: hex::encode(signature.encode().as_slice()),
            root_key_id: Some(key_id_for(&root.verifying_key())),
        })
    }

    /// Verify against one pinned root.
    pub fn verify(&self, root_public: &VerifyingKey) -> Result<()> {
        if self.format_version != KEYSET_FORMAT_VERSION {
            return Err(Error::KeySet(format!(
                "unsupported key-set format version {} (this build understands {})",
                self.format_version, KEYSET_FORMAT_VERSION
            )));
        }
        validate(&self.signers)?;
        let message = signing_message(
            self.format_version,
            self.issued_at_ms,
            self.not_after_ms,
            &self.signers,
        )?;
        // Attacker-reachable: index-slicing would panic.
        let raw = hex::decode(&self.signature)
            .map_err(|_| Error::KeySet("signature is not hex".into()))?;
        let encoded = EncodedSignature::<MlDsa44>::try_from(raw.as_slice())
            .map_err(|_| Error::KeySet("signature is not 2420 bytes".into()))?;
        let sig = ml_dsa::Signature::<MlDsa44>::decode(&encoded)
            .ok_or_else(|| Error::KeySet("signature does not decode".into()))?;
        root_public
            .verify(&message, &sig)
            .map_err(|_| Error::KeySet("key-set signature does not verify".into()))
    }

    /// Verify against any pinned root (root-rotation overlap).
    pub fn verify_any<'k>(
        &self,
        roots: impl IntoIterator<Item = &'k VerifyingKey>,
    ) -> Result<&'k VerifyingKey> {
        let mut last = None;
        for r in roots {
            match self.verify(r) {
                Ok(()) => return Ok(r),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            Error::KeySet("no pinned root keys to verify the key set against".into())
        }))
    }

    /// The signers acceptable for `context` at `now_ms`.
    ///
    /// An expired statement yields none, surfacing as "cannot verify" rather than
    /// silent acceptance. Commit-log authority extends to its successor tree
    /// ([`crate::sth::key_set_authority_for`]).
    pub fn active_signers_for(&self, context: &[u8], now_ms: u64) -> Vec<&SignerEntry> {
        if self.not_after_ms.is_some_and(|t| now_ms > t) {
            return Vec::new();
        }
        let want = String::from_utf8_lossy(context).to_string();
        let authority =
            String::from_utf8_lossy(crate::sth::key_set_authority_for(context)).to_string();
        self.signers
            .iter()
            .filter(|s| s.not_after_ms.is_none_or(|t| now_ms <= t))
            .filter(|s| s.trees.contains(&want) || s.trees.contains(&authority))
            .collect()
    }
}

pub fn root_key_from_hex(hex: &str) -> Result<VerifyingKey> {
    let raw = decode_public_key(hex)?;
    let encoded = EncodedVerifyingKey::<MlDsa44>::try_from(raw.as_slice())
        .map_err(|_| Error::KeySet("root key is not a valid ML-DSA-44 key".into()))?;
    Ok(VerifyingKey::decode(&encoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ml_dsa::Keypair;

    const NOW: u64 = 1_700_000_000_000;
    const COMMIT: &str = "pollis-verifiable-log:sth:v2";
    const BINARIES: &str = "pollis-verifiable-log:sth:v2:binaries";

    fn root(seed: u8) -> SigningKey {
        SigningKey::from_seed(&[seed; 32].into())
    }

    fn signer(seed: u8, trees: &[&str], not_after_ms: Option<u64>) -> SignerEntry {
        let sk = root(seed);
        let vk = sk.verifying_key();
        SignerEntry {
            key_id: key_id_for(&vk),
            public_key: hex::encode(vk.encode().as_slice()),
            trees: trees.iter().map(|t| t.to_string()).collect(),
            not_after_ms,
        }
    }

    fn statement(root_key: &SigningKey, signers: Vec<SignerEntry>) -> KeySetStatement {
        KeySetStatement::create(root_key, NOW, Some(NOW + 90 * 86_400_000), signers).expect("create")
    }

    #[test]
    fn a_statement_verifies_under_the_root_that_signed_it() {
        let r = root(1);
        let st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        assert!(st.verify(&r.verifying_key()).is_ok());
    }

    #[test]
    fn a_different_root_cannot_vouch_for_a_key_set() {
        let st = statement(&root(1), vec![signer(2, &[COMMIT], None)]);
        assert!(st.verify(&root(9).verifying_key()).is_err());
    }

    #[test]
    fn substituting_a_signer_key_breaks_the_statement() {
        let r = root(1);
        let mut st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        st.signers[0].public_key = hex::encode(root(3).verifying_key().encode().as_slice());
        assert!(st.verify(&r.verifying_key()).is_err());
    }

    #[test]
    fn widening_a_signers_trees_breaks_the_statement() {
        let r = root(1);
        let mut st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        st.signers[0].trees.push(BINARIES.to_string());
        assert!(st.verify(&r.verifying_key()).is_err());
    }

    #[test]
    fn extending_an_expiry_breaks_the_statement() {
        let r = root(1);
        let mut st = statement(&r, vec![signer(2, &[COMMIT], Some(NOW))]);
        st.signers[0].not_after_ms = Some(NOW + 10_000_000);
        assert!(st.verify(&r.verifying_key()).is_err());
    }

    #[test]
    fn reordering_the_set_breaks_the_statement() {
        let r = root(1);
        let mut st = statement(&r, vec![signer(2, &[COMMIT], None), signer(3, &[COMMIT], None)]);
        st.signers.swap(0, 1);
        assert!(st.verify(&r.verifying_key()).is_err());
    }

    #[test]
    fn a_duplicated_key_id_is_refused() {
        let r = root(1);
        let mut st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        let dup = st.signers[0].clone();
        st.signers.push(dup);
        assert!(st.verify(&r.verifying_key()).is_err());
    }

    #[test]
    fn an_empty_set_is_refused_rather_than_retiring_the_log() {
        assert!(KeySetStatement::create(&root(1), NOW, None, vec![]).is_err());
    }

    #[test]
    fn a_signer_naming_no_trees_is_refused() {
        assert!(KeySetStatement::create(&root(1), NOW, None, vec![signer(2, &[], None)]).is_err());
    }

    #[test]
    fn an_unknown_format_version_is_refused_not_guessed_at() {
        let r = root(1);
        let mut st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        st.format_version = KEYSET_FORMAT_VERSION + 1;
        assert!(st.verify(&r.verifying_key()).is_err());
    }

    #[test]
    fn a_signer_is_only_active_for_the_trees_it_names() {
        let r = root(1);
        let st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        assert_eq!(st.active_signers_for(COMMIT.as_bytes(), NOW).len(), 1);
        assert_eq!(st.active_signers_for(BINARIES.as_bytes(), NOW).len(), 0);
    }

    #[test]
    fn a_commit_log_signer_also_signs_its_successor_tree_and_nothing_else() {
        let r = root(1);
        let st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        let successor = crate::sth::STH_CONTEXT_COMMIT_LOG_2;
        assert_eq!(st.active_signers_for(successor, NOW).len(), 1);
        let st = statement(&r, vec![signer(2, &[BINARIES], None)]);
        assert_eq!(st.active_signers_for(successor, NOW).len(), 0);
        assert_eq!(st.active_signers_for(COMMIT.as_bytes(), NOW).len(), 0);
    }

    #[test]
    fn a_retired_signer_stops_being_active_on_schedule() {
        let r = root(1);
        let st = statement(&r, vec![signer(2, &[COMMIT], Some(NOW + 1000))]);
        assert_eq!(st.active_signers_for(COMMIT.as_bytes(), NOW).len(), 1);
        assert_eq!(st.active_signers_for(COMMIT.as_bytes(), NOW + 2000).len(), 0);
    }

    #[test]
    fn an_expired_statement_yields_no_signers_at_all() {
        let r = root(1);
        let st = KeySetStatement::create(&r, NOW, Some(NOW + 1000), vec![signer(2, &[COMMIT], None)])
            .expect("create");
        assert_eq!(st.active_signers_for(COMMIT.as_bytes(), NOW).len(), 1);
        assert_eq!(st.active_signers_for(COMMIT.as_bytes(), NOW + 2000).len(), 0);
    }

    #[test]
    fn verify_any_accepts_either_root_during_an_overlap() {
        let outgoing = root(1);
        let incoming = root(7);
        let st = statement(&incoming, vec![signer(2, &[COMMIT], None)]);
        let roots = [outgoing.verifying_key(), incoming.verifying_key()];
        assert!(st.verify_any(roots.iter()).is_ok());
    }

    #[test]
    fn verify_any_rejects_when_no_pinned_root_signed_it() {
        let st = statement(&root(5), vec![signer(2, &[COMMIT], None)]);
        let roots = [root(1).verifying_key(), root(7).verifying_key()];
        assert!(st.verify_any(roots.iter()).is_err());
    }

    #[test]
    fn a_signature_cannot_be_lifted_onto_a_different_statement() {
        let r = root(1);
        let a = statement(&r, vec![signer(2, &[COMMIT], None)]);
        let mut b = statement(&r, vec![signer(3, &[BINARIES], None)]);
        b.signature = a.signature.clone();
        assert!(b.verify(&r.verifying_key()).is_err());
    }

    #[test]
    fn the_root_key_id_hint_is_not_a_trust_input() {
        let r = root(1);
        let mut st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        st.root_key_id = Some("deadbeefdeadbeef".into());
        assert!(st.verify(&r.verifying_key()).is_ok());
    }

    #[test]
    fn a_statement_round_trips_through_json() {
        let r = root(1);
        let st = statement(&r, vec![signer(2, &[COMMIT], None), signer(3, &[BINARIES], None)]);
        let back: KeySetStatement =
            serde_json::from_str(&serde_json::to_string(&st).expect("ser")).expect("de");
        assert_eq!(back, st);
        assert!(back.verify(&r.verifying_key()).is_ok());
    }
}
