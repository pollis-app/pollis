//! The **mls-commit-log** tenant: the canonical leaf encoding for an MLS
//! commit and the [`CommitLogInvariant`] that makes #357's per-conversation
//! rules globally auditable.
//!
//! This module is pure — no IO, no DB, no clock. The DB reading lives in
//! [`crate::source`]; everything here operates on already-read values so the
//! encoding and invariant can be unit-tested and reused (e.g. by a monitor)
//! without a database.
//!
//! ## Windowed pseudonyms (#701)
//!
//! The published leaf carries **windowed pseudonyms**, not the raw
//! `conversation_id` / `sender_id`, so a third party who only sees the public
//! log cannot build a longitudinal activity map (which groups exist, who commits
//! to them, how often, across the whole product). See
//! [`derive_conversation_pseudonym`] / [`derive_sender_pseudonym`] and the
//! design section in `docs/transparency.md`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use verifiable_log::{Entry, InvariantViolation, TenantInvariant};

/// Tenant id for the MLS commit log in the shared verifiable log.
pub const TENANT: &str = "mls-commit-log";

/// Domain-separation tag for the conversation pseudonym. Bumping the trailing
/// version invalidates every derived pseudonym, so it changes only at a full
/// republish (a frozen-contract event — see [`CommitLeaf`]).
const CONVERSATION_PSEUDONYM_DOMAIN: &[u8] =
    b"pollis-verifiable-log:commit-pseudonym:conversation:v1";

/// Domain-separation tag for the sender pseudonym. The sender pseudonym is bound
/// to the conversation as well as the window, so the same user is not linkable
/// across conversations — the worst part of the pre-#701 exposure.
const SENDER_PSEUDONYM_DOMAIN: &[u8] = b"pollis-verifiable-log:commit-pseudonym:sender:v1";

/// How many global `seq` values fall in one pseudonym window.
///
/// A conversation's pseudonym is stable **within** a window and rotates at each
/// boundary, so linkage is broken across windows for anyone who does not know the
/// real `conversation_id`. The size trades two properties against each other:
///
/// * **larger** → more of a conversation's commits share a window, so the public
///   (no-side-knowledge) fork/monotonicity check has more to bite on, but a
///   conversation stays linkable across a longer stretch of the log;
/// * **smaller** → stronger cross-window unlinkability, but the public invariant
///   degrades toward per-entry singletons and there are more window boundaries
///   (the residual — see [`CommitLogInvariant`]).
///
/// The commit log grows by one leaf per MLS *commit* (a membership/key change),
/// not per message, so it is low-volume; 1024 keeps a busy conversation's activity
/// groupable within a window while fragmenting a long-lived group across many
/// windows. It is the one tunable of the scheme and, because it is baked into the
/// frozen leaf, only ever changes at a full republish.
pub const PSEUDONYM_WINDOW_SIZE: u64 = 1024;

/// The window index a given `seq` falls in. Derivable from the published leaf's
/// own `seq`, so a member (who knows `conversation_id`) can recompute a leaf's
/// pseudonym with no extra published field, and an outsider never needs it (they
/// group by the pseudonym itself).
pub fn window_for_seq(seq: i64) -> u64 {
    (seq.max(0) as u64) / PSEUDONYM_WINDOW_SIZE
}

/// Keyless, length-framed `SHA-256` over a domain tag and a list of fields,
/// lowercase hex. Length-prefixing every part (the domain included) makes the
/// pre-image unambiguous, so no two distinct field lists can collide by
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

/// The windowed conversation pseudonym: `H(domain || conversation_id || window)`.
///
/// Keyless — the only "secret" is `conversation_id` itself, which members and the
/// server already know but a third-party log scraper does not. No new key is
/// introduced, so there is no new custody problem (contrast
/// `docs/sth-signing-key-custody.md`). Anyone who knows the real
/// `conversation_id` can derive every window's pseudonym and verify their own
/// full history; anyone who does not cannot link one window to the next.
pub fn derive_conversation_pseudonym(conversation_id: &str, window: u64) -> String {
    hash_framed(
        CONVERSATION_PSEUDONYM_DOMAIN,
        &[conversation_id.as_bytes(), &window.to_le_bytes()],
    )
}

/// The windowed, conversation-bound sender pseudonym:
/// `H(domain || conversation_id || sender_id || window)`.
///
/// Binding to `conversation_id` means the same user gets an unrelated pseudonym in
/// every conversation, so an outsider cannot follow a person across the groups
/// they participate in — and, being windowed too, cannot follow them across time
/// within one conversation either.
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
/// identity. It deliberately stores `sha256(commit_data)` (hex), **never** the
/// raw TLS-serialised commit bytes: the leaf commits to the bytes without
/// disclosing or persisting them.
///
/// The on-the-wire leaf encoding is **compact JSON of this struct with fields
/// in exactly this declared order** (`conversation_pseudonym`, `generation`,
/// `epoch`, `sender_pseudonym`, `seq`, `commit_sha256`). serde emits struct
/// fields in declaration order with no insignificant whitespace, so the encoding
/// is deterministic and stable. This is a frozen contract extension of
/// `verifiable-log`'s leaf encoding — see the builder README.
///
/// ### The identity fields are windowed pseudonyms (#701)
///
/// Before #701 this leaf published the raw `conversation_id` and real `sender_id`
/// in the clear, so anyone could build a longitudinal activity map of every group.
/// It now publishes [`derive_conversation_pseudonym`] and
/// [`derive_sender_pseudonym`] instead. Because leaf bytes are hashed into the
/// Merkle tree, **changing the leaf shape is not an edit — it invalidates every
/// signed root ever published and every cached inclusion proof.** So this change
/// takes effect only at a full **republish of the tree from scratch**, which is
/// exactly what the #672 / PL-11 ML-DSA-44 key rotation (executed by #699) already
/// does under the `sth:v2` contexts. #701's code changes *what the republished
/// tree contains*; it must land with that ceremony, not before.
///
/// `generation` (#454 P4) is omitted entirely when it is 0, so the field-omission
/// precedent this struct already set is preserved. (There is no byte-for-byte
/// continuity to preserve with the pre-#701 tree — the whole tree is rebuilt — but
/// the omission discipline is kept for tidiness and cross-generation stability.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitLeaf {
    /// Windowed pseudonym for the MLS conversation (group) —
    /// [`derive_conversation_pseudonym`]. Stable within a window, rotating at each
    /// [`PSEUDONYM_WINDOW_SIZE`] boundary.
    pub conversation_pseudonym: String,
    /// Suite generation of the MLS lineage this commit belongs to (#454 P4).
    /// A conversation that has never migrated is generation 0; each hybrid-suite
    /// migration stands up a successor lineage at generation N+1, restarting at
    /// MLS epoch 0.
    #[serde(default, skip_serializing_if = "is_generation_zero")]
    pub generation: u64,
    /// MLS epoch *after* this commit, within [`Self::generation`]'s lineage.
    pub epoch: u64,
    /// Windowed, conversation-bound pseudonym for the committer —
    /// [`derive_sender_pseudonym`]. Recorded so a later slice can add cryptographic
    /// authorization; this slice does NOT validate it (that needs MLS group state).
    pub sender_pseudonym: String,
    /// `mls_commit_log.seq` — the log's global insertion order. Also the window
    /// driver: `window = seq / PSEUDONYM_WINDOW_SIZE`.
    pub seq: i64,
    /// `sha256(commit_data)`, lowercase hex (32 bytes). The raw blob is never
    /// stored in the leaf.
    pub commit_sha256: String,
}

/// Is this the classic suite generation? Used to keep generation-0 leaves from
/// carrying a `generation` field — see [`CommitLeaf`].
fn is_generation_zero(generation: &u64) -> bool {
    *generation == 0
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
/// append and enforces, per **grouping key** (the leaf's `conversation_pseudonym`):
///
/// * **(a) no fork** — no two entries share the same
///   `(conversation_pseudonym, generation, epoch)`;
/// * **(b) no regression/replay** — within a grouping key, `(generation, epoch)`
///   is strictly increasing **lexicographically** in `seq` order;
/// * **(c) a lineage opens at epoch 0** — the first commit of a generation
///   higher than any seen before must be at epoch 0.
///
/// ### Windowing and what the grouping key means (#701)
///
/// The grouping key is the windowed [`derive_conversation_pseudonym`]. Two entries
/// of the same conversation in the same window share a pseudonym, so grouping by
/// it is exactly "same conversation, same window". This is what makes the
/// invariant **publicly checkable with no side knowledge**: an outsider replays
/// the pseudonymous log, groups by `conversation_pseudonym`, and catches any fork
/// or regression that happens *within a window* — including a fork the operator
/// tried to plant (DoD #1).
///
/// **The residual, named honestly.** Because a conversation's pseudonym rotates at
/// each window boundary, an outsider cannot group its window-`w` entries with its
/// window-`w+1` entries. So a fork or regression whose two branches straddle a
/// boundary is **not** publicly detectable by grouping alone. This is not a bug to
/// be patched away: a *public* commitment that let an outsider re-link the two
/// windows would, by construction, also let them defeat the unlinkability the
/// scheme exists to provide — the two goals are mutually exclusive for a
/// third-party observer. A **member** (who knows the real `conversation_id`) is
/// unaffected: they derive every window's pseudonym, regroup the whole
/// conversation across windows, and check the full-strength invariant end to end
/// (see [`crate::builder::build_bundle`]'s source-integrity gate and
/// `verifiable_log_serve::group::verify_group_in_bundle`). The builder, which
/// holds the real ids, therefore never *emits* a boundary-straddling fork; only a
/// substituted/compromised publisher could, and members catch it.
///
/// Because the builder appends commits in `seq` order, the candidate always has
/// the largest `seq` seen so far for its grouping key, so "strictly greater than
/// every prior `(generation, epoch)`" is exactly rule (b).
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
/// its natural start.
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
        // The highest generation this grouping key has ever reached. Tracked
        // across the whole scan rather than checked per-pair, because rule (c)
        // is about the candidate versus *every* prior entry at once: opening a
        // lineage is only legal when nothing has opened it before.
        let mut max_generation: Option<u64> = None;
        for prev_entry in existing {
            let prev = Self::parse(prev_entry)?;
            if prev.conversation_pseudonym != cand.conversation_pseudonym {
                continue;
            }
            let prev_key = (prev.generation, prev.epoch);
            max_generation = Some(max_generation.map_or(prev.generation, |m| m.max(prev.generation)));
            // (a) fork: same grouping key + same lineage + same epoch, two
            // distinct commits.
            if prev_key == cand_key {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "fork in conversation `{}` at generation {} epoch {}: seq {} conflicts with seq {}",
                        cand.conversation_pseudonym, cand.generation, cand.epoch, cand.seq, prev.seq
                    ),
                ));
            }
            // (b) regression/replay: a prior commit for this grouping key
            // already reached a higher `(generation, epoch)`, so this one goes
            // backwards — either within a lineage, or back onto one the
            // conversation has already retired.
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
        // (c) a lineage opens at epoch 0. Only checked when the candidate is
        // genuinely opening one — i.e. its generation is above everything this
        // grouping key has recorded. The very first commit of a grouping key
        // (no prior entries at all) is exempt: the builder is appending an
        // already-existing history in `seq` order and may legitimately start
        // mid-lineage after a log prune — or, under windowing, because an
        // earlier window carried this conversation's earlier epochs.
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

    /// A window small enough that these unit tests can straddle a boundary with
    /// small seqs. The real driver is [`PSEUDONYM_WINDOW_SIZE`]; here we just pick
    /// seqs on either side of a real boundary.
    const W: i64 = PSEUDONYM_WINDOW_SIZE as i64;

    /// A published (pseudonymous) leaf for `conv` at `seq`, as the builder would
    /// emit it: the grouping key is the windowed conversation pseudonym.
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
        // Field order is frozen: conversation_pseudonym, generation, epoch,
        // sender_pseudonym, seq, commit_sha256.
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(s.starts_with("{\"conversation_pseudonym\":"));
        assert!(s.find("\"epoch\":").unwrap() < s.find("\"sender_pseudonym\":").unwrap());
        assert!(s.find("\"sender_pseudonym\":").unwrap() < s.find("\"seq\":").unwrap());
        assert!(s.find("\"seq\":").unwrap() < s.find("\"commit_sha256\":").unwrap());
        assert_eq!(CommitLeaf::decode(&bytes).unwrap(), l);
    }

    /// The generation-0 omission precedent is preserved through the rename: a
    /// generation-0 leaf carries no `generation` field.
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
        // Different conversation at epoch 0 is fine.
        assert!(inv.check(&[&e0, &e1], &eb).is_ok());
    }

    /// DoD #1: a fork planted *within a window* is still rejected by a public
    /// replay that has no side knowledge — it groups purely by the published
    /// pseudonym.
    #[test]
    fn rejects_fork_within_a_window() {
        let inv = CommitLogInvariant;
        // seqs 1 and 2 → same window → same conversation pseudonym.
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

    /// The named residual: a fork whose two branches straddle a window boundary is
    /// NOT caught by a public replay grouping on the published pseudonym, because
    /// the two branches carry different pseudonyms. (A member who knows the real
    /// `conversation_id` still catches it — see the serve-layer group tests.)
    #[test]
    fn a_fork_across_a_window_boundary_escapes_public_grouping() {
        let inv = CommitLogInvariant;
        let e0 = leaf("a", 0, 1).to_entry().unwrap();
        // Same conversation, same (generation, epoch) — a real fork — but placed
        // in the next window.
        let fork_next_window = leaf("a", 0, W + 1).to_entry().unwrap();
        assert_ne!(
            CommitLeaf::decode(&e0.data).unwrap().conversation_pseudonym,
            CommitLeaf::decode(&fork_next_window.data).unwrap().conversation_pseudonym,
            "the boundary must rotate the pseudonym"
        );
        // Public replay cannot see the two as the same conversation, so it does
        // not fire. This documents the residual honestly rather than hiding it.
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

    // -- Pseudonym properties (DoD #3: the negative, proven from the leaves alone).

    /// The same conversation gets an unrelated pseudonym in a different window, so
    /// an entries dump alone cannot link a conversation across windows.
    #[test]
    fn a_conversation_is_unlinkable_across_windows() {
        let w0 = derive_conversation_pseudonym("conv-a", 0);
        let w1 = derive_conversation_pseudonym("conv-a", 1);
        assert_ne!(w0, w1, "the pseudonym must rotate across windows");
        // …and it is stable *within* a window (what keeps the public invariant
        // checkable).
        assert_eq!(w0, derive_conversation_pseudonym("conv-a", 0));
    }

    /// The same user gets an unrelated pseudonym in a different conversation, so an
    /// entries dump alone cannot follow a person across the groups they are in —
    /// the worst part of the pre-#701 exposure.
    #[test]
    fn a_sender_is_unlinkable_across_conversations() {
        let in_a = derive_sender_pseudonym("conv-a", "u-alice", 0);
        let in_b = derive_sender_pseudonym("conv-b", "u-alice", 0);
        assert_ne!(in_a, in_b, "the same user must not be linkable across conversations");
        // And unlinkable across windows within one conversation.
        let in_a_later = derive_sender_pseudonym("conv-a", "u-alice", 1);
        assert_ne!(in_a, in_a_later);
    }

    /// A member who knows `conversation_id` derives exactly the published grouping
    /// key from a leaf's own `seq` — the mechanism that lets `verify_group` work
    /// across windows with no extra published field.
    #[test]
    fn a_member_rederives_the_published_pseudonym_from_seq() {
        let l = leaf_at("conv-a", 0, 4, W + 7);
        assert_eq!(
            l.conversation_pseudonym,
            derive_conversation_pseudonym("conv-a", window_for_seq(l.seq))
        );
    }
}
