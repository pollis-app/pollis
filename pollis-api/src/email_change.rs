//! Device-signed email change, a two-step OTP flow separate from the signup OTP.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct RequestEmailChangeBody {
    pub new_email: String,
}

#[derive(Serialize, Deserialize)]
pub struct VerifyEmailChangeBody {
    pub new_email: String,
    pub code: String,
}

/// `POST /v1/auth/verify-email-change` — the swap, plus the caller's username
/// (#987).
///
/// The client mirrors the new address into its local `accounts.json` so the
/// login screen's "continue as" picker shows it, and that mirror needs the
/// username. Reading it back afterwards was a second round trip for a row the
/// DS had just written — and one that could observe a rename that happened in
/// between and stamp it as part of this change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum EmailChanged {
    Ok {
        /// `None` when the row has no username, which is a shape the accounts
        /// index already tolerates.
        #[serde(default)]
        username: Option<String>,
    },
}
