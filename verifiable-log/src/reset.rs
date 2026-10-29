//! Signed **reset statements**: the log's own record that one of its trees was
//! retired and replaced by a new one (#1579).
//!
//! **Why this exists.** A transparency log promises that every signed head extends
//! every earlier one. When a tree breaks that promise (the first commit-log tree
//! did, after it was rebuilt from a pruned database table), there are two honest
//! options: continue it at hash level, or retire it and start a new tree. A silent
//! restart is not one of them, because a new tree with no statement behind it is
//! indistinguishable from a log quietly discarding history it did not like.
//!
//! A reset statement is that statement, signed by the log's signing key under its
//! own domain ([`RESET_DOMAIN`]), so it can never be mistaken for, or replayed as, a
//! tree head or a key set. It records:
//!
//! * the retired tree's identity (name, STH context, URL path);
//! * its **last consistent head** (a full signed STH) and the leaf hashes that
//!   head commits to, so anyone can recompute the root of every earlier head on
//!   the same chain without the leaf data, some of which is gone;
//! * every other head of the retired tree that is still served, each with its
//!   root: the sizes on the consistent chain, the **forked** heads signed during
//!   the incident, and heads **superseded** by an earlier re-genesis;
//! * why the tree was retired;
//! * the successor tree's identity and its **genesis**: the head at size 1, i.e.
//!   the hash of its first leaf, which every later head of the successor must
//!   commit to.
//!
//! A verifier then reports the retired tree as RETIRED rather than failing on it:
//! the consistent chain is still checked head by head, and each listed fork is
//! reported as known. Anything the statement does not list (a head at an unlisted
//! size, or a listed size with a different root) is still a failure. Listing is
//! not absolution for what the log might sign tomorrow: the statement is signed
//! once and its retired tree is frozen, so a head it does not name is evidence the
//! freeze was broken.
//!
//! **Why the online key, not the root.** The root key (#754) is offline and signs
//! only key sets, and CI must never hold it. The signing key is the party that
//! signed the inconsistent heads, so it is the party whose account of them is
//! being recorded: the statement binds that key to one description of its own
//! history, publicly and permanently. It cannot be used to hide anything, because
//! every head it lists is checked against what is actually served.
//!
//! **Encoding.** The signed bytes are `RESET_DOMAIN || len(u64 BE) || body`, where
//! `body` is the compact JSON of [`ResetBody`]. Every type in the body is a struct,
//! a sequence, a string or an integer, and unknown fields are refused, so parsing
//! and re-serialising a body is canonical: field order follows the declaration,
//! never the input.

use ml_dsa::{EncodedSignature, Keypair, MlDsa44, Signer, Verifier};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::{from_hex, to_hex, Hash};
use crate::merkle;
use crate::sth::{key_id_for, SigningKey, Sth, VerifyingKey};

/// Domain separation for reset statements. Distinct from every STH context and
/// from the key-set domain.
pub const RESET_DOMAIN: &[u8] = b"pollis-verifiable-log:reset:v1";

/// Wire format version of [`ResetBody`]. A verifier that does not recognise it
/// refuses rather than guessing at the bytes.
pub const RESET_FORMAT_VERSION: u32 = 1;

/// A head of the retired tree that is listed by size and root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListedHead {
    pub tree_size: u64,
    /// Lowercase hex, 32 bytes.
    pub root_hash: String,
    /// The timestamp in the served head (ms since epoch), for disclosure.
    pub timestamp: u64,
    /// One line on what this head is.
    pub note: String,
}

/// The tree being retired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetiredTree {
    /// Tree name, as in [`crate::sth::STH_CONTEXTS`].
    pub tree: String,
    /// STH context the retired heads are signed under.
    pub sth_context: String,
    /// URL path the retired tree is served under, relative to the log's base URL.
    pub path: String,
    /// The newest head that is consistent with every earlier head on its chain.
    pub last_consistent: Sth,
    /// Leaf hashes (lowercase hex) of the `last_consistent.tree_size` leaves that
    /// head commits to, in leaf order.
    pub leaf_hashes: Vec<String>,
    /// Every served head size on the consistent chain, ascending. Each head's root
    /// is the Merkle root of that prefix of [`Self::leaf_hashes`].
    pub chain_sizes: Vec<u64>,
    /// Heads signed during the incident that are not on the consistent chain.
    pub forks: Vec<ListedHead>,
    /// Heads left over from an earlier generation of the tree, still served.
    pub superseded: Vec<ListedHead>,
}

/// The tree that replaces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuccessorTree {
    pub tree: String,
    pub sth_context: String,
    pub path: String,
    /// Root of the successor's head at size 1: the hash of its first leaf.
    pub genesis_root_hash: String,
}

/// The signed content of a reset statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetBody {
    pub format_version: u32,
    /// When the statement was signed (ms since epoch).
    pub issued_at_ms: u64,
    /// Plain-language reason, and where it is documented.
    pub reason: String,
    pub retired: RetiredTree,
    pub successor: SuccessorTree,
}

/// A signed reset statement, as served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetStatement {
    pub statement: ResetBody,
    /// ML-DSA-44 signature over [`signing_message`], lowercase hex.
    pub signature: String,
    /// Which key signed it. A selection hint outside the signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
}

/// How a served head of the retired tree relates to the statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadClass<'a> {
    /// On the consistent chain: its root is the root of that prefix of the
    /// listed leaf hashes.
    Chain,
    /// A fork the statement lists, at this size and root.
    ListedFork(&'a ListedHead),
    /// A superseded head the statement lists, at this size and root.
    Superseded(&'a ListedHead),
    /// Not described by the statement. Always a failure.
    Unlisted,
}

fn refuse(msg: impl Into<String>) -> Error {
    Error::Reset(msg.into())
}

fn signing_message(body: &ResetBody) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(body).map_err(|e| refuse(format!("encode body: {e}")))?;
    let mut m = Vec::with_capacity(RESET_DOMAIN.len() + 8 + json.len());
    m.extend_from_slice(RESET_DOMAIN);
    m.extend_from_slice(&(json.len() as u64).to_be_bytes());
    m.extend_from_slice(&json);
    Ok(m)
}

impl ResetBody {
    /// The decoded leaf hashes of the retired chain.
    pub fn chain_leaves(&self) -> Result<Vec<Hash>> {
        self.retired
            .leaf_hashes
            .iter()
            .map(|h| from_hex(h))
            .collect()
    }

    /// Check the statement against itself: the leaf hashes reproduce the last
    /// consistent head, the chain sizes are on that chain, and every listed fork
    /// really is off it. Says nothing about what is served; see [`Self::classify`].
    pub fn validate(&self) -> Result<()> {
        if self.format_version != RESET_FORMAT_VERSION {
            return Err(refuse(format!(
                "unsupported reset format version {} (this build understands {})",
                self.format_version, RESET_FORMAT_VERSION
            )));
        }
        let r = &self.retired;
        let leaves = self.chain_leaves()?;
        let n = r.last_consistent.tree_size;
        if leaves.len() as u64 != n || n == 0 {
            return Err(refuse(format!(
                "the statement lists {} leaf hashes for a last consistent head of size {n}",
                leaves.len()
            )));
        }
        if merkle::root_hash(&leaves) != r.last_consistent.root_bytes()? {
            return Err(refuse(
                "the listed leaf hashes do not reproduce the last consistent head",
            ));
        }
        for w in r.chain_sizes.windows(2) {
            if w[0] >= w[1] {
                return Err(refuse("chain sizes must be ascending with no duplicates"));
            }
        }
        if r.chain_sizes.first().is_some_and(|s| *s == 0) || r.chain_sizes.last() != Some(&n) {
            return Err(refuse(
                "chain sizes must be non-zero and end at the last consistent head",
            ));
        }
        let mut listed: Vec<(u64, &str)> = Vec::new();
        for head in r.forks.iter().chain(&r.superseded) {
            let root = from_hex(&head.root_hash)?;
            if head.tree_size == 0 {
                return Err(refuse("a listed head has size 0"));
            }
            if head.tree_size <= n && merkle::root_hash(&leaves[..head.tree_size as usize]) == root
            {
                return Err(refuse(format!(
                    "the head listed at size {} is on the consistent chain, so it is not a fork",
                    head.tree_size
                )));
            }
            if listed.iter().any(|(s, h)| *s == head.tree_size && *h == head.root_hash) {
                return Err(refuse(format!(
                    "the head at size {} is listed twice",
                    head.tree_size
                )));
            }
            listed.push((head.tree_size, &head.root_hash));
        }
        from_hex(&self.successor.genesis_root_hash)?;
        if self.successor.tree == r.tree
            || self.successor.sth_context == r.sth_context
            || self.successor.path == r.path
        {
            return Err(refuse(
                "the successor must differ from the retired tree in name, context and path",
            ));
        }
        Ok(())
    }

    /// Root of the retired chain at `size`, when `size` is on it.
    pub fn chain_root(&self, size: u64) -> Option<Hash> {
        let leaves = self.chain_leaves().ok()?;
        if size == 0 || size as usize > leaves.len() {
            return None;
        }
        Some(merkle::root_hash(&leaves[..size as usize]))
    }

    /// Classify a served retired-tree head by its size and root.
    pub fn classify(&self, tree_size: u64, root_hash: &str) -> HeadClass<'_> {
        let root = root_hash.to_ascii_lowercase();
        if let (Some(chain), Ok(served)) = (self.chain_root(tree_size), from_hex(&root)) {
            if chain == served {
                return HeadClass::Chain;
            }
        }
        if let Some(f) = self
            .retired
            .forks
            .iter()
            .find(|h| h.tree_size == tree_size && h.root_hash == root)
        {
            return HeadClass::ListedFork(f);
        }
        if let Some(s) = self
            .retired
            .superseded
            .iter()
            .find(|h| h.tree_size == tree_size && h.root_hash == root)
        {
            return HeadClass::Superseded(s);
        }
        HeadClass::Unlisted
    }
}

impl ResetStatement {
    /// Validate and sign `body`.
    pub fn create(signing_key: &SigningKey, body: ResetBody) -> Result<Self> {
        body.validate()?;
        let message = signing_message(&body)?;
        let signature: ml_dsa::Signature<MlDsa44> = signing_key.sign(&message);
        Ok(Self {
            statement: body,
            signature: hex::encode(signature.encode().as_slice()),
            key_id: Some(key_id_for(&signing_key.verifying_key())),
        })
    }

    /// Verify the signature under one key, then [`ResetBody::validate`].
    pub fn verify(&self, key: &VerifyingKey) -> Result<()> {
        let message = signing_message(&self.statement)?;
        let raw = hex::decode(&self.signature).map_err(|_| refuse("signature is not hex"))?;
        let encoded = EncodedSignature::<MlDsa44>::try_from(raw.as_slice())
            .map_err(|_| refuse("signature is not 2420 bytes"))?;
        let sig = ml_dsa::Signature::<MlDsa44>::decode(&encoded)
            .ok_or_else(|| refuse("signature does not decode"))?;
        key.verify(&message, &sig)
            .map_err(|_| refuse("reset statement signature does not verify"))?;
        self.statement.validate()
    }

    /// Verify under any of `candidates`, returning the key that verified.
    pub fn verify_any<'k>(
        &self,
        candidates: &'k [(String, VerifyingKey)],
    ) -> Result<&'k (String, VerifyingKey)> {
        let mut last = refuse("no candidate keys to verify the reset statement against");
        for c in candidates {
            match self.verify(&c.1) {
                Ok(()) => return Ok(c),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

/// Lowercase hex of a leaf hash, for building a statement.
pub fn hash_hex(h: &Hash) -> String {
    to_hex(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sth::{STH_CONTEXT_COMMIT_LOG, STH_CONTEXT_COMMIT_LOG_2};
    use crate::{Entry, VerifiableLog};

    const TS: u64 = 1_700_000_000_000;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_seed(&[seed; 32].into())
    }

    fn body() -> ResetBody {
        let mut log = VerifiableLog::new();
        for i in 0..9u8 {
            log.append(Entry::new("t", vec![i])).unwrap();
        }
        let leaves: Vec<String> = (0..9)
            .map(|i| to_hex(&log.entry(i).unwrap().leaf_hash()))
            .collect();
        let last = Sth::create(&key(1), 9, log.root(), TS);
        let mut fork_log = VerifiableLog::new();
        fork_log.append(Entry::new("t", vec![99])).unwrap();
        ResetBody {
            format_version: RESET_FORMAT_VERSION,
            issued_at_ms: TS,
            reason: "test".into(),
            retired: RetiredTree {
                tree: "commit-log".into(),
                sth_context: String::from_utf8_lossy(STH_CONTEXT_COMMIT_LOG).into(),
                path: "v1".into(),
                last_consistent: last,
                leaf_hashes: leaves,
                chain_sizes: vec![4, 9],
                forks: vec![ListedHead {
                    tree_size: 1,
                    root_hash: to_hex(&fork_log.root()),
                    timestamp: TS,
                    note: "fork".into(),
                }],
                superseded: vec![],
            },
            successor: SuccessorTree {
                tree: "commit-log-2".into(),
                sth_context: String::from_utf8_lossy(STH_CONTEXT_COMMIT_LOG_2).into(),
                path: "v1/commit-log-2".into(),
                genesis_root_hash: to_hex(&[5u8; 32]),
            },
        }
    }

    fn cands(seed: u8) -> Vec<(String, VerifyingKey)> {
        let vk = key(seed).verifying_key();
        vec![(key_id_for(&vk), vk)]
    }

    #[test]
    fn a_statement_verifies_and_round_trips() {
        let st = ResetStatement::create(&key(1), body()).unwrap();
        assert!(st.verify_any(&cands(1)).is_ok());
        let back: ResetStatement =
            serde_json::from_str(&serde_json::to_string_pretty(&st).unwrap()).unwrap();
        assert_eq!(back, st);
        assert!(back.verify_any(&cands(1)).is_ok());
    }

    #[test]
    fn another_key_cannot_vouch_for_it() {
        let st = ResetStatement::create(&key(1), body()).unwrap();
        assert!(st.verify_any(&cands(2)).is_err());
    }

    #[test]
    fn any_tampering_breaks_the_signature() {
        let st = ResetStatement::create(&key(1), body()).unwrap();
        let mut a = st.clone();
        a.statement.retired.forks[0].root_hash = to_hex(&[7u8; 32]);
        assert!(a.verify_any(&cands(1)).is_err());
        let mut b = st.clone();
        b.statement.successor.genesis_root_hash = to_hex(&[6u8; 32]);
        assert!(b.verify_any(&cands(1)).is_err());
        let mut c = st.clone();
        c.statement.reason.push('!');
        assert!(c.verify_any(&cands(1)).is_err());
        let mut d = st;
        d.statement.retired.chain_sizes = vec![9];
        assert!(d.verify_any(&cands(1)).is_err());
    }

    #[test]
    fn an_unknown_field_is_refused_not_ignored() {
        let st = ResetStatement::create(&key(1), body()).unwrap();
        let mut v: serde_json::Value = serde_json::to_value(&st).unwrap();
        v["statement"]["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<ResetStatement>(v).is_err());
    }

    /// A signature from the same key over the same bytes under another domain
    /// (a tree head, a key set) is never a reset signature, and the reverse.
    #[test]
    fn a_signature_under_another_context_does_not_verify() {
        let b = body();
        let json = serde_json::to_vec(&b).unwrap();
        let mut wrong = Vec::new();
        wrong.extend_from_slice(crate::keyset::KEYSET_DOMAIN);
        wrong.extend_from_slice(&(json.len() as u64).to_be_bytes());
        wrong.extend_from_slice(&json);
        let sig: ml_dsa::Signature<MlDsa44> = key(1).sign(&wrong);
        let st = ResetStatement {
            statement: b,
            signature: hex::encode(sig.encode().as_slice()),
            key_id: None,
        };
        assert!(st.verify_any(&cands(1)).is_err());

        // A reset signature is not an STH signature either.
        let good = ResetStatement::create(&key(1), body()).unwrap();
        let fake_head = Sth {
            tree_size: 9,
            root_hash: good.statement.retired.last_consistent.root_hash.clone(),
            timestamp: TS,
            signature: good.signature.clone(),
            key_id: None,
        };
        assert!(!fake_head.verify(&key(1).verifying_key()));
    }

    #[test]
    fn validation_catches_a_statement_that_contradicts_itself() {
        let mut b = body();
        b.retired.leaf_hashes.pop();
        assert!(ResetStatement::create(&key(1), b).is_err());
        let mut b = body();
        b.retired.chain_sizes = vec![9, 4];
        assert!(ResetStatement::create(&key(1), b).is_err());
        // A "fork" that is really on the chain.
        let mut b = body();
        let root = merkle::root_hash(&b.chain_leaves().unwrap()[..3]);
        b.retired.forks[0] = ListedHead {
            tree_size: 3,
            root_hash: to_hex(&root),
            timestamp: TS,
            note: String::new(),
        };
        assert!(ResetStatement::create(&key(1), b).is_err());
        let mut b = body();
        b.successor.path = "v1".into();
        assert!(ResetStatement::create(&key(1), b).is_err());
        let mut b = body();
        b.format_version = 2;
        assert!(ResetStatement::create(&key(1), b).is_err());
    }

    #[test]
    fn classify_separates_chain_listed_and_unlisted_heads() {
        let b = body();
        let chain4 = to_hex(&b.chain_root(4).unwrap());
        assert_eq!(b.classify(4, &chain4), HeadClass::Chain);
        // An unlisted size on the chain still classifies as chain: the leaf
        // hashes, not the size list, are what decide.
        let chain2 = to_hex(&b.chain_root(2).unwrap());
        assert_eq!(b.classify(2, &chain2), HeadClass::Chain);
        let fork = b.retired.forks[0].clone();
        assert!(matches!(b.classify(1, &fork.root_hash), HeadClass::ListedFork(_)));
        assert_eq!(b.classify(1, &to_hex(&[8u8; 32])), HeadClass::Unlisted);
        assert_eq!(b.classify(4, &fork.root_hash), HeadClass::Unlisted);
        assert_eq!(b.classify(50, &chain4), HeadClass::Unlisted);
    }
}
