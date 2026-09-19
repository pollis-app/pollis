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
    /// The code sent to the NEW address — proof the caller controls the mailbox
    /// they are moving to.
    pub code: String,
    /// The code sent to the address the account is on TODAY (#1161) — proof the
    /// caller controls the mailbox that owns the account.
    ///
    /// The device signature and `code` are both satisfied by whoever is holding
    /// an unlocked device, so without this a borrowed or stolen device could
    /// move the account's recovery address. This is the proof that has to reach
    /// the real owner.
    ///
    /// `#[serde(default)]` is shape compatibility, not an opt-out: an account
    /// that has an address and does not answer its challenge is refused (401,
    /// `invalid current-address code`). A client that predates this field
    /// therefore cannot complete an email change, which is the fail-closed
    /// direction — a proof a caller may decline to give is not a proof.
    #[serde(default)]
    pub current_code: Option<String>,
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
