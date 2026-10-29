//! Signed reset statements: the log's own record that a tree was retired and
//! replaced (#1579, `docs/transparency.md`).
//!
//! A silent restart would be indistinguishable from discarding history. The
//! statement lists every served head of the retired tree; the tree is frozen, so
//! any unlisted head is a failure. It is signed by the online key that signed the
//! inconsistent heads (the root is offline, #754), binding that key to one
//! account of its own history.
//!
//! Signed bytes: `RESET_DOMAIN || len(u64 BE) || compact JSON body`. Unknown
//! fields are refused, so re-serialising a body is canonical.

use ml_dsa::{EncodedSignature, Keypair, MlDsa44, Signer, Verifier};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::{from_hex, to_hex, Hash};
use crate::merkle;
use crate::sth::{key_id_for, SigningKey, Sth, VerifyingKey};

/// Distinct from every STH context and from the key-set domain.
pub const RESET_DOMAIN: &[u8] = b"pollis-verifiable-log:reset:v1";

/// Wire format version of [`ResetBody`]; an unknown version is refused.
pub const RESET_FORMAT_VERSION: u32 = 1;

/// A head of the retired tree that is listed by size and root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListedHead {
    pub tree_size: u64,
    pub root_hash: String,
    pub timestamp: u64,
    pub note: String,
}

/// The tree being retired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetiredTree {
    /// Tree name, as in [`crate::sth::STH_CONTEXTS`].
    pub tree: String,
    pub sth_context: String,
    /// Relative to the log's base URL.
    pub path: String,
    /// The newest head that is consistent with every earlier head on its chain.
    pub last_consistent: Sth,
    /// Leaf hashes of the leaves `last_consistent` commits to, so every earlier
    /// chain root is recomputable without the (partly gone) leaf data.
    pub leaf_hashes: Vec<String>,
    /// Every served head size on the consistent chain, ascending. Each head's root
    /// is the Merkle root of that prefix of [`Self::leaf_hashes`].
    pub chain_sizes: Vec<u64>,
    /// Heads signed during the incident that are not on the consistent chain.
    pub forks: Vec<ListedHead>,
    /// Heads left over from an earlier generation of the tree, still served.
    pub superseded: Vec<ListedHead>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuccessorTree {
    pub tree: String,
    pub sth_context: String,
    pub path: String,
    /// Root of the successor's head at size 1: the hash of its first leaf.
    pub genesis_root_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetBody {
    pub format_version: u32,
    pub issued_at_ms: u64,
    pub reason: String,
    pub retired: RetiredTree,
    pub successor: SuccessorTree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetStatement {
    pub statement: ResetBody,
    pub signature: String,
    /// Unsigned selection hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
}

/// How a served head of the retired tree relates to the statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadClass<'a> {
    Chain,
    ListedFork(&'a ListedHead),
    Superseded(&'a ListedHead),
    /// Always a failure.
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
    pub fn chain_leaves(&self) -> Result<Vec<Hash>> {
        self.retired
            .leaf_hashes
            .iter()
            .map(|h| from_hex(h))
            .collect()
    }

    /// Check the statement against itself only, not against what is served.
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

    pub fn chain_root(&self, size: u64) -> Option<Hash> {
        let leaves = self.chain_leaves().ok()?;
        if size == 0 || size as usize > leaves.len() {
            return None;
        }
        Some(merkle::root_hash(&leaves[..size as usize]))
    }

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
        // The leaf hashes, not the size list, decide chain membership.
        let chain2 = to_hex(&b.chain_root(2).unwrap());
        assert_eq!(b.classify(2, &chain2), HeadClass::Chain);
        let fork = b.retired.forks[0].clone();
        assert!(matches!(b.classify(1, &fork.root_hash), HeadClass::ListedFork(_)));
        assert_eq!(b.classify(1, &to_hex(&[8u8; 32])), HeadClass::Unlisted);
        assert_eq!(b.classify(4, &fork.root_hash), HeadClass::Unlisted);
        assert_eq!(b.classify(50, &chain4), HeadClass::Unlisted);
    }
}
