//! The pre-identity OTP endpoints, gated by the OTP itself (no device signature,
//! no session). Wire types only.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct RequestOtpBody {
    pub email: String,
}

#[derive(Serialize, Deserialize)]
pub struct VerifyOtpBody {
    pub email: String,
    pub code: String,
    pub device_id: String,
    /// Informational only and ignored: identity is established by the
    /// CAS-guarded `establish-identity`, never here.
    #[serde(default)]
    pub account_id_pub: Option<String>,
}

/// `POST /v1/auth/verify-otp`: the minted session and the account it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyOtpResponse {
    pub user_id: String,
    pub username: String,
    /// True when THIS verify created the account row.
    pub is_new_account: bool,
    /// Whether the account has a published `account_id_pub`: "establish
    /// identity" vs "enroll this device".
    pub has_identity: bool,
    pub session_token: String,
    /// Unix seconds.
    pub session_expires_at: i64,
    /// The account's published `account_id_pub`, base64, when `has_identity`
    /// (#987), for the client's orphan check. Not a disclosure: it is the
    /// caller's own account (the OTP proved the mailbox) and a public key.
    #[serde(default)]
    pub account_id_pub: Option<String>,
}
