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
