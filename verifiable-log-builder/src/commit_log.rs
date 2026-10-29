//! The **mls-commit-log** tenant: the canonical leaf encoding for an MLS
//! commit and the [`CommitLogInvariant`] (#357). Pure: no IO, DB or clock.
//! Leaves carry windowed pseudonyms, not raw ids, so the public log is no activity map (#701).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use verifiable_log::{Entry, InvariantViolation, TenantInvariant};

/// Tenant id for the MLS commit log in the shared verifiable log.
pub const TENANT: &str = "mls-commit-log";

/// Domain-separation tag for the conversation pseudonym; bumping it forces a full republish.
const CONVERSATION_PSEUDONYM_DOMAIN: &[u8] =
    b"pollis-verifiable-log:commit-pseudonym:conversation:v1";

/// Sender pseudonym domain; bound to the conversation so users are unlinkable across groups.
const SENDER_PSEUDONYM_DOMAIN: &[u8] = b"pollis-verifiable-log:commit-pseudonym:sender:v1";

/// How many global `seq` values fall in one pseudonym window.
/// Pseudonyms are stable within a window and rotate at each boundary. Baked into the frozen leaf.
pub const PSEUDONYM_WINDOW_SIZE: u64 = 1024;

/// The window index a given `seq` falls in. Derivable from the published leaf's
/// own `seq`, so a member can recompute a leaf's pseudonym with no extra field.
pub fn window_for_seq(seq: i64) -> u64 {
    (seq.max(0) as u64) / PSEUDONYM_WINDOW_SIZE
}

/// Keyless, length-framed `SHA-256` over a domain tag and a list of fields,
/// lowercase hex. Every part is length-prefixed so field lists cannot collide.
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

/// The windowed conversation pseudonym: `H(domain || conversation_id || window)`.
/// Keyless: the only secret is `conversation_id`, unknown to a third-party log scraper.
pub fn derive_conversation_pseudonym(conversation_id: &str, window: u64) -> String {
    hash_framed(
        CONVERSATION_PSEUDONYM_DOMAIN,
        &[conversation_id.as_bytes(), &window.to_le_bytes()],
    )
}

/// The windowed, conversation-bound sender pseudonym:
/// `H(domain || conversation_id || sender_id || window)`.
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

/// The canonical, frozen leaf payload committing to a single MLS commit's
/// identity. Stores `sha256(commit_data)`, never the raw commit bytes.
///
/// Encoded as compact JSON in declared field order; changing the shape forces a full republish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitLeaf {
    /// Windowed pseudonym for the MLS conversation (group) —
    /// [`derive_conversation_pseudonym`].
    pub conversation_pseudonym: String,
    /// Suite generation of the MLS lineage this commit belongs to (#454 P4).
    /// Each hybrid-suite migration starts lineage N+1 at MLS epoch 0.
    #[serde(default, skip_serializing_if = "is_generation_zero")]
    pub generation: u64,
    /// MLS epoch *after* this commit, within [`Self::generation`]'s lineage.
    pub epoch: u64,
    /// Windowed, conversation-bound pseudonym for the committer —
    /// [`derive_sender_pseudonym`]. Not validated (that needs MLS group state).
    pub sender_pseudonym: String,
    /// `mls_commit_log.seq` — the log's global insertion order. Also the window
    /// driver: `window = seq / PSEUDONYM_WINDOW_SIZE`.
    pub seq: i64,
    /// `sha256(commit_data)`, lowercase hex (32 bytes).
    pub commit_sha256: String,
}

/// Generation-0 leaves carry no `generation` field.
fn is_generation_zero(generation: &u64) -> bool {
    *generation == 0
}

impl CommitLeaf {
    /// Canonical leaf bytes: compact JSON in the struct's declared field order.
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
/// Registered for [`TENANT`], it enforces per **grouping key** (`conversation_pseudonym`):
///
/// * **(a) no fork** — no two entries share the same
///   `(conversation_pseudonym, generation, epoch)`;
/// * **(b) no regression/replay** — within a grouping key, `(generation, epoch)`
///   is strictly increasing **lexicographically** in `seq` order;
/// * **(c) a lineage opens at epoch 0** — the first commit of a generation
///   higher than any seen before must be at epoch 0.
///
/// A fork straddling a window boundary is not publicly detectable; members holding
/// the real id still catch it. Generation-first ordering keeps migrations valid.
pub struct CommitLogInvariant;

impl CommitLogInvariant {
    /// An unparseable leaf is a violation, not silently accepted.
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
        // Tracked across the whole scan: rule (c) compares the candidate with every prior entry.
        let mut max_generation: Option<u64> = None;
        for prev_entry in existing {
            let prev = Self::parse(prev_entry)?;
            if prev.conversation_pseudonym != cand.conversation_pseudonym {
                continue;
            }
            let prev_key = (prev.generation, prev.epoch);
            max_generation = Some(max_generation.map_or(prev.generation, |m| m.max(prev.generation)));
            // (a) fork: same grouping key, lineage and epoch, two distinct commits.
            if prev_key == cand_key {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "fork in conversation `{}` at generation {} epoch {}: seq {} conflicts with seq {}",
                        cand.conversation_pseudonym, cand.generation, cand.epoch, cand.seq, prev.seq
                    ),
                ));
            }
            // (b) regression/replay: a prior commit already reached a higher `(generation, epoch)`.
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
        // (c) a lineage opens at epoch 0. A grouping key's first entry is exempt (prunes,
        // windowing).
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

    /// A published leaf for `conv` at `seq`, grouped by the windowed conversation pseudonym.
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

    /// A fork within a window is rejected by a public replay grouping only by pseudonym.
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

    /// The named residual: a fork straddling a window boundary is not caught by a public replay.
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

    /// The same conversation gets an unrelated pseudonym in a different window.
    #[test]
    fn a_conversation_is_unlinkable_across_windows() {
        let w0 = derive_conversation_pseudonym("conv-a", 0);
        let w1 = derive_conversation_pseudonym("conv-a", 1);
        assert_ne!(w0, w1, "the pseudonym must rotate across windows");
        assert_eq!(w0, derive_conversation_pseudonym("conv-a", 0));
    }

    /// The same user gets an unrelated pseudonym in a different conversation.
    #[test]
    fn a_sender_is_unlinkable_across_conversations() {
        let in_a = derive_sender_pseudonym("conv-a", "u-alice", 0);
        let in_b = derive_sender_pseudonym("conv-b", "u-alice", 0);
        assert_ne!(in_a, in_b, "the same user must not be linkable across conversations");
        let in_a_later = derive_sender_pseudonym("conv-a", "u-alice", 1);
        assert_ne!(in_a, in_a_later);
    }

    /// A member derives exactly the published grouping key from a leaf's own `seq`.
    #[test]
    fn a_member_rederives_the_published_pseudonym_from_seq() {
        let l = leaf_at("conv-a", 0, 4, W + 7);
        assert_eq!(
            l.conversation_pseudonym,
            derive_conversation_pseudonym("conv-a", window_for_seq(l.seq))
        );
    }
}
