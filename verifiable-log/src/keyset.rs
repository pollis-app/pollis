//! Root-signed key-set statements — what a client pins instead of a signer key (#754).
//!
//! **The problem this exists to solve.** The log's signing seed is a GitHub Actions
//! secret, and the client pins that signer's public key as a compiled-in constant.
//! Those two facts together have no answer to a leak: an attacker holding the seed
//! signs a forged tree, and the fix — a client update that re-pins — is itself
//! verified *through the binaries tree the compromised key signs*. The attacker
//! vouches for their own build of the remedy. Recovery requires an authority the
//! online signer cannot speak for.
//!
//! **The shape of the answer.** A separate root key, held offline, signs nothing but
//! statements of the form "these key ids may sign these trees, until this date".
//! Clients pin the ROOT. The signer set becomes data the client fetches and verifies
//! rather than a constant compiled into it, so retiring a compromised signer is a
//! statement the root signs — not a release.
//!
//! The root is deliberately dull: it signs rarely, so it can live offline in a way a
//! per-release signer never could. That asymmetry is the entire security argument.
//!
//! **Why sorted and deduplicated.** The statement means a *set*. Two encodings of the
//! same set must be the same bytes, or "the signature covers the set" is not quite
//! true — a reordering would be a different signed object asserting the same thing,
//! which is exactly the ambiguity signature schemes are supposed to remove. Both are
//! enforced on verification, not merely on construction, because only the verifier's
//! check binds an attacker.

use ml_dsa::{EncodedSignature, EncodedVerifyingKey, Keypair, MlDsa44, Signer, Verifier};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::sth::{key_id_for, SigningKey, VerifyingKey};

/// Domain separation for key-set statements. Distinct from every STH context, so a
/// signature over one can never be replayed as the other even if a key were shared.
pub const KEYSET_DOMAIN: &[u8] = b"pollis-verifiable-log:keyset:v1";

/// Wire format version. Bumped when the signed preimage changes; a verifier that
/// does not recognise the version refuses rather than guessing at the bytes.
pub const KEYSET_FORMAT_VERSION: u32 = 1;

/// Encoded ML-DSA-44 public key length, in bytes.
const PUBLIC_KEY_LEN: usize = 1312;

/// One signer the root vouches for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignerEntry {
    /// [`key_id_for`] of `public_key`. Carried so diagnostics and the `Sth.key_id`
    /// hint can name a key without handling 1312 bytes; never a trust input on its
    /// own — the key bytes are what verification uses.
    pub key_id: String,
    /// ML-DSA-44 public key, lowercase hex (1312 bytes).
    pub public_key: String,
    /// Domain-separation contexts this key may sign under, e.g. the per-tree STH
    /// contexts. A compromise of the commit-log signer must not imply authority over
    /// the binaries tree — which is the tree used to verify the fix.
    pub trees: Vec<String>,
    /// Milliseconds since epoch after which this signer is no longer accepted.
    /// `None` = no scheduled expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after_ms: Option<u64>,
}

/// A root-signed assertion about which keys may sign which trees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySetStatement {
    pub format_version: u32,
    /// When the root issued this statement (ms since epoch).
    pub issued_at_ms: u64,
    /// When this statement stops being accepted, independent of the signers it
    /// names. Bounds how long a client that cannot reach the log keeps trusting a
    /// stale set — an unbounded statement would let an attacker who can block
    /// updates freeze a compromised signer in place forever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after_ms: Option<u64>,
    /// Sorted by `key_id`, no duplicates. See the module note.
    pub signers: Vec<SignerEntry>,
    /// Root ML-DSA-44 signature over [`signing_message`], hex (2420 bytes).
    pub signature: String,
    /// Which root signed it, so a client pinning several roots during a root
    /// rotation selects rather than trial-verifies. A hint, outside the signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_key_id: Option<String>,
}

/// Canonical signed bytes. Every variable-length field is length-prefixed so no two
/// distinct statements can share a preimage.
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
    // 0 is the "no expiry" sentinel: an epoch-0 expiry is already expired, so the
    // sentinel can never collide with a meaningful value.
    m.extend_from_slice(&not_after_ms.unwrap_or(0).to_be_bytes());
    m.extend_from_slice(&(signers.len() as u32).to_be_bytes());
    for s in signers {
        m.extend_from_slice(&(s.key_id.len() as u32).to_be_bytes());
        m.extend_from_slice(s.key_id.as_bytes());
        // The raw key, not its hex: one canonical representation, and it fails loudly
        // here rather than admitting a differently-cased spelling of the same key.
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
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| Error::KeySet("public key is not lowercase hex".into()))
        })
        .collect()
}

/// Sorted by `key_id`, no duplicates, non-empty, and every key well-formed.
///
/// Enforced on the VERIFIER side deliberately: a construction-time check binds only
/// honest producers.
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
            signature: hex_of(signature.encode().as_slice()),
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
        let raw = (0..self.signature.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&self.signature[i..i + 2], 16)
                    .map_err(|_| Error::KeySet("signature is not hex".into()))
            })
            .collect::<Result<Vec<u8>>>()?;
        let encoded = EncodedSignature::<MlDsa44>::try_from(raw.as_slice())
            .map_err(|_| Error::KeySet("signature is not 2420 bytes".into()))?;
        let sig = ml_dsa::Signature::<MlDsa44>::decode(&encoded)
            .ok_or_else(|| Error::KeySet("signature does not decode".into()))?;
        root_public
            .verify(&message, &sig)
            .map_err(|_| Error::KeySet("key-set signature does not verify".into()))
    }

    /// Verify against any pinned root — the root-rotation overlap path.
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
    /// Expiry is applied here rather than left to the caller so a stale statement
    /// cannot quietly keep a retired signer alive: a statement past its own
    /// `not_after_ms` yields nothing at all, which surfaces as "cannot verify"
    /// rather than as silent acceptance.
    pub fn active_signers_for(&self, context: &[u8], now_ms: u64) -> Vec<&SignerEntry> {
        if self.not_after_ms.is_some_and(|t| now_ms > t) {
            return Vec::new();
        }
        let want = String::from_utf8_lossy(context).to_string();
        self.signers
            .iter()
            .filter(|s| !s.not_after_ms.is_some_and(|t| now_ms > t))
            .filter(|s| s.trees.iter().any(|t| *t == want))
            .collect()
    }
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode a hex public key into a verifying key.
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
            public_key: hex_of(vk.encode().as_slice()),
            trees: trees.iter().map(|t| t.to_string()).collect(),
            not_after_ms,
        }
    }

    fn statement(root_key: &SigningKey, signers: Vec<SignerEntry>) -> KeySetStatement {
        KeySetStatement::create(root_key, NOW, Some(NOW + 90 * 86_400_000), signers).expect("create")
    }

    // ── The property the whole design rests on ───────────────────────────────

    #[test]
    fn a_statement_verifies_under_the_root_that_signed_it() {
        let r = root(1);
        let st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        assert!(st.verify(&r.verifying_key()).is_ok());
    }

    #[test]
    fn a_different_root_cannot_vouch_for_a_key_set() {
        // The point of the split: the online signer, or anyone else, must not be
        // able to speak for which keys are valid.
        let st = statement(&root(1), vec![signer(2, &[COMMIT], None)]);
        assert!(st.verify(&root(9).verifying_key()).is_err());
    }

    #[test]
    fn substituting_a_signer_key_breaks_the_statement() {
        // The attack this exists to stop: swap in a key you hold, keep everything
        // else, and the client accepts your forged tree.
        let r = root(1);
        let mut st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        st.signers[0].public_key = hex_of(root(3).verifying_key().encode().as_slice());
        assert!(st.verify(&r.verifying_key()).is_err());
    }

    #[test]
    fn widening_a_signers_trees_breaks_the_statement() {
        // A commit-log signer must not be able to grant itself the binaries tree —
        // the tree used to verify the fix for its own compromise.
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
        // The statement means a SET, so exactly one encoding may verify — otherwise
        // "the signature covers the set" is not quite true.
        let r = root(1);
        let mut st = statement(&r, vec![signer(2, &[COMMIT], None), signer(3, &[COMMIT], None)]);
        st.signers.swap(0, 1);
        assert!(st.verify(&r.verifying_key()).is_err());
    }

    // ── Shapes a verifier must refuse ────────────────────────────────────────

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

    // ── Expiry semantics ─────────────────────────────────────────────────────

    #[test]
    fn a_signer_is_only_active_for_the_trees_it_names() {
        let r = root(1);
        let st = statement(&r, vec![signer(2, &[COMMIT], None)]);
        assert_eq!(st.active_signers_for(COMMIT.as_bytes(), NOW).len(), 1);
        assert_eq!(st.active_signers_for(BINARIES.as_bytes(), NOW).len(), 0);
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
        // The anti-freeze property: an attacker who can block a client's updates
        // must not be able to pin a compromised signer in place forever. Past its
        // own expiry the statement grants nothing, which surfaces as "cannot
        // verify" rather than as silent continued trust.
        let r = root(1);
        let st = KeySetStatement::create(&r, NOW, Some(NOW + 1000), vec![signer(2, &[COMMIT], None)])
            .expect("create");
        assert_eq!(st.active_signers_for(COMMIT.as_bytes(), NOW).len(), 1);
        assert_eq!(st.active_signers_for(COMMIT.as_bytes(), NOW + 2000).len(), 0);
    }

    // ── Root rotation ────────────────────────────────────────────────────────

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
        // Length-prefixed canonical bytes: no two distinct statements share a
        // preimage, so a valid signature is valid for exactly one of them.
        let r = root(1);
        let a = statement(&r, vec![signer(2, &[COMMIT], None)]);
        let mut b = statement(&r, vec![signer(3, &[BINARIES], None)]);
        b.signature = a.signature.clone();
        assert!(b.verify(&r.verifying_key()).is_err());
    }

    #[test]
    fn the_root_key_id_hint_is_not_a_trust_input() {
        // It is outside the signature by design; forging it must change nothing.
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
