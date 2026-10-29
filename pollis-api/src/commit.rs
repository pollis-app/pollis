//! The MLS control plane: commit submission and the Welcomes it carries.
//!
//! Wire types only; see the matching `pollis-delivery` module for behaviour.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SubmitBody {
    pub conversation_id: String,
    /// The suite generation this commit belongs to; absent (pre-hybrid) → `0`.
    #[serde(default)]
    pub generation: i64,
    /// The epoch this commit was built from: the head the client believes it is at.
    pub based_on_epoch: i64,
    /// Migration only (`generation = N + 1`, `based_on_epoch = 0`): the head of
    /// generation `N` this migration closes, making it a compare-and-swap, so a
    /// commit landing in `N` after the roster read invalidates the migration
    /// instead of being orphaned. Must be absent otherwise.
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
    /// New published GroupInfo at the resulting epoch, base64, for external
    /// joins.
    #[serde(default)]
    pub group_info: Option<String>,
    /// Welcomes for devices added by this commit.
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

/// `POST /v1/commits/since`: the device's catch-up high-water for one
/// conversation; the retention floor is its MIN across members (#539).
/// Device-signed with values in the body, since it raises the floor (the
/// unauthenticated GET it replaced could wipe a commit log, #681).
#[derive(Debug, Serialize, Deserialize)]
pub struct CommitSinceReport {
    pub conversation_id: String,
    /// The suite lineage of this position (#454); absent → `0`. Kept monotone
    /// lexicographically with `since`, so a migrated device's epoch 0 is not a
    /// regression.
    #[serde(default)]
    pub generation: i64,
    pub since: i64,
    /// No-auth (dev/test) path only; ignored when the DS enforces auth.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// `POST /v1/commits`: accepted, or rejected with the head to re-base onto. The
/// HTTP status (200 / 409) mirrors the variant.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SubmitResponse {
    /// The commit won its epoch. The lineage head is now `epoch + 1`.
    Accepted { generation: i64, epoch: i64 },
    /// Not at the head: the current head and the missing commits, to re-base
    /// and resubmit. Scoped to the submitter's generation; a `head_generation`
    /// above it means the conversation migrated and the client should take the
    /// Welcome instead of re-basing.
    Rejected {
        head: i64,
        head_generation: i64,
        missing: Vec<CommitWire>,
    },
}

/// One commit row as it crosses the wire (blobs base64).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitWire {
    pub generation: i64,
    pub epoch: i64,
    pub seq: i64,
    pub sender_id: String,
    /// base64
    pub commit: String,
    pub added_user_id: Option<String>,
    pub added_device_ids: Option<String>,
    pub created_at: String,
}
