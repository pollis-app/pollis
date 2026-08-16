//! The MLS control plane: commit submission and the Welcomes it carries.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SubmitBody {
    pub conversation_id: String,
    /// The suite generation this commit belongs to. Absent (pre-hybrid client) →
    /// `0`, which is exactly the lineage such a client is in.
    #[serde(default)]
    pub generation: i64,
    /// The epoch this commit was built from = the head the client believes it's at.
    pub based_on_epoch: i64,
    /// Migration only (`generation = N + 1`, `based_on_epoch = 0`): the head the
    /// submitter observed on generation `N`, which this migration CLOSES. Making
    /// the migrator name it turns "open the next lineage" into a compare-and-swap
    /// on the old one, so a commit that lands in generation `N` after the
    /// migrator read the roster invalidates the migration instead of being
    /// silently orphaned by it. Ignored (and must be absent) for a same-generation
    /// commit.
    #[serde(default)]
    pub closes_epoch: Option<i64>,
    pub sender_id: String,
    /// TLS-serialized MLS Commit, base64.
    pub commit: String,
    #[serde(default)]
    pub added_user_id: Option<String>,
    /// CSV of device ids added by this commit, if any.
    #[serde(default)]
    pub added_device_ids: Option<String>,
    /// New published GroupInfo at the *resulting* epoch (`based_on_epoch + 1`),
    /// base64. Lets a future joiner external-join. Optional.
    #[serde(default)]
    pub group_info: Option<String>,
    /// Welcomes for devices added by this commit. Optional.
    #[serde(default)]
    pub welcomes: Vec<WelcomeBody>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WelcomeBody {
    pub recipient_id: String,
    pub recipient_device_id: String,
    /// TLS-serialized MLS Welcome, base64.
    pub welcome: String,
}

/// `POST /v1/commits/since` — the device's catch-up high-water for one
/// conversation, the signal the server-side retention floor is the MIN of across
/// current members (#539, I4 Tier 1).
///
/// A WRITE, and device-signed: it raises the retention floor, so it must be
/// authenticated as the device it claims to be. It used to be an unauthenticated
/// GET with the values in the query string, which let anyone raise the floor for
/// any device and, chained with the unclamped prune floor, wipe a conversation's
/// whole commit log (#681). Living in the signed BODY is what binds the reported
/// values to the signature's `sha256(body)`.
#[derive(Debug, Serialize, Deserialize)]
pub struct CommitSinceReport {
    pub conversation_id: String,
    /// The suite lineage this position belongs to (#454 P4); absent → `0`. The
    /// DS keeps `(generation, since)` monotone lexicographically, which is what
    /// stops a migrated device's report — epoch 0 of generation `N + 1`,
    /// numerically below the retired lineage's last epoch — reading as a
    /// regression.
    #[serde(default)]
    pub generation: i64,
    pub since: i64,
    /// No-auth (dev/test) path only. When the DS enforces auth both halves come
    /// from the verified signature and these are ignored — though still
    /// signature-bound, since the signature covers the whole body.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}
