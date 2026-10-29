//! Device-signed email change, a two-step OTP flow separate from the signup OTP.
//!
//! Wire types only; see `pollis-delivery` for handling.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct RequestEmailChangeBody {
    pub new_email: String,
}

#[derive(Serialize, Deserialize)]
pub struct VerifyEmailChangeBody {
    pub new_email: String,
    /// The code sent to the NEW address — proof the caller controls the mailbox
    /// they are moving to.
    pub code: String,
    /// The code sent to the address the account is on TODAY (#1161) — proof the
    /// caller controls the mailbox that owns the account.
    ///
    /// Without it a stolen unlocked device could move the recovery address; required despite `serde(default)`.
    #[serde(default)]
    pub current_code: Option<String>,
}

/// `POST /v1/auth/verify-email-change` — the swap, plus the caller's username
/// (#987).
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
