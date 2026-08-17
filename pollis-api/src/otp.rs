//! The pre-identity OTP endpoints. Gated by the OTP itself — no device signature
//! and no session.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

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
    /// Informational only — verify-otp NEVER writes `account_id_pub` (identity is
    /// established by the separate, CAS-guarded `establish-identity`). Accepted
    /// for forward-compat with the client and deliberately ignored here.
    #[serde(default)]
    pub account_id_pub: Option<String>,
}

/// `POST /v1/auth/verify-otp` — the minted session and the account it belongs
/// to.
///
/// Typed rather than read out of a `serde_json::Value` by string key, which is
/// what `pollis-core` did on both of its call sites: `v["session_token"]`
/// compiles whatever the server calls that field, so a rename would have turned
/// into a runtime "response missing session_token" on every sign-in.
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
}
