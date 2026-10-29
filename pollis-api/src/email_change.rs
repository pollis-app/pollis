//! Device-signed email change, a two-step OTP flow separate from the signup OTP.
//! Wire types only.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct RequestEmailChangeBody {
    pub new_email: String,
}

#[derive(Serialize, Deserialize)]
pub struct VerifyEmailChangeBody {
    pub new_email: String,
    /// The code sent to the new address (proves control of it).
    pub code: String,
    /// The code sent to the current address (#1161). An unlocked device alone
    /// satisfies the signature and `code`, so without this a stolen device could
    /// move the recovery address. Not optional in effect: an account with an
    /// address that omits it is refused (401), so older clients fail closed.
    #[serde(default)]
    pub current_code: Option<String>,
}

/// `POST /v1/auth/verify-email-change`: the swap plus the caller's username
/// (#987), for mirroring the address into `accounts.json` without a re-read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum EmailChanged {
    Ok {
        /// `None` when the row has no username (the accounts index tolerates it).
        #[serde(default)]
        username: Option<String>,
    },
}
