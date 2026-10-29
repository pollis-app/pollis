//! The MLS control plane's READ half (#987): one signed POST that answers every
//! question the client used to ask its own Turso connection.
//!
//! # Why one endpoint, and why POST
//!
//! **One snapshot.** GroupInfo and the pending-Welcome flag share one transaction.
//!
//! **One batch.** `head`, `head_generation` and `commits` share a transaction and are contiguity-checked; [`pruned_below`] marks a real prune.
//!
//! **POST, not GET.** The signature does not cover the query string (#681), so parameters live in the signed body.
//!
//! [`pruned_below`]: ConversationState::pruned_below

use serde::{Deserialize, Serialize};

use crate::commit::CommitWire;

/// Everything the MLS replay/join path needs, for one or more conversations, in
/// one signed round trip.
///
/// Batched so a cold-launch sweep is one round trip, not one per conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationStateBody {
    pub queries: Vec<ConversationStateQuery>,
    /// No-auth (dev/test) path only; ignored when auth is enforced.
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
    /// Include the GroupInfo *blob*, not just its head (only `external_join_attempt` needs it).
    #[serde(default)]
    pub want_group_info_blob: bool,
    /// Fetch the commit bytes at this epoch: the input to the client's `invariants::resolve`.
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
/// Log-DB fields share one transaction; the main-DB gates are `Option<bool>` so each keeps its own failure bias.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationState {
    pub conversation_id: String,
    /// `false` → not a member; every field below is empty (per entry, so one stale id cannot blind a sweep).
    pub authorized: bool,
    #[serde(default)]
    pub group_info: Option<GroupInfoState>,
    /// An undelivered `mls_welcome` for this user+device, read in the SAME transaction as `group_info`.
    pub welcome_pending: bool,
    /// Head epoch WITHIN `generation` (`MAX(epoch) + 1`, 0 for an empty lineage).
    pub head: i64,
    /// The conversation's newest lineage.
    pub head_generation: i64,
    /// The lineage `head`, `commits` and `commit_at_epoch` are scoped to — the
    /// requested one, or the head generation when the query left it `None`.
    pub generation: i64,
    /// `epoch >= since` in `generation`, ascending, from the same transaction as the heads.
    #[serde(default)]
    pub commits: Vec<CommitWire>,
    /// A genuine discontinuity INSIDE `commits`: an epoch missing between two
    /// epochs that are present. `None` is the normal case.
    ///
    /// Computed in the snapshot transaction by [`check_contiguous`], so it is a fact about the log.
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
    /// When the authenticated device was registered (`user_device.created_at`),
    /// verbatim (#1571). `None` = not reported (a server that predates it, or a
    /// failed read).
    ///
    /// With [`Self::member_since`]: a device registered before its user joined is entitled to every message since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_registered_at: Option<String>,
    /// When the authenticated USER became a member of this conversation, verbatim:
    /// `dm_channel_member.added_at` for a DM, `group_member.joined_at` for a
    /// group or one of its channels (#1571). `None` = not reported. Never earlier
    /// than the conversation itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_since: Option<String>,
    /// Cross-signing inputs for every distinct `added_user_id` named by
    /// `commits`, so replay verifies added devices without a round trip per
    /// add-carrying commit. Empty when the batch adds nobody.
    #[serde(default)]
    pub added_identities: Vec<AddedIdentity>,
}

/// A user's cross-signing root plus the device rows a commit's `added_device_ids`
/// name — the INPUTS to `IdentityDirectory::leaf_verdict`, never its verdict.
///
/// Folded into the batch (#987 §5); verification itself stays client-side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddedIdentity {
    pub user_id: String,
    /// base64; `None` (absent or NULL) means "retry", never "verified".
    #[serde(default)]
    pub account_id_pub: Option<String>,
    /// One entry per device row that EXISTS; a missing row is the client's `AbsentRetry` case.
    pub devices: Vec<DeviceCertRow>,
}

/// One `user_device` row's cross-signing columns, blobs base64.
///
/// All `Option`: a NULL cert column means "retry", a set `revoked_at` means "revoked".
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

/// Drain this device's undelivered Welcomes (the read half of `POST /v1/welcomes/ack`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchWelcomesBody {
    /// No-auth path only; when auth is on the recipient is the signed user and
    /// a mismatch is a 403 (`resolve_recipient`).
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    /// Also report how many unclaimed KeyPackages the SIGNING device still has
    /// in this suite (#1412), on [`FetchWelcomesResponse::unclaimed_key_packages`].
    ///
    /// Rides on the drain so a low pool is refilled before a peer's claim finds it empty.
    ///
    /// Additive: an older DS answers without a count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_key_packages_in_suite: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchWelcomesResponse {
    pub welcomes: Vec<PendingWelcome>,
    /// Unclaimed KeyPackages in the requested suite; `None` if not asked, unreadable, or an older server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unclaimed_key_packages: Option<i64>,
    /// Whether a last-resort KeyPackage is held (#1571); `Some(false)` cues publishing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_last_resort_key_package: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingWelcome {
    pub id: String,
    /// TLS-serialized MLS Welcome, base64.
    pub welcome: String,
    /// The conversation the Welcome row was FILED UNDER — `mls_welcome.
    /// conversation_id`, i.e. the conversation whose commit bundle wrote it.
    ///
    /// The client refuses the join if this disagrees with the Welcome's `GroupId` (the half of C1 the DS cannot check).
    ///
    /// Additive: `None` means the server does not say (the legacy path).
    #[serde(default)]
    pub conversation_id: Option<String>,
}

/// A discontinuity in a served commit batch: the log is missing an epoch that
/// sits between two it does have.
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
/// Pure and total.
///
/// # Report, do not refuse
///
/// A hole is REPORTED, not refused: dropping the group and external-joining is the recovery.
///
/// Same-transaction reads (#987) mean a reported hole is real, not phantom.
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
