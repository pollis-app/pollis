//! The pre-identity OTP endpoints. Gated by the OTP itself — no device signature
//! and no session.
//!
//! Wire types only; see `pollis-delivery` for handling.

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
    /// Ignored: verify-otp never writes `account_id_pub` (see `establish-identity`).
    #[serde(default)]
    pub account_id_pub: Option<String>,
}

/// `POST /v1/auth/verify-otp` — the minted session and the account it belongs
/// to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyOtpResponse {
    pub user_id: String,
    pub username: String,
    /// True when THIS verify created the account row.
    pub is_new_account: bool,
    /// Whether the account already has a published `account_id_pub`. Drives the
    /// client's "establish identity" vs "enroll this device" branch.
    pub has_identity: bool,
    pub session_token: String,
    /// Unix seconds.
    pub session_expires_at: i64,
    /// The account's published `account_id_pub`, base64, when it has one
    /// (`has_identity`).
    ///
    /// Rides on the verify (#987) for the orphan check; it is the caller's own public key.
    #[serde(default)]
    pub account_id_pub: Option<String>,
}
