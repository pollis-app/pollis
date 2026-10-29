//! The mls-commit-log tenant: the leaf encoding for an MLS commit and
//! [`CommitLogInvariant`], the auditable form of #357. Pure: no IO, DB or clock.
//!
//! Leaves carry windowed pseudonyms, not raw ids (#701), so the public log can't
//! be scraped into an activity map. Design: `docs/transparency.md`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use verifiable_log::{Entry, InvariantViolation, TenantInvariant};

pub const TENANT: &str = "mls-commit-log";

/// Bumping the version invalidates every pseudonym: only at a full republish.
const CONVERSATION_PSEUDONYM_DOMAIN: &[u8] =
    b"pollis-verifiable-log:commit-pseudonym:conversation:v1";

const SENDER_PSEUDONYM_DOMAIN: &[u8] = b"pollis-verifiable-log:commit-pseudonym:sender:v1";

/// Global `seq` values per pseudonym window: larger strengthens the public
/// invariant, smaller strengthens unlinkability. Baked into the frozen leaf, so
/// it changes only at a full republish. See `docs/transparency.md`.
pub const PSEUDONYM_WINDOW_SIZE: u64 = 1024;

/// Derived from the leaf's own `seq`, so members need no extra published field.
pub fn window_for_seq(seq: i64) -> u64 {
    (seq.max(0) as u64) / PSEUDONYM_WINDOW_SIZE
}

/// Length-framed SHA-256, so no two distinct field lists collide by
/// concatenation.
fn hash_framed(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_le_bytes());
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

/// `H(domain || conversation_id || window)`. Keyless: the only secret is
/// `conversation_id`, which members know and log scrapers don't.
pub fn derive_conversation_pseudonym(conversation_id: &str, window: u64) -> String {
    hash_framed(
        CONVERSATION_PSEUDONYM_DOMAIN,
        &[conversation_id.as_bytes(), &window.to_le_bytes()],
    )
}

/// `H(domain || conversation_id || sender_id || window)`. Bound to the
/// conversation so a user can't be followed across groups.
pub fn derive_sender_pseudonym(conversation_id: &str, sender_id: &str, window: u64) -> String {
    hash_framed(
        SENDER_PSEUDONYM_DOMAIN,
        &[
            conversation_id.as_bytes(),
            sender_id.as_bytes(),
            &window.to_le_bytes(),
        ],
    )
}

/// Frozen leaf committing to one MLS commit: `sha256(commit_data)`, never the
/// commit bytes.
///
/// Encoded as compact JSON in declared field order (builder README). Changing
/// the shape invalidates every published root and proof, so it happens only at
/// a full republish. `generation` is omitted when 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitLeaf {
    /// [`derive_conversation_pseudonym`].
    pub conversation_pseudonym: String,
    /// Suite generation of the commit's lineage (#454). Each suite migration
    /// starts a successor lineage at N+1, restarting at epoch 0.
    #[serde(default, skip_serializing_if = "is_generation_zero")]
    pub generation: u64,
    /// MLS epoch after this commit, within [`Self::generation`]'s lineage.
    pub epoch: u64,
    /// [`derive_sender_pseudonym`]. Not validated: that needs MLS group state.
    pub sender_pseudonym: String,
    /// Global insertion order; also drives the window.
    pub seq: i64,
    pub commit_sha256: String,
}

fn is_generation_zero(generation: &u64) -> bool {
    *generation == 0
}

impl CommitLeaf {
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

/// Per grouping key (`conversation_pseudonym`, i.e. same conversation and
/// window), on every append:
///
/// * **(a) no fork**: no two entries share `(generation, epoch)`;
/// * **(b) no regression**: `(generation, epoch)` strictly increases
///   lexicographically in `seq` order, since a suite migration restarts the
///   epoch at 0 under a higher generation;
/// * **(c) a lineage opens at epoch 0**, so a fork can't pose as a migration.
///
/// A fork straddling a window boundary escapes this public check by design
/// (re-linking windows would undo the unlinkability); members, who know the real
/// id, catch it. See `docs/transparency.md`.
pub struct CommitLogInvariant;

impl CommitLogInvariant {
    /// An unparseable leaf is a violation, never silently accepted.
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
        // Tracked across the whole scan: (c) compares against every prior entry.
        let mut max_generation: Option<u64> = None;
        for prev_entry in existing {
            let prev = Self::parse(prev_entry)?;
            if prev.conversation_pseudonym != cand.conversation_pseudonym {
                continue;
            }
            let prev_key = (prev.generation, prev.epoch);
            max_generation = Some(max_generation.map_or(prev.generation, |m| m.max(prev.generation)));
            // (a)
            if prev_key == cand_key {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "fork in conversation `{}` at generation {} epoch {}: seq {} conflicts with seq {}",
                        cand.conversation_pseudonym, cand.generation, cand.epoch, cand.seq, prev.seq
                    ),
                ));
            }
            // (b)
            if prev_key > cand_key {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "epoch regression in conversation `{}`: seq {} is generation {} epoch {} but seq {} already reached generation {} epoch {}",
                        cand.conversation_pseudonym,
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
        // (c). A grouping key's first commit is exempt: history may start
        // mid-lineage after a prune, or because an earlier window held it.
        if let Some(max_generation) = max_generation {
            if cand.generation > max_generation && cand.epoch != 0 {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "conversation `{}` opens generation {} at epoch {} (seq {}): a successor lineage must start at epoch 0",
                        cand.conversation_pseudonym, cand.generation, cand.epoch, cand.seq
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

    const W: i64 = PSEUDONYM_WINDOW_SIZE as i64;

    fn leaf(conv: &str, epoch: u64, seq: i64) -> CommitLeaf {
        leaf_at(conv, 0, epoch, seq)
    }

    fn leaf_at(conv: &str, generation: u64, epoch: u64, seq: i64) -> CommitLeaf {
        let window = window_for_seq(seq);
        CommitLeaf {
            conversation_pseudonym: derive_conversation_pseudonym(conv, window),
            generation,
            epoch,
            sender_pseudonym: derive_sender_pseudonym(conv, "u-sender", window),
            seq,
            commit_sha256: hex::encode([epoch as u8; 32]),
        }
    }

    #[test]
    fn encode_is_stable_and_roundtrips() {
        let l = leaf("conv-a", 3, 42);
        let bytes = l.encode().unwrap();
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(s.starts_with("{\"conversation_pseudonym\":"));
        assert!(s.find("\"epoch\":").unwrap() < s.find("\"sender_pseudonym\":").unwrap());
        assert!(s.find("\"sender_pseudonym\":").unwrap() < s.find("\"seq\":").unwrap());
        assert!(s.find("\"seq\":").unwrap() < s.find("\"commit_sha256\":").unwrap());
        assert_eq!(CommitLeaf::decode(&bytes).unwrap(), l);
    }

    #[test]
    fn generation_zero_is_omitted_from_the_encoding() {
        let bytes = leaf("conv-a", 3, 42).encode().unwrap();
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(!s.contains("\"generation\""), "generation 0 must be omitted: {s}");
        assert_eq!(CommitLeaf::decode(&bytes).unwrap().generation, 0);
    }

    #[test]
    fn a_successor_generation_is_carried_in_the_encoding() {
        let l = leaf_at("conv-a", 1, 0, 43);
        let bytes = l.encode().unwrap();
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(s.contains("\"generation\":1"), "got: {s}");
        assert!(
            s.find("\"conversation_pseudonym\":").unwrap() < s.find("\"generation\":").unwrap()
        );
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
        assert!(inv.check(&[&e0, &e1], &eb).is_ok());
    }

    #[test]
    fn rejects_fork_within_a_window() {
        let inv = CommitLogInvariant;
        let e0 = leaf("a", 0, 1).to_entry().unwrap();
        let fork = leaf("a", 0, 2).to_entry().unwrap();
        assert_eq!(
            CommitLeaf::decode(&e0.data).unwrap().conversation_pseudonym,
            CommitLeaf::decode(&fork.data).unwrap().conversation_pseudonym,
            "same conversation in the same window must share a pseudonym"
        );
        let err = inv.check(&[&e0], &fork).unwrap_err();
        assert!(err.message.contains("fork"), "got: {}", err.message);
    }

    /// Documents the residual: members catch this (serve-layer group tests).
    #[test]
    fn a_fork_across_a_window_boundary_escapes_public_grouping() {
        let inv = CommitLogInvariant;
        let e0 = leaf("a", 0, 1).to_entry().unwrap();
        let fork_next_window = leaf("a", 0, W + 1).to_entry().unwrap();
        assert_ne!(
            CommitLeaf::decode(&e0.data).unwrap().conversation_pseudonym,
            CommitLeaf::decode(&fork_next_window.data).unwrap().conversation_pseudonym,
            "the boundary must rotate the pseudonym"
        );
        assert!(inv.check(&[&e0], &fork_next_window).is_ok());
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

    #[test]
    fn a_suite_migration_is_not_a_regression() {
        let inv = CommitLogInvariant;
        let e0 = leaf("a", 0, 1).to_entry().unwrap();
        let e5 = leaf("a", 5, 2).to_entry().unwrap();
        let successor = leaf_at("a", 1, 0, 3).to_entry().unwrap();
        assert!(inv.check(&[&e0, &e5], &successor).is_ok());
        let successor1 = leaf_at("a", 1, 1, 4).to_entry().unwrap();
        assert!(inv.check(&[&e0, &e5, &successor], &successor1).is_ok());
    }

    #[test]
    fn rejects_a_fork_inside_a_successor_lineage() {
        let inv = CommitLogInvariant;
        let g1 = leaf_at("a", 1, 0, 1).to_entry().unwrap();
        let fork = leaf_at("a", 1, 0, 2).to_entry().unwrap();
        let err = inv.check(&[&g1], &fork).unwrap_err();
        assert!(err.message.contains("fork"), "got: {}", err.message);
    }

    #[test]
    fn rejects_a_lineage_that_opens_mid_stream() {
        let inv = CommitLogInvariant;
        let e5 = leaf("a", 5, 1).to_entry().unwrap();
        let bogus = leaf_at("a", 1, 9, 2).to_entry().unwrap();
        let err = inv.check(&[&e5], &bogus).unwrap_err();
        assert!(err.message.contains("must start at epoch 0"), "got: {}", err.message);
    }

    #[test]
    fn a_pruned_history_may_begin_mid_lineage() {
        let inv = CommitLogInvariant;
        let first = leaf_at("a", 2, 7, 1).to_entry().unwrap();
        assert!(inv.check(&[], &first).is_ok());
        let next = leaf_at("a", 2, 8, 2).to_entry().unwrap();
        assert!(inv.check(&[&first], &next).is_ok());
    }

    #[test]
    fn generations_are_scoped_to_a_conversation() {
        let inv = CommitLogInvariant;
        let a1 = leaf_at("a", 1, 0, 1).to_entry().unwrap();
        let b0 = leaf("b", 0, 2).to_entry().unwrap();
        assert!(inv.check(&[&a1], &b0).is_ok());
    }

    #[test]
    fn a_conversation_is_unlinkable_across_windows() {
        let w0 = derive_conversation_pseudonym("conv-a", 0);
        let w1 = derive_conversation_pseudonym("conv-a", 1);
        assert_ne!(w0, w1, "the pseudonym must rotate across windows");
        assert_eq!(w0, derive_conversation_pseudonym("conv-a", 0));
    }

    #[test]
    fn a_sender_is_unlinkable_across_conversations() {
        let in_a = derive_sender_pseudonym("conv-a", "u-alice", 0);
        let in_b = derive_sender_pseudonym("conv-b", "u-alice", 0);
        assert_ne!(in_a, in_b, "the same user must not be linkable across conversations");
        let in_a_later = derive_sender_pseudonym("conv-a", "u-alice", 1);
        assert_ne!(in_a, in_a_later);
    }

    #[test]
    fn a_member_rederives_the_published_pseudonym_from_seq() {
        let l = leaf_at("conv-a", 0, 4, W + 7);
        assert_eq!(
            l.conversation_pseudonym,
            derive_conversation_pseudonym("conv-a", window_for_seq(l.seq))
        );
    }
}
