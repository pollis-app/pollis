//! The MLS control plane's READ half (#987): one signed POST that answers every
//! question the client used to ask its own Turso connection.
//!
//! # Why one endpoint, and why POST
//!
//! Two constraints shape this module, and both are correctness constraints
//! rather than style:
//!
//! **One snapshot.** `external_join_attempt` reads the published GroupInfo and
//! then, eight lines later, checks whether a Welcome for this device is pending.
//! The correctness argument for that pair is written into `pollis_delivery::
//! commit::submit_commit`: the commit, its GroupInfo and its Welcomes land in ONE
//! `IMMEDIATE` transaction, so a reader that observes the GroupInfo necessarily
//! observes the Welcome. That holds across two reads of one libsql connection.
//! It does NOT hold across two HTTP requests, which may reach different replicas
//! or different moments — and a device that external-joins while its own Welcome
//! is inbound clobbers the freshly-joined group and is stranded permanently on
//! `no GroupInfo stored — cannot external-join`. The Kani property
//! `ExternalJoin ⟹ !welcome_pending` is proved over a *pure* function, so feeding
//! it inputs from two snapshots does not fail the proof — it makes it vacuous.
//! Hence [`ConversationState`]: GroupInfo, the pending-Welcome flag, the head and
//! the commit batch all come from ONE read transaction, server-side.
//!
//! **One batch.** A non-contiguous commit batch is read by the client as a
//! permanent gap, and its response is destructive — `forget_local_mls_group_at`
//! drops the device's MLS crypto state. So `head`, `head_generation` and
//! `commits` come from the same transaction as each other (which today's
//! `GET /v1/commits/:id` handler does NOT do — three separate queries on a bare
//! connection), and the handler additionally *verifies* contiguity before
//! answering, refusing rather than serving a torn batch. [`pruned_below`] is what
//! keeps a genuine retention prune (#539) distinguishable from a torn read.
//!
//! **POST, not GET.** The canonical signing message covers the request path
//! *without its query string*, so a signed `GET …?since=N` carries
//! unauthenticated parameters — precisely the shape of #681, where an
//! unauthenticated `GET /v1/commits?user_id&device_id` was a remote commit-log
//! wipe. Every read here is a POST whose parameters live in the signed body.
//!
//! [`pruned_below`]: ConversationState::pruned_below

use serde::{Deserialize, Serialize};

use crate::commit::CommitWire;

// ── POST /v1/mls/conversation-state ──────────────────────────────────────────

/// Everything the MLS replay/join path needs, for one or more conversations, in
/// one signed round trip.
///
/// Batched (`queries`) because the cold-launch sweep runs the catch-up chain per
/// conversation, serially: a 20-group launch would otherwise be 20+ sequential
/// signed round trips, which moves the latency problem rather than fixing it
/// (#987 §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationStateBody {
    pub queries: Vec<ConversationStateQuery>,
    /// No-auth (dev/test) path only. When the DS enforces auth both halves come
    /// from the verified signature and these are ignored — though still
    /// signature-bound, since the signature covers the whole body.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// One conversation's worth of question. Every field beyond `conversation_id` is
/// opt-in, so a caller that only wants the GroupInfo head does not pay for a
/// commit scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationStateQuery {
    pub conversation_id: String,
    /// The lineage to answer `head`/`commits`/`commit_at_epoch` for. `None` →
    /// the conversation's head generation.
    #[serde(default)]
    pub generation: Option<i64>,
    /// Fetch commits with `epoch >= since` in `generation`. `None` → no commit
    /// scan at all (the caller only wants heads/flags).
    #[serde(default)]
    pub since: Option<i64>,
    /// Include the GroupInfo *blob*, not just its `(generation, epoch)` head.
    /// Only `external_join_attempt` needs the bytes; the durability backstop
    /// (`ensure_group_info_published`) needs the head alone, and the blob carries
    /// the whole ratchet tree.
    #[serde(default)]
    pub want_group_info_blob: bool,
    /// Fetch the canonical commit bytes stored at this epoch in `generation` —
    /// the input to `reconcile`'s ambiguous-failure resolution. The DECISION
    /// stays client-side (`invariants::resolve`, Kani-proved); this is the input
    /// it consumes.
    #[serde(default)]
    pub commit_at_epoch: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationStateResponse {
    /// One entry per `queries` element, in request order.
    pub states: Vec<ConversationState>,
}

/// The published GroupInfo's head, and optionally its bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupInfoState {
    pub generation: i64,
    pub epoch: i64,
    /// base64, present iff the query set `want_group_info_blob`.
    #[serde(default)]
    pub group_info: Option<String>,
}

/// One conversation's snapshot.
///
/// Every log-DB field below is read inside ONE transaction, so the pairs the
/// client evaluates together — `group_info` with `welcome_pending`,
/// `head`/`head_generation` with `commits` — cannot be torn. The two main-DB
/// gates are `Option<bool>` rather than `bool` precisely so they keep their
/// OPPOSITE failure biases: `None` means "the server could not determine this",
/// and the client applies each gate's own bias at its own call site
/// (`local_user_is_member` fails CLOSED, `has_pending_welcome` fails OPEN — they
/// are seven lines apart and must not collapse into one error path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationState {
    pub conversation_id: String,
    /// `false` → the authenticated user is not a member of this conversation and
    /// every field below is empty. A per-entry flag rather than a 403 for the
    /// whole batch, so one stale conversation id in a sweep cannot blind the
    /// client to the other nineteen.
    pub authorized: bool,
    #[serde(default)]
    pub group_info: Option<GroupInfoState>,
    /// An undelivered `mls_welcome` targeting this user+device exists for this
    /// conversation. Read in the SAME transaction as `group_info` — that pairing
    /// is the whole reason this endpoint exists.
    pub welcome_pending: bool,
    /// Head epoch WITHIN `generation` (`MAX(epoch) + 1`, 0 for an empty lineage).
    pub head: i64,
    /// The conversation's newest lineage.
    pub head_generation: i64,
    /// The lineage `head`, `commits` and `commit_at_epoch` are scoped to — the
    /// requested one, or the head generation when the query left it `None`.
    pub generation: i64,
    /// `epoch >= since` in `generation`, ascending. Read in the SAME transaction
    /// as `head`, `head_generation` and `pruned_below`, so what the client sees
    /// here cannot disagree with them.
    #[serde(default)]
    pub commits: Vec<CommitWire>,
    /// A genuine discontinuity INSIDE `commits`: an epoch missing between two
    /// epochs that are present. `None` is the normal case.
    ///
    /// Computed by [`check_contiguous`] in the snapshot transaction, so it is a
    /// fact about the log rather than an artifact of reading `commits` and
    /// `head` at two different instants — which is what could previously
    /// manufacture a phantom gap and send a client into a destructive recovery
    /// for nothing. The client's response to a REAL hole (drop the local group,
    /// external-join onto the head) is correct and is what
    /// `epoch_gap_recovers_via_external_join` pins; see [`check_contiguous`] for
    /// why this is reported rather than refused.
    #[serde(default)]
    pub hole: Option<CommitHole>,
    /// The lowest epoch still retained in `generation`, or `None` for an empty
    /// lineage. `pruned_below > since` is how a client tells a genuine retention
    /// prune (#539) — where dropping the local group and re-joining is the
    /// CORRECT response — from a torn read, where it would be data loss.
    #[serde(default)]
    pub pruned_below: Option<i64>,
    /// base64 commit bytes at `commit_at_epoch`, when the query asked and a row
    /// exists.
    #[serde(default)]
    pub commit_at_epoch: Option<String>,
    /// Is the authenticated user a CURRENT member? `None` = undetermined (the
    /// server's main-DB read failed). Client fails CLOSED on both `None` and a
    /// transport error — a membership leak is worse than a delayed recovery.
    #[serde(default)]
    pub is_member: Option<bool>,
    /// Is the authenticated device still registered (not revoked)? `None` =
    /// undetermined. Client fails CLOSED.
    #[serde(default)]
    pub device_registered: Option<bool>,
    /// Cross-signing inputs for every distinct `added_user_id` named by
    /// `commits`, so replay verifies added devices without a round trip per
    /// add-carrying commit. Empty when the batch adds nobody.
    #[serde(default)]
    pub added_identities: Vec<AddedIdentity>,
}

/// A user's cross-signing root plus the device rows a commit's `added_device_ids`
/// name — the INPUTS to `verify_added_devices`, never its verdict.
///
/// Folded into the commit batch rather than fetched per commit (#987 §5): cert
/// verification runs once per add-carrying commit during replay, so a cold
/// catch-up over K adds was K sequential round trips interleaved with MLS work.
/// The verification LOOP is unchanged and still lives on the client, because it
/// is cryptography over a signature chain the DS is not trusted to evaluate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddedIdentity {
    pub user_id: String,
    /// base64. `None` when the `users` row is absent or its `account_id_pub` is
    /// NULL — which the client reads as "not replicated yet, retry", never as
    /// "verified". Absence must stay distinguishable from a present-but-empty
    /// key, hence `Option` rather than an empty string.
    #[serde(default)]
    pub account_id_pub: Option<String>,
    /// One entry per device row that EXISTS. A requested device with no row is
    /// simply missing here, which is the client's `AbsentRetry` case — the DS
    /// does not manufacture a placeholder, because "absent" and "present with
    /// null columns" are different states with different (both non-fatal, both
    /// distinct) client handling.
    pub devices: Vec<DeviceCertRow>,
}

/// One `user_device` row's cross-signing columns, blobs base64.
///
/// Every column is `Option` because every one of them is genuinely nullable
/// mid-enrollment, and the client distinguishes the cases: a NULL cert column on
/// a live row is "cert publish has not landed yet" (retry), while a non-NULL
/// `revoked_at` is "revoked" (a verdict). Flattening them into non-optional
/// fields would erase exactly the distinction the replay depends on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCertRow {
    pub device_id: String,
    /// base64
    #[serde(default)]
    pub device_cert: Option<String>,
    #[serde(default)]
    pub cert_issued_at: Option<String>,
    #[serde(default)]
    pub cert_identity_version: Option<i64>,
    /// base64
    #[serde(default)]
    pub mls_signature_pub: Option<String>,
    #[serde(default)]
    pub revoked_at: Option<String>,
    /// base64
    #[serde(default)]
    pub mls_signature_pub_pq: Option<String>,
}

// ── POST /v1/welcomes/fetch ──────────────────────────────────────────────────

/// Drain this device's undelivered Welcomes. The write half
/// (`POST /v1/welcomes/ack`) already exists; this is its read counterpart, and
/// the last direct log-DB read on the welcome path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchWelcomesBody {
    /// No-auth path only; when auth is on the recipient is the signed user and
    /// a mismatch is a 403 (`resolve_recipient`).
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchWelcomesResponse {
    pub welcomes: Vec<PendingWelcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingWelcome {
    pub id: String,
    /// TLS-serialized MLS Welcome, base64.
    pub welcome: String,
}

// ── Contiguity, as a pure function ───────────────────────────────────────────

/// A discontinuity in a served commit batch: the log is missing an epoch that
/// sits between two it does have.
///
/// Reported on [`ConversationState::hole`], computed by [`check_contiguous`]
/// INSIDE the snapshot transaction, so both ends compile the same rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommitHole {
    /// The epoch that should have come next.
    pub expected: i64,
    /// The epoch actually found there.
    pub found: i64,
}

/// A commit batch is contiguous iff its epochs are consecutive from first to
/// last.
///
/// Pure, so the invariant is testable without a database, and total, so "we did
/// not check" is not representable.
///
/// # Report, do not refuse
///
/// A hole is REPORTED to the client, not turned into an error. The distinction
/// matters and is easy to get backwards, because the client's response to a hole
/// — `forget_local_mls_group_at`, which deletes its MLS crypto state for that
/// group — reads like something to be prevented. It is not: it is the RECOVERY.
/// The device cannot advance a local group across an epoch that does not exist,
/// so it drops it and external-joins onto the head, which is exactly what
/// `epoch_gap_recovers_via_external_join` and `corrupt_commit_recovers_instead_of_wedging`
/// pin. Refusing to serve the batch would make a genuinely damaged conversation
/// permanently unreadable instead — every read erroring, forever, with no path
/// back. "Messages must work" points the other way.
///
/// What #987 actually fixed is the PHANTOM hole. The reads used to come off
/// separate statements, so a batch fetched at one instant could disagree with a
/// `head` fetched at another and manufacture a gap that was never in the log —
/// and the client would destroy real state recovering from nothing. Computing
/// this inside the same transaction as `head`, `head_generation` and
/// `pruned_below` means a reported hole is a fact about the log.
///
/// Whether the batch *starts* where the caller asked is a different question,
/// answered by [`ConversationState::pruned_below`]: a prune moves the floor and
/// is legitimate; a hole is damage.
pub fn check_contiguous(commits: &[CommitWire]) -> Result<(), CommitHole> {
    for pair in commits.windows(2) {
        let expected = pair[0].epoch + 1;
        if pair[1].epoch != expected {
            return Err(CommitHole {
                expected,
                found: pair[1].epoch,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(epoch: i64) -> CommitWire {
        CommitWire {
            generation: 0,
            epoch,
            seq: epoch,
            sender_id: "u".into(),
            commit: String::new(),
            added_user_id: None,
            added_device_ids: None,
            created_at: String::new(),
        }
    }

    #[test]
    fn empty_and_single_batches_are_contiguous() {
        assert_eq!(check_contiguous(&[]), Ok(()));
        assert_eq!(check_contiguous(&[wire(7)]), Ok(()));
    }

    #[test]
    fn consecutive_epochs_are_contiguous_wherever_they_start() {
        let batch: Vec<_> = (4..=9).map(wire).collect();
        assert_eq!(check_contiguous(&batch), Ok(()));
    }

    /// One missing epoch in the middle of a batch — reported with BOTH epochs so
    /// an operator reading a log line can find the gap without a query.
    #[test]
    fn a_hole_is_reported_with_both_epochs() {
        let batch = vec![wire(3), wire(4), wire(6)];
        assert_eq!(
            check_contiguous(&batch),
            Err(CommitHole {
                expected: 5,
                found: 6
            })
        );
    }

    /// Duplicate or out-of-order epochs are holes too — `epoch + 1` is the only
    /// accepted step, so neither a repeat nor a regression sneaks past.
    #[test]
    fn repeats_and_regressions_are_holes() {
        assert!(check_contiguous(&[wire(3), wire(3)]).is_err());
        assert!(check_contiguous(&[wire(3), wire(2)]).is_err());
    }
}
