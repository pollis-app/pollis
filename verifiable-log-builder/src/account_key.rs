//! The account-key tenant: the leaf encoding for one account identity-key
//! version and [`AccountKeyInvariant`]. Its own tree, signed under
//! [`STH_CONTEXT`]. Pure: no IO, DB or clock.

use serde::{Deserialize, Serialize};
use verifiable_log::{Entry, InvariantViolation, TenantInvariant};

pub const TENANT: &str = "account-key";

/// The account-key tree's STH context, re-exported from the core so it can't
/// drift.
pub use verifiable_log::STH_CONTEXT_ACCOUNT_KEYS as STH_CONTEXT;

/// Frozen leaf: which public key was authoritative for `user_id` at
/// `identity_version`. The key is public by design, so it is carried, not hashed.
///
/// Encoded as compact JSON in declared field order; do not reorder fields
/// (builder README).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyLeaf {
    pub user_id: String,
    /// 1 at signup, incremented on each `reset_identity`.
    pub identity_version: u64,
    /// Ed25519 account identity public key, lowercase hex.
    pub account_id_pub: String,
    pub seq: i64,
}

impl AccountKeyLeaf {
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    pub fn to_entry(&self) -> Result<Entry, serde_json::Error> {
        Ok(Entry::new(TENANT, self.encode()?))
    }
}

/// Per user: (a) no duplicate `identity_version` (mirrors the DB's UNIQUE
/// index), and (b) `identity_version` strictly increasing in `seq` order. Rows
/// are appended in `seq` order, so checking against every prior version is (b).
pub struct AccountKeyInvariant;

impl AccountKeyInvariant {
    /// An unparseable leaf is a violation, never silently accepted.
    fn parse(entry: &Entry) -> Result<AccountKeyLeaf, InvariantViolation> {
        AccountKeyLeaf::decode(&entry.data).map_err(|e| {
            InvariantViolation::new(TENANT, format!("malformed account-key leaf: {e}"))
        })
    }
}

impl TenantInvariant for AccountKeyInvariant {
    fn check(&self, existing: &[&Entry], candidate: &Entry) -> Result<(), InvariantViolation> {
        let cand = Self::parse(candidate)?;
        for prev_entry in existing {
            let prev = Self::parse(prev_entry)?;
            if prev.user_id != cand.user_id {
                continue;
            }
            // (a)
            if prev.identity_version == cand.identity_version {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "duplicate identity_version for user `{}`: version {} at seq {} conflicts with seq {}",
                        cand.user_id, cand.identity_version, cand.seq, prev.seq
                    ),
                ));
            }
            // (b)
            if prev.identity_version > cand.identity_version {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "identity_version regression for user `{}`: seq {} is version {} but seq {} already reached version {}",
                        cand.user_id, cand.seq, cand.identity_version, prev.seq, prev.identity_version
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(user: &str, version: u64, seq: i64) -> AccountKeyLeaf {
        AccountKeyLeaf {
            user_id: user.to_string(),
            identity_version: version,
            account_id_pub: hex::encode([version as u8; 32]),
            seq,
        }
    }

    #[test]
    fn encode_is_stable_and_roundtrips() {
        let l = leaf("u-alice", 2, 7);
        let bytes = l.encode().unwrap();
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(s.starts_with("{\"user_id\":"));
        assert!(s.find("\"identity_version\":").unwrap() < s.find("\"account_id_pub\":").unwrap());
        assert!(s.find("\"account_id_pub\":").unwrap() < s.find("\"seq\":").unwrap());
        assert_eq!(AccountKeyLeaf::decode(&bytes).unwrap(), l);
    }

    #[test]
    fn accepts_strictly_increasing_versions() {
        let inv = AccountKeyInvariant;
        let a1 = leaf("u-alice", 1, 1).to_entry().unwrap();
        let a2 = leaf("u-alice", 2, 3).to_entry().unwrap();
        let b1 = leaf("u-bob", 1, 2).to_entry().unwrap();
        assert!(inv.check(&[], &a1).is_ok());
        assert!(inv.check(&[&a1], &b1).is_ok());
        assert!(inv.check(&[&a1, &b1], &a2).is_ok());
    }

    #[test]
    fn rejects_duplicate_version() {
        let inv = AccountKeyInvariant;
        let a1 = leaf("u-alice", 1, 1).to_entry().unwrap();
        let dup = leaf("u-alice", 1, 2).to_entry().unwrap();
        let err = inv.check(&[&a1], &dup).unwrap_err();
        assert!(err.message.contains("duplicate"), "got: {}", err.message);
    }

    #[test]
    fn rejects_version_regression() {
        let inv = AccountKeyInvariant;
        let a1 = leaf("u-alice", 1, 1).to_entry().unwrap();
        let a5 = leaf("u-alice", 5, 2).to_entry().unwrap();
        let back = leaf("u-alice", 3, 3).to_entry().unwrap();
        let err = inv.check(&[&a1, &a5], &back).unwrap_err();
        assert!(err.message.contains("regression"), "got: {}", err.message);
    }
}
