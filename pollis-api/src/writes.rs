//! MLS control-plane side writes: GroupInfo republication and the Welcome
//! lifecycle (ack / reset / purge / resubmit).
//!
//! Wire types only; see the matching `pollis-delivery` module for behaviour.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct GroupInfoBody {
    pub conversation_id: String,
    /// The suite generation (#454); absent (pre-hybrid) → 0.
    #[serde(default)]
    pub generation: i64,
    pub epoch: i64,
    /// TLS-serialized MLS GroupInfo, base64.
    pub group_info: String,
    pub updated_by_device_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct AckBody {
    pub welcome_ids: Vec<String>,
    /// Recipient, no-auth path only; with auth on it must equal the
    /// authenticated user or be absent.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ResetBody {
    /// `Some` → reset only this device's (and device-agnostic) Welcomes (W6);
    /// `None` → reset all of the recipient's Welcomes (W7).
    #[serde(default)]
    pub device_id: Option<String>,
    /// Recipient, no-auth path only (`pollis_delivery::writes::resolve_recipient`).
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct PurgeBody {
    /// Recipient, no-auth path only (`pollis_delivery::writes::resolve_recipient`).
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ResubmitBody {
    pub conversation_id: String,
    /// The suite generation of the group the Welcome admits to (#454); absent → 0.
    #[serde(default)]
    pub generation: i64,
    pub recipient_id: String,
    pub recipient_device_id: String,
    /// TLS-serialized MLS Welcome, base64.
    pub welcome: String,
}

/// `POST /v1/welcomes/ack` and `/v1/welcomes/reset`: how many `mls_welcome`
/// rows the write touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum WelcomesUpdated {
    Ok { updated: u64 },
}

/// `POST /v1/welcomes/purge`: how many `mls_welcome` rows were deleted (named
/// `deleted` on the wire, unlike ack/reset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum WelcomesPurged {
    Ok { deleted: u64 },
}
