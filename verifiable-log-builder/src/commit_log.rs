//! The **mls-commit-log** tenant: the canonical leaf encoding for an MLS
//! commit and the [`CommitLogInvariant`] that makes #357's per-conversation
//! rules globally auditable.
//!
//! This module is pure — no IO, no DB, no clock. The DB reading lives in
//! [`crate::source`]; everything here operates on already-read values so the
//! encoding and invariant can be unit-tested and reused (e.g. by a monitor)
//! without a database.

use serde::{Deserialize, Serialize};
use verifiable_log::{Entry, InvariantViolation, TenantInvariant};

/// Tenant id for the MLS commit log in the shared verifiable log.
pub const TENANT: &str = "mls-commit-log";

/// The canonical, frozen leaf payload committing to a single MLS commit's
/// identity. It deliberately stores `sha256(commit_data)` (hex), **never** the
/// raw TLS-serialised commit bytes: the leaf commits to the bytes without
/// disclosing or persisting them.
///
/// The on-the-wire leaf encoding is **compact JSON of this struct with fields
/// in exactly this declared order** (`conversation_id`, `epoch`, `sender_id`,
/// `seq`, `commit_sha256`). serde emits struct fields in declaration order with
/// no insignificant whitespace, so the encoding is deterministic and stable.
/// This is a frozen contract extension of `verifiable-log`'s leaf encoding —
/// see the builder README.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitLeaf {
    /// MLS conversation (group) id.
    pub conversation_id: String,
    /// MLS epoch *after* this commit.
    pub epoch: u64,
    /// Committer's user id. Recorded so a later slice can add cryptographic
    /// authorization ("was this sender entitled to commit at this epoch?");
    /// this slice does NOT validate it (that needs MLS group state).
    pub sender_id: String,
    /// `mls_commit_log.seq` — the log's global insertion order.
    pub seq: i64,
    /// `sha256(commit_data)`, lowercase hex (32 bytes). The raw blob is never
    /// stored in the leaf.
    pub commit_sha256: String,
}

impl CommitLeaf {
    /// Canonical leaf bytes: compact JSON in the struct's declared field order.
    /// Infallible in practice (all fields are plainly serialisable); a failure
    /// would only come from an allocator error, which we surface rather than
    /// unwrap.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Parse leaf bytes produced by [`Self::encode`] back into a `CommitLeaf`.
    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Build the tenant-tagged [`Entry`] for this leaf.
    pub fn to_entry(&self) -> Result<Entry, serde_json::Error> {
        Ok(Entry::new(TENANT, self.encode()?))
    }
}

/// The global, auditable form of #357. Registered for [`TENANT`] on the builder's
/// log, it is consulted on every append and enforces, per conversation:
///
/// * **(a) no fork** — no two entries share the same `(conversation_id, epoch)`;
/// * **(b) no epoch regression/replay** — within a conversation, `epoch` is
///   strictly increasing in `seq` order.
///
/// Because the builder appends commits in `seq` order, the candidate always has
/// the largest `seq` seen so far for its conversation, so "strictly greater than
/// every prior epoch for this conversation" is exactly rule (b).
///
/// Out of scope this slice: cryptographic authorization of the committer
/// (needs MLS group state). `sender_id` is recorded in the leaf for a later
/// slice; it is not validated here.
pub struct CommitLogInvariant;

impl CommitLogInvariant {
    /// A leaf whose bytes don't parse as a `CommitLeaf` can't be reasoned about,
    /// so it's treated as a violation rather than silently accepted.
    fn parse(entry: &Entry) -> Result<CommitLeaf, InvariantViolation> {
        CommitLeaf::decode(&entry.data).map_err(|e| {
            InvariantViolation::new(TENANT, format!("malformed commit-log leaf: {e}"))
        })
    }
}

impl TenantInvariant for CommitLogInvariant {
    fn check(
        &self,
        existing: &[&Entry],
        candidate: &Entry,
    ) -> Result<(), InvariantViolation> {
        let cand = Self::parse(candidate)?;
        for prev_entry in existing {
            let prev = Self::parse(prev_entry)?;
            if prev.conversation_id != cand.conversation_id {
                continue;
            }
            // (a) fork: same conversation + same epoch, two distinct commits.
            if prev.epoch == cand.epoch {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "fork in conversation `{}` at epoch {}: seq {} conflicts with seq {}",
                        cand.conversation_id, cand.epoch, cand.seq, prev.seq
                    ),
                ));
            }
            // (b) epoch regression/replay: a prior commit for this conversation
            // already reached a higher epoch, so this one goes backwards.
            if prev.epoch > cand.epoch {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "epoch regression in conversation `{}`: seq {} is epoch {} but seq {} already reached epoch {}",
                        cand.conversation_id, cand.seq, cand.epoch, prev.seq, prev.epoch
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

    fn leaf(conv: &str, epoch: u64, seq: i64) -> CommitLeaf {
        CommitLeaf {
            conversation_id: conv.to_string(),
            epoch,
            sender_id: "u-sender".to_string(),
            seq,
            commit_sha256: hex::encode([epoch as u8; 32]),
        }
    }

    #[test]
    fn encode_is_stable_and_roundtrips() {
        let l = leaf("conv-a", 3, 42);
        let bytes = l.encode().unwrap();
        // Field order is frozen: conversation_id, epoch, sender_id, seq, commit_sha256.
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(s.starts_with("{\"conversation_id\":"));
        assert!(s.find("\"epoch\":").unwrap() < s.find("\"sender_id\":").unwrap());
        assert!(s.find("\"sender_id\":").unwrap() < s.find("\"seq\":").unwrap());
        assert!(s.find("\"seq\":").unwrap() < s.find("\"commit_sha256\":").unwrap());
        assert_eq!(CommitLeaf::decode(&bytes).unwrap(), l);
    }

    #[test]
    fn accepts_strictly_increasing_epochs() {
        let inv = CommitLogInvariant;
        let e0 = leaf("a", 0, 1).to_entry().unwrap();
        let e1 = leaf("a", 1, 2).to_entry().unwrap();
        let eb = leaf("b", 0, 3).to_entry().unwrap();
        assert!(inv.check(&[], &e0).is_ok());
        assert!(inv.check(&[&e0], &e1).is_ok());
        // Different conversation at epoch 0 is fine.
        assert!(inv.check(&[&e0, &e1], &eb).is_ok());
    }

    #[test]
    fn rejects_fork() {
        let inv = CommitLogInvariant;
        let e0 = leaf("a", 0, 1).to_entry().unwrap();
        // Same conversation + same epoch, different commit bytes (different seq).
        let fork = leaf("a", 0, 2).to_entry().unwrap();
        let err = inv.check(&[&e0], &fork).unwrap_err();
        assert!(err.message.contains("fork"), "got: {}", err.message);
    }

    #[test]
    fn rejects_epoch_regression() {
        let inv = CommitLogInvariant;
        let e0 = leaf("a", 0, 1).to_entry().unwrap();
        let e1 = leaf("a", 5, 2).to_entry().unwrap();
        let back = leaf("a", 3, 3).to_entry().unwrap();
        let err = inv.check(&[&e0, &e1], &back).unwrap_err();
        assert!(err.message.contains("regression"), "got: {}", err.message);
    }
}
