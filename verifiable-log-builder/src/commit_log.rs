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

/// Is this the classic suite generation? Used to keep generation-0 leaves
/// encoding byte-for-byte as they did before #454 P4 — see [`CommitLeaf`].
fn is_generation_zero(generation: &u64) -> bool {
    *generation == 0
}

/// The canonical, frozen leaf payload committing to a single MLS commit's
/// identity. It deliberately stores `sha256(commit_data)` (hex), **never** the
/// raw TLS-serialised commit bytes: the leaf commits to the bytes without
/// disclosing or persisting them.
///
/// The on-the-wire leaf encoding is **compact JSON of this struct with fields
/// in exactly this declared order** (`conversation_id`, `generation`, `epoch`,
/// `sender_id`, `seq`, `commit_sha256`). serde emits struct fields in
/// declaration order with no insignificant whitespace, so the encoding is
/// deterministic and stable. This is a frozen contract extension of
/// `verifiable-log`'s leaf encoding — see the builder README.
///
/// `generation` (#454 P4) is omitted entirely when it is 0, so every leaf for a
/// conversation that has never migrated suite encodes byte-for-byte as it did
/// before P4. That matters more than tidiness: leaf bytes are hashed into the
/// Merkle tree, so re-encoding history under a new shape would invalidate every
/// signed root ever published, and every inclusion proof a client has cached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitLeaf {
    /// MLS conversation (group) id.
    pub conversation_id: String,
    /// Suite generation of the MLS lineage this commit belongs to (#454 P4).
    /// A conversation that has never migrated is generation 0; each hybrid-suite
    /// migration stands up a successor lineage at generation N+1, restarting at
    /// MLS epoch 0.
    #[serde(default, skip_serializing_if = "is_generation_zero")]
    pub generation: u64,
    /// MLS epoch *after* this commit, within [`Self::generation`]'s lineage.
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

/// The global, auditable form of #357, widened to suite generations by #454 P4.
/// Registered for [`TENANT`] on the builder's log, it is consulted on every
/// append and enforces, per conversation:
///
/// * **(a) no fork** — no two entries share the same
///   `(conversation_id, generation, epoch)`;
/// * **(b) no regression/replay** — within a conversation, `(generation, epoch)`
///   is strictly increasing **lexicographically** in `seq` order;
/// * **(c) a lineage opens at epoch 0** — the first commit of a generation
///   higher than any seen before must be at epoch 0.
///
/// Because the builder appends commits in `seq` order, the candidate always has
/// the largest `seq` seen so far for its conversation, so "strictly greater than
/// every prior `(generation, epoch)` for this conversation" is exactly rule (b).
///
/// ### Why the key is lexicographic, and why (c) has to exist
///
/// MLS binds the ciphersuite at group creation, so migrating a conversation to
/// the post-quantum hybrid suite cannot be a commit — it stands up a *successor*
/// group whose epoch counter restarts at 0. A scalar epoch key would therefore
/// read every migration as an epoch regression and flag honest history as a
/// violation. Ordering `(generation, epoch)` lexicographically fixes that: the
/// successor's epoch 0 sorts above the retired lineage's last epoch because its
/// generation is higher.
///
/// But (b) alone is now weak, because a server could open generation N+1 while
/// generation N was still at epoch 5 and then keep appending to neither — or
/// open it at a fabricated mid-lineage epoch to make a fork look like a
/// migration. (c) closes that: a lineage can only ever *begin*, and it begins at
/// its natural start. Together (b) and (c) prove the lineages of a conversation
/// are contiguous, non-overlapping, and totally ordered — without the leaf
/// needing to carry the predecessor's closing epoch, which would change the
/// frozen encoding for every conversation, migrated or not.
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
        let cand_key = (cand.generation, cand.epoch);
        // The highest generation this conversation has ever reached. Tracked
        // across the whole scan rather than checked per-pair, because rule (c)
        // is about the candidate versus *every* prior entry at once: opening a
        // lineage is only legal when nothing has opened it before.
        let mut max_generation: Option<u64> = None;
        for prev_entry in existing {
            let prev = Self::parse(prev_entry)?;
            if prev.conversation_id != cand.conversation_id {
                continue;
            }
            let prev_key = (prev.generation, prev.epoch);
            max_generation = Some(max_generation.map_or(prev.generation, |m| m.max(prev.generation)));
            // (a) fork: same conversation + same lineage + same epoch, two
            // distinct commits.
            if prev_key == cand_key {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "fork in conversation `{}` at generation {} epoch {}: seq {} conflicts with seq {}",
                        cand.conversation_id, cand.generation, cand.epoch, cand.seq, prev.seq
                    ),
                ));
            }
            // (b) regression/replay: a prior commit for this conversation
            // already reached a higher `(generation, epoch)`, so this one goes
            // backwards — either within a lineage, or back onto one the
            // conversation has already retired.
            if prev_key > cand_key {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "epoch regression in conversation `{}`: seq {} is generation {} epoch {} but seq {} already reached generation {} epoch {}",
                        cand.conversation_id,
                        cand.seq,
                        cand.generation,
                        cand.epoch,
                        prev.seq,
                        prev.generation,
                        prev.epoch
                    ),
                ));
            }
        }
        // (c) a lineage opens at epoch 0. Only checked when the candidate is
        // genuinely opening one — i.e. its generation is above everything this
        // conversation has recorded. The very first commit of a conversation
        // (no prior entries at all) is exempt: the builder is appending an
        // already-existing history in `seq` order and may legitimately start
        // mid-lineage after a log prune.
        if let Some(max_generation) = max_generation {
            if cand.generation > max_generation && cand.epoch != 0 {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "conversation `{}` opens generation {} at epoch {} (seq {}): a successor lineage must start at epoch 0",
                        cand.conversation_id, cand.generation, cand.epoch, cand.seq
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
        leaf_at(conv, 0, epoch, seq)
    }

    fn leaf_at(conv: &str, generation: u64, epoch: u64, seq: i64) -> CommitLeaf {
        CommitLeaf {
            conversation_id: conv.to_string(),
            generation,
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
        // Field order is frozen: conversation_id, generation, epoch, sender_id,
        // seq, commit_sha256.
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(s.starts_with("{\"conversation_id\":"));
        assert!(s.find("\"epoch\":").unwrap() < s.find("\"sender_id\":").unwrap());
        assert!(s.find("\"sender_id\":").unwrap() < s.find("\"seq\":").unwrap());
        assert!(s.find("\"seq\":").unwrap() < s.find("\"commit_sha256\":").unwrap());
        assert_eq!(CommitLeaf::decode(&bytes).unwrap(), l);
    }

    /// The compatibility contract that lets #454 P4 ship without invalidating
    /// published roots: a generation-0 leaf encodes with no `generation` field
    /// at all, so its bytes — and therefore its Merkle hash, and every inclusion
    /// proof over it — are identical to what the pre-P4 builder emitted.
    #[test]
    fn generation_zero_is_omitted_from_the_encoding() {
        let bytes = leaf("conv-a", 3, 42).encode().unwrap();
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert_eq!(
            s,
            "{\"conversation_id\":\"conv-a\",\"epoch\":3,\"sender_id\":\"u-sender\",\"seq\":42,\
             \"commit_sha256\":\"0303030303030303030303030303030303030303030303030303030303030303\"}"
        );
        // And a pre-P4 leaf decodes as generation 0 rather than failing.
        assert_eq!(CommitLeaf::decode(&bytes).unwrap().generation, 0);
    }

    #[test]
    fn a_successor_generation_is_carried_in_the_encoding() {
        let l = leaf_at("conv-a", 1, 0, 43);
        let bytes = l.encode().unwrap();
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(s.contains("\"generation\":1"), "got: {s}");
        assert!(s.find("\"conversation_id\":").unwrap() < s.find("\"generation\":").unwrap());
        assert!(s.find("\"generation\":").unwrap() < s.find("\"epoch\":").unwrap());
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

    /// The #454 P4 property a scalar epoch key could not express: a successor
    /// lineage restarts at MLS epoch 0, numerically *below* the retired
    /// lineage's last epoch. Before the key widened, an honest suite migration
    /// would have been reported to every auditor as an epoch regression.
    #[test]
    fn a_suite_migration_is_not_a_regression() {
        let inv = CommitLogInvariant;
        let e0 = leaf("a", 0, 1).to_entry().unwrap();
        let e5 = leaf("a", 5, 2).to_entry().unwrap();
        let successor = leaf_at("a", 1, 0, 3).to_entry().unwrap();
        assert!(inv.check(&[&e0, &e5], &successor).is_ok());
        // …and the successor lineage then advances normally.
        let successor1 = leaf_at("a", 1, 1, 4).to_entry().unwrap();
        assert!(inv.check(&[&e0, &e5, &successor], &successor1).is_ok());
    }

    /// A conversation may not be dragged back onto a lineage it has retired:
    /// the closed generation's keys are gone from every honest device, so a
    /// commit appearing there is a server rewriting history, not a late writer.
    #[test]
    fn rejects_a_commit_on_a_retired_lineage() {
        let inv = CommitLogInvariant;
        let e5 = leaf("a", 5, 1).to_entry().unwrap();
        let successor = leaf_at("a", 1, 0, 2).to_entry().unwrap();
        let back = leaf("a", 6, 3).to_entry().unwrap();
        let err = inv.check(&[&e5, &successor], &back).unwrap_err();
        assert!(err.message.contains("regression"), "got: {}", err.message);
    }

    /// Two distinct commits at the same `(generation, epoch)` are still a fork —
    /// widening the key must not have opened a hole at the same epoch number.
    #[test]
    fn rejects_a_fork_inside_a_successor_lineage() {
        let inv = CommitLogInvariant;
        let g1 = leaf_at("a", 1, 0, 1).to_entry().unwrap();
        let fork = leaf_at("a", 1, 0, 2).to_entry().unwrap();
        let err = inv.check(&[&g1], &fork).unwrap_err();
        assert!(err.message.contains("fork"), "got: {}", err.message);
    }

    /// Rule (c). Without it, a server could disguise a fork as a migration:
    /// bump the generation and pick any epoch it likes, and rule (b) — which
    /// only ever looks *up* — would wave it through. Requiring the opening
    /// commit to sit at epoch 0 makes a lineage's start unforgeable.
    #[test]
    fn rejects_a_lineage_that_opens_mid_stream() {
        let inv = CommitLogInvariant;
        let e5 = leaf("a", 5, 1).to_entry().unwrap();
        let bogus = leaf_at("a", 1, 9, 2).to_entry().unwrap();
        let err = inv.check(&[&e5], &bogus).unwrap_err();
        assert!(err.message.contains("must start at epoch 0"), "got: {}", err.message);
    }

    /// Rule (c) applies only to the commit that *opens* a lineage. The builder
    /// appends existing history in `seq` order and, after a commit-log prune,
    /// may legitimately begin mid-lineage — so a conversation's very first
    /// observed entry is exempt whatever its generation and epoch.
    #[test]
    fn a_pruned_history_may_begin_mid_lineage() {
        let inv = CommitLogInvariant;
        let first = leaf_at("a", 2, 7, 1).to_entry().unwrap();
        assert!(inv.check(&[], &first).is_ok());
        let next = leaf_at("a", 2, 8, 2).to_entry().unwrap();
        assert!(inv.check(&[&first], &next).is_ok());
    }

    /// Lineages are per conversation: one conversation migrating says nothing
    /// about another, so generation 0 entries elsewhere stay legal.
    #[test]
    fn generations_are_scoped_to_a_conversation() {
        let inv = CommitLogInvariant;
        let a1 = leaf_at("a", 1, 0, 1).to_entry().unwrap();
        let b0 = leaf("b", 0, 2).to_entry().unwrap();
        assert!(inv.check(&[&a1], &b0).is_ok());
    }
}
