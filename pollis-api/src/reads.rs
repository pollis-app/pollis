//! The MLS control plane's read half (#987), designed around three rules (see
//! `.codesight/wiki/commands.md`):
//!
//! - **One snapshot.** GroupInfo, the pending-Welcome flag, the heads and the
//!   commit batch come from one server-side read transaction
//!   ([`ConversationState`]). Across two requests a device can see a GroupInfo
//!   without its atomically written Welcome, external-join into its own inbound
//!   Welcome and be stranded.
//! - **One batch.** `head`, `head_generation` and `commits` share that
//!   transaction, so a reported [`hole`] is a fact about the log, and
//!   [`pruned_below`] separates a retention prune (#539) from damage.
//! - **POST, not GET**: the signature covers the path without its query string
//!   (#681).
//!
//! [`hole`]: ConversationState::hole
//! [`pruned_below`]: ConversationState::pruned_below

use serde::{Deserialize, Serialize};

use crate::commit::CommitWire;

/// Everything the MLS replay/join path needs, for one or more conversations, in
/// one signed round trip; batched so a cold-launch sweep is not one trip per
/// conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationStateBody {
    pub queries: Vec<ConversationStateQuery>,
    /// No-auth (dev/test) path only; ignored when the DS enforces auth.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// One conversation's question; every field beyond `conversation_id` is opt-in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationStateQuery {
    pub conversation_id: String,
    /// The lineage for `head` / `commits` / `commit_at_epoch`; `None` → the head
    /// generation.
    #[serde(default)]
    pub generation: Option<i64>,
    /// Fetch commits with `epoch >= since` in `generation`; `None` → no scan.
    #[serde(default)]
    pub since: Option<i64>,
    /// Include the GroupInfo blob (the whole ratchet tree), not just its head.
    /// Only `external_join_attempt` needs it.
    #[serde(default)]
    pub want_group_info_blob: bool,
    /// Fetch the commit bytes stored at this epoch: the input to `reconcile`'s
    /// ambiguous-failure resolution, which stays client-side.
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
/// One conversation's snapshot, every log-DB field read in one transaction.
///
/// The main-DB gates are `Option<bool>` so `None` ("couldn't determine") keeps
/// each call site's own bias: `local_user_is_member` fails closed,
/// `has_pending_welcome` fails open. They must not collapse into one error path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationState {
    pub conversation_id: String,
    /// `false` → not a member; every field below is empty. Per entry, so one
    /// stale id in a sweep cannot blind the rest of the batch.
    pub authorized: bool,
    #[serde(default)]
    pub group_info: Option<GroupInfoState>,
    /// An undelivered `mls_welcome` for this user+device exists, read in the same
    /// transaction as `group_info`.
    pub welcome_pending: bool,
    /// Head epoch WITHIN `generation` (`MAX(epoch) + 1`, 0 for an empty lineage).
    pub head: i64,
    /// The conversation's newest lineage.
    pub head_generation: i64,
    /// The lineage `head`, `commits` and `commit_at_epoch` are scoped to.
    pub generation: i64,
    /// `epoch >= since` in `generation`, ascending, from the same transaction as
    /// `head`, `head_generation` and `pruned_below`.
    #[serde(default)]
    pub commits: Vec<CommitWire>,
    /// A discontinuity inside `commits`; `None` normally. Computed by
    /// [`check_contiguous`] in the snapshot transaction.
    #[serde(default)]
    pub hole: Option<CommitHole>,
    /// The lowest epoch retained in `generation`. `pruned_below > since` marks a
    /// retention prune (#539), where re-joining is correct, not a torn read.
    #[serde(default)]
    pub pruned_below: Option<i64>,
    /// base64 commit bytes at `commit_at_epoch`, when asked and present.
    #[serde(default)]
    pub commit_at_epoch: Option<String>,
    /// Is the authenticated user a current member? `None` = undetermined; the
    /// client fails closed.
    #[serde(default)]
    pub is_member: Option<bool>,
    /// Is the authenticated device still registered? `None` = undetermined; the
    /// client fails closed.
    #[serde(default)]
    pub device_registered: Option<bool>,
    /// Cross-signing inputs for every `added_user_id` in `commits`, so replay
    /// verifies added devices without a round trip per add.
    #[serde(default)]
    pub added_identities: Vec<AddedIdentity>,
}

/// A user's cross-signing root plus the device rows a commit's
/// `added_device_ids` name: inputs to `IdentityDirectory::leaf_verdict`, never
/// its verdict. Verification stays on the client; the DS is not trusted with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddedIdentity {
    pub user_id: String,
    /// base64. `None` when the row or key is absent, which the client reads as
    /// "retry", never "verified".
    #[serde(default)]
    pub account_id_pub: Option<String>,
    /// One entry per device row that exists. A missing device is the client's
    /// `AbsentRetry` case; the DS never manufactures a placeholder.
    pub devices: Vec<DeviceCertRow>,
}

/// One `user_device` row's cross-signing columns, blobs base64. Each is
/// genuinely nullable mid-enrollment: a NULL cert means "retry", a non-NULL
/// `revoked_at` is a verdict.
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

/// Drain this device's undelivered Welcomes (the read half of
/// `POST /v1/welcomes/ack`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchWelcomesBody {
    /// No-auth path only; with auth on, a mismatch with the signer is a 403.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    /// Also report the signing device's unclaimed KeyPackages in this suite
    /// (#1412), riding the drain every wake-up already makes. Packages drain
    /// without Welcomes too (lost epoch races, refused leaves), so a Welcome was
    /// never a sufficient refill trigger. Additive; `None` asks for no count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_key_packages_in_suite: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchWelcomesResponse {
    pub welcomes: Vec<PendingWelcome>,
    /// The count asked for in `count_key_packages_in_suite`. `None` when not
    /// asked, unreadable, or from an older DS; the client then refills only
    /// after processing a Welcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unclaimed_key_packages: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingWelcome {
    pub id: String,
    /// TLS-serialized MLS Welcome, base64.
    pub welcome: String,
    /// The conversation the Welcome row was filed under.
    ///
    /// The client refuses the join if this disagrees with the `GroupId` inside
    /// the signed Welcome: the DS can only check the recipient is in the
    /// conversation's desired roster, which an attacker can satisfy for their
    /// own group. `None` (an older DS) means the legacy path, not a mismatch.
    #[serde(default)]
    pub conversation_id: Option<String>,
}

/// A discontinuity in a served commit batch: an epoch missing between two that
/// are present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommitHole {
    /// The epoch that should have come next.
    pub expected: i64,
    /// The epoch actually found there.
    pub found: i64,
}

/// A commit batch is contiguous iff its epochs are consecutive. Pure and total.
///
/// A hole is reported, not refused: the client's response (drop the local group
/// and external-join onto the head) is the recovery, and refusing would leave a
/// damaged conversation unreadable forever. A batch starting above `since` is a
/// prune ([`ConversationState::pruned_below`]), not a hole.
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

    /// A missing middle epoch is reported with both epochs.
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

    /// Repeats and regressions are holes too.
    #[test]
    fn repeats_and_regressions_are_holes() {
        assert!(check_contiguous(&[wire(3), wire(3)]).is_err());
        assert!(check_contiguous(&[wire(3), wire(2)]).is_err());
    }
}
