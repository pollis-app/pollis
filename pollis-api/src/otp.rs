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
