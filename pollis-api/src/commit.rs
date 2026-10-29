//! The MLS control plane: commit submission and the Welcomes it carries.
//!
//! Wire types only; see `pollis-delivery` for handling.

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
    /// Migration only: the generation-`N` head this migration closes, making it a CAS on the old lineage.
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
/// Device-signed: it raises the retention floor, so the values must be bound to the signature (#681).
#[derive(Debug, Serialize, Deserialize)]
pub struct CommitSinceReport {
    pub conversation_id: String,
    /// The suite lineage (#454 P4); absent → `0`. `(generation, since)` is kept monotone lexicographically.
    #[serde(default)]
    pub generation: i64,
    pub since: i64,
    /// No-auth (dev/test) path only; ignored when auth is enforced.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// `POST /v1/commits` — accepted, or rejected with the head to re-base onto.
///
/// One type for both; the HTTP status (200 / 409) mirrors the variant.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SubmitResponse {
    /// The commit won its epoch. The lineage head is now `epoch + 1`.
    Accepted { generation: i64, epoch: i64 },
    /// The client wasn't at the head. Here's the current head and the commits
    /// it's missing so it can re-base and resubmit — no fork possible.
    ///
    /// `head` and `missing` are per the submitter's generation; a higher `head_generation` means it migrated.
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
