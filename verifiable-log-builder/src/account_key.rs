//! The **account-key** tenant: the canonical leaf encoding for one account
//! identity-key version and the [`AccountKeyInvariant`] that makes the account
//! key directory's per-user rules globally auditable.
//!
//! This is the second tenant of the transparency log (the first is
//! [`crate::commit_log`]). Per the locked design it gets its **own** Merkle tree
//! and its **own** STH — account-key entries are never interleaved into the
//! commit-log tree — and that tree's STHs are signed under a domain-separated
//! context ([`STH_CONTEXT`]) so an account-key head can never be presented as a
//! commit-log head.
//!
//! Like [`crate::commit_log`] this module is pure — no IO, no DB, no clock. The
//! DB reading lives in [`crate::source`]; everything here operates on
//! already-read values so the encoding and invariant can be unit-tested and
//! reused (e.g. by a monitor) without a database.

use serde::{Deserialize, Serialize};
use verifiable_log::{Entry, InvariantViolation, TenantInvariant};

/// Tenant id for the account-key directory in the shared verifiable log.
pub const TENANT: &str = "account-key";

/// Domain-separation context for the account-key tree's Signed Tree Heads.
///
/// It extends the commit-log's frozen `pollis-verifiable-log:sth:v1` with an
/// `:account-keys` suffix. The commit-log context must NOT change (continuity of
/// already-published STHs); this distinct context guarantees an STH signed for
/// one tree fails verification against the other even though both use the same
/// Ed25519 key. Verified via [`verifiable_log::Sth::verify_with_context`].
pub const STH_CONTEXT: &[u8] = b"pollis-verifiable-log:sth:v1:account-keys";

/// The canonical, frozen leaf payload committing to a single account
/// identity-key version: which public key was authoritative for `user_id` at
/// `identity_version`.
///
/// Unlike a commit leaf (which stores `sha256(commit_data)` to avoid disclosing
/// the commit bytes), the account public key is **public by design** — the whole
/// point of an account-key directory is to publish it — so `account_id_pub`
/// carries the key itself, lowercase hex.
///
/// The on-the-wire leaf encoding is **compact JSON of this struct with fields in
/// exactly this declared order** (`user_id`, `identity_version`,
/// `account_id_pub`, `seq`). serde emits struct fields in declaration order with
/// no insignificant whitespace, so the encoding is deterministic and stable.
/// This is a frozen contract extension of `verifiable-log`'s leaf encoding —
/// see the builder README.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyLeaf {
    /// The user whose account identity key this row records.
    pub user_id: String,
    /// Monotonic identity version: 1 at signup, incremented on each rotation
    /// (`reset_identity`).
    pub identity_version: u64,
    /// The Ed25519 account identity public key authoritative at this version,
    /// lowercase hex.
    pub account_id_pub: String,
    /// `account_key_log.seq` — the log's global insertion order.
    pub seq: i64,
}

impl AccountKeyLeaf {
    /// Canonical leaf bytes: compact JSON in the struct's declared field order.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Parse leaf bytes produced by [`Self::encode`] back into an `AccountKeyLeaf`.
    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Build the tenant-tagged [`Entry`] for this leaf.
    pub fn to_entry(&self) -> Result<Entry, serde_json::Error> {
        Ok(Entry::new(TENANT, self.encode()?))
    }
}

/// The globally-auditable form of the account-key directory's per-user rule.
/// Registered for [`TENANT`] on the builder's log, it is consulted on every
/// append and enforces, per user:
///
/// * **(a) no duplicate version** — no two entries share the same
///   `(user_id, identity_version)` (the public mirror of the DB's
///   `UNIQUE(user_id, identity_version)` index);
/// * **(b) no version regression/replay** — within a user, `identity_version` is
///   strictly increasing in `seq` order.
///
/// Because the builder appends rows in `seq` order, the candidate always has the
/// largest `seq` seen so far for its user, so "strictly greater than every prior
/// version for this user" is exactly rule (b).
pub struct AccountKeyInvariant;

impl AccountKeyInvariant {
    /// A leaf whose bytes don't parse as an `AccountKeyLeaf` can't be reasoned
    /// about, so it's treated as a violation rather than silently accepted.
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
            // (a) duplicate version: same user + same identity_version, two rows.
            if prev.identity_version == cand.identity_version {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "duplicate identity_version for user `{}`: version {} at seq {} conflicts with seq {}",
                        cand.user_id, cand.identity_version, cand.seq, prev.seq
                    ),
                ));
            }
            // (b) version regression/replay: a prior row for this user already
            // reached a higher version, so this one goes backwards.
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
        // Field order is frozen: user_id, identity_version, account_id_pub, seq.
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
        // A different user at version 1 is fine.
        assert!(inv.check(&[&a1], &b1).is_ok());
        assert!(inv.check(&[&a1, &b1], &a2).is_ok());
    }

    #[test]
    fn rejects_duplicate_version() {
        let inv = AccountKeyInvariant;
        let a1 = leaf("u-alice", 1, 1).to_entry().unwrap();
        // Same user + same version, different key bytes (different seq).
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
