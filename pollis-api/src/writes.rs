//! MLS control-plane side writes: GroupInfo republication and the Welcome
//! lifecycle (ack / reset / purge / resubmit).
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct GroupInfoBody {
    pub conversation_id: String,
    /// The suite generation this GroupInfo belongs to (#454 P4). Absent (a
    /// pre-hybrid client) → 0, the lineage such a client is in.
    #[serde(default)]
    pub generation: i64,
    pub epoch: i64,
    /// TLS-serialized MLS GroupInfo, base64 (STANDARD).
    pub group_info: String,
    pub updated_by_device_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct AckBody {
    pub welcome_ids: Vec<String>,
    /// Recipient, used ONLY on the no-auth path; when auth is on it must equal
    /// the authenticated user (or be absent).
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ResetBody {
    /// `Some` → reset only this device's (and device-agnostic) Welcomes (W6);
    /// `None` → reset all of the recipient's Welcomes (W7).
    #[serde(default)]
    pub device_id: Option<String>,
    /// Recipient, used ONLY on the no-auth path (see `pollis_delivery::writes::resolve_recipient`).
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct PurgeBody {
    /// Recipient, used ONLY on the no-auth path (see `pollis_delivery::writes::resolve_recipient`).
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ResubmitBody {
    pub conversation_id: String,
    /// The suite generation of the group this Welcome admits the recipient to
    /// (#454 P4). Absent → 0, the lineage every pre-hybrid group is in.
    #[serde(default)]
    pub generation: i64,
    pub recipient_id: String,
    pub recipient_device_id: String,
    /// TLS-serialized MLS Welcome, base64 (STANDARD).
    pub welcome: String,
}

// ── Responses (#922) ─────────────────────────────────────────────────────────

/// `POST /v1/welcomes/ack` and `POST /v1/welcomes/reset` — how many
/// `mls_welcome` rows the write touched.
///
/// One type for both because they answer the same bytes; the ENDPOINT is still
/// distinguished by the request type, which is what carries the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum WelcomesUpdated {
    Ok { updated: u64 },
}

/// `POST /v1/welcomes/purge` — how many `mls_welcome` rows were deleted.
///
/// Purge deletes where ack/reset update, so the count is named `deleted` rather
/// than `updated`. A separate type rather than a shared "count" struct: the two
/// numbers mean different things, and the wire has always spelled them
/// differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum WelcomesPurged {
    Ok { deleted: u64 },
}
