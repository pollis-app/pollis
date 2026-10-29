//! The MLS control plane's read half (#987): one signed POST answering every
//! replay/join question from ONE server-side read transaction, so pairs the
//! client evaluates together (GroupInfo + pending Welcome, head + commits)
//! cannot be torn. POST because the signature does not cover a query string
//! (#681). See `.codesight/wiki/commands.md` → "Four properties".

use serde::{Deserialize, Serialize};

use crate::commit::CommitWire;

/// Everything the MLS replay/join path needs, for one or more conversations, in
/// one signed round trip. Batched so a cold-launch sweep is not one round trip
/// per conversation (#987 §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationStateBody {
    pub queries: Vec<ConversationStateQuery>,
    /// No-auth (dev/test) path only; with auth on, both come from the signature.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// One conversation's question. Every field beyond `conversation_id` is opt-in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationStateQuery {
    pub conversation_id: String,
    /// Lineage for `head`/`commits`/`commit_at_epoch`; `None` → head generation.
    #[serde(default)]
    pub generation: Option<i64>,
    /// Fetch commits with `epoch >= since` in `generation`; `None` → no commit scan.
    #[serde(default)]
    pub since: Option<i64>,
    /// Include the GroupInfo blob (the whole ratchet tree), not just its head.
    /// Only `external_join_attempt` needs it.
    #[serde(default)]
    pub want_group_info_blob: bool,
    /// Fetch the commit bytes at this epoch: input to `reconcile`'s
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

/// One conversation's snapshot, every log-DB field read in one transaction.
///
/// The main-DB gates are `Option<bool>` so each keeps its own failure bias at
/// its call site: `None` = undetermined; `is_member`/`device_registered` fail
/// CLOSED, `welcome_pending` fails OPEN. Do not collapse them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationState {
    pub conversation_id: String,
    /// `false` → not a member and every field below is empty. Per entry rather
    /// than a batch-wide 403, so one stale id cannot blind the whole sweep.
    pub authorized: bool,
    #[serde(default)]
    pub group_info: Option<GroupInfoState>,
    /// An undelivered `mls_welcome` for this user+device exists. Read in the same
    /// transaction as `group_info`; that pairing is why this endpoint exists.
    pub welcome_pending: bool,
    /// Head epoch WITHIN `generation` (`MAX(epoch) + 1`, 0 for an empty lineage).
    pub head: i64,
    /// The conversation's newest lineage.
    pub head_generation: i64,
    /// The lineage `head`, `commits` and `commit_at_epoch` are scoped to.
    pub generation: i64,
    /// `epoch >= since` in `generation`, ascending.
    #[serde(default)]
    pub commits: Vec<CommitWire>,
    /// A discontinuity inside `commits`, computed by [`check_contiguous`] in the
    /// snapshot transaction; `None` normally.
    #[serde(default)]
    pub hole: Option<CommitHole>,
    /// The lowest epoch still retained in `generation`. `pruned_below > since`
    /// marks a genuine retention prune (#539), where re-joining is correct, as
    /// opposed to a torn read, where it would be data loss.
    #[serde(default)]
    pub pruned_below: Option<i64>,
    /// base64 commit bytes at `commit_at_epoch`, when asked and present.
    #[serde(default)]
    pub commit_at_epoch: Option<String>,
    /// Is the authenticated user a current member? `None` = undetermined; the
    /// client fails CLOSED (a membership leak is worse than a delayed recovery).
    #[serde(default)]
    pub is_member: Option<bool>,
    /// Is the authenticated device still registered? `None` = undetermined;
    /// fails CLOSED.
    #[serde(default)]
    pub device_registered: Option<bool>,
    /// The authenticated device's `user_device.created_at`, verbatim (#1571).
    /// With [`Self::member_since`] it tells whether the device was already
    /// registered when its user joined: if so, a late join at the head is a
    /// visible loss, not the silent "sent before you joined" case. `None` = not
    /// reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_registered_at: Option<String>,
    /// When the authenticated user became a member (`dm_channel_member.added_at`
    /// or `group_member.joined_at`, #1571). `None` = not reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_since: Option<String>,
    /// Cross-signing inputs for every `added_user_id` in `commits`, so replay
    /// needs no round trip per add.
    #[serde(default)]
    pub added_identities: Vec<AddedIdentity>,
}

/// A user's cross-signing root plus the device rows a commit's `added_device_ids`
/// name: inputs to `IdentityDirectory::leaf_verdict`, never its verdict (the DS
/// is not trusted to evaluate the signature chain).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddedIdentity {
    pub user_id: String,
    /// base64. `None` when the row is absent or the key is NULL: "not replicated
    /// yet, retry", never "verified".
    #[serde(default)]
    pub account_id_pub: Option<String>,
    /// One entry per device row that exists. A missing device is the client's
    /// `AbsentRetry` case; the DS never fabricates a placeholder, because
    /// "absent" and "present with null columns" are handled differently.
    pub devices: Vec<DeviceCertRow>,
}

/// One `user_device` row's cross-signing columns, blobs base64. Each is
/// `Option` because a NULL cert column on a live row means "not landed yet"
/// (retry) while non-NULL `revoked_at` is a verdict; replay depends on that.
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

/// Drain this device's undelivered Welcomes (read half of `POST /v1/welcomes/ack`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchWelcomesBody {
    /// No-auth path only; with auth on the recipient is the signer and a mismatch
    /// is a 403.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    /// Also report the signing device's unclaimed KeyPackages in this suite
    /// (#1412), so every wake-up's drain can trigger a refill: packages are
    /// also consumed without a Welcome (lost epoch race, refused leaf). `None`
    /// asks for no count; an older DS ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_key_packages_in_suite: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchWelcomesResponse {
    pub welcomes: Vec<PendingWelcome>,
    /// The signing device's unclaimed KeyPackages in the requested suite. `None`
    /// when not asked, unreadable (the drain never fails on it), or from an older
    /// server; the client then refills only after processing a Welcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unclaimed_key_packages: Option<i64>,
    /// Whether the signing device holds a last-resort KeyPackage in that suite
    /// (#1571), on the same terms. `Some(false)` is the cue to publish one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_last_resort_key_package: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingWelcome {
    pub id: String,
    /// TLS-serialized MLS Welcome, base64.
    pub welcome: String,
    /// The conversation the Welcome row was filed under.
    ///
    /// The client refuses the join if this disagrees with the `GroupId` inside
    /// the Welcome blob: the half of C1 the DS cannot check, since an attacker
    /// can satisfy `writes::in_desired_roster` for their own group but cannot
    /// file a victim-`GroupId` Welcome under the victim's conversation. `None`
    /// (older DS) means "not said" and takes the legacy path, not a mismatch.
    #[serde(default)]
    pub conversation_id: Option<String>,
}

/// A discontinuity in a served commit batch: an epoch missing between two that
/// are present. Reported on [`ConversationState::hole`].
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
/// A hole is reported, not refused: the client's response (drop the local
/// group, external-join onto the head) is the recovery, and refusing would
/// leave a damaged conversation unreadable forever. Computing it inside the
/// snapshot transaction is what rules out phantom holes (#987). A moved floor
/// is a prune ([`ConversationState::pruned_below`]), not a hole.
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

    /// Reported with both epochs so a log line locates the gap without a query.
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

    /// `epoch + 1` is the only accepted step, so repeats and regressions are holes.
    #[test]
    fn repeats_and_regressions_are_holes() {
        assert!(check_contiguous(&[wire(3), wire(3)]).is_err());
        assert!(check_contiguous(&[wire(3), wire(2)]).is_err());
    }
}
