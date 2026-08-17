//! Account lifecycle: identity rotation, deletion, recovery, security events,
//! enrollment approval/rejection, device revocation and logout.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct RotateIdentityBody {
    /// The `identity_version` the client read BEFORE rotating — i.e. the head of
    /// `account_key_log` it believes it is appending onto. The new version is
    /// `based_on_version + 1`. This is the CAS expectation, the exact analogue of
    /// a commit's `based_on_epoch`.
    pub based_on_version: i64,
    /// New account identity public key, base64 (STANDARD).
    pub account_id_pub: String,
    /// New `account_recovery` blob, all base64 (STANDARD).
    pub salt: String,
    pub nonce: String,
    pub wrapped_key: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SecurityEventBody {
    pub kind: String,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub metadata: Option<String>,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ApproveEnrollmentBody {
    pub request_id: String,
    /// base64 (STANDARD) of `approver_pub || nonce || ciphertext`.
    pub wrapped_account_key: String,
    pub approved_by_device_id: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct RejectEnrollmentBody {
    pub request_id: String,
    pub approved_by_device_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct RevokeDeviceBody {
    /// The device being revoked (one of the actor's OWN devices, not the caller).
    pub device_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct LogoutDeviceBody {
    /// The device logging out — in practice the signing device itself. Bound
    /// `WHERE user_id = actor`, so a caller can only remove a device on THEIR OWN
    /// account.
    pub device_id: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ResetRecoverBody {
    /// The device performing the reset — its `user_device` row is KEPT (it stays
    /// enrolled under the new identity); every OTHER device of the actor is
    /// dropped. `None` → drop ALL of the actor's devices.
    #[serde(default)]
    pub current_device_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DeleteAccountBody {
    #[serde(default)]
    pub user_id: Option<String>,
}

/// `POST /v1/account/rotate-identity` — the `identity_version` the rotation
/// landed on.
///
/// A lost CAS race answers 409 (with the current head) rather than this type:
/// the client must re-read and retry, and conflating "applied" with "rejected"
/// in one body is how a caller ends up treating a refusal as a success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RotateIdentityResponse {
    Ok { identity_version: i64 },
}
