//! Account lifecycle: identity rotation, deletion, recovery, security events,
//! enrollment approval/rejection, device revocation and logout.
//!
//! Wire types only; see the matching `pollis-delivery` module for behaviour.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct RotateIdentityBody {
    /// The `identity_version` read before rotating: the CAS expectation (like a
    /// commit's `based_on_epoch`); the new version is `based_on_version + 1`.
    pub based_on_version: i64,
    /// New account identity public key, base64.
    pub account_id_pub: String,
    /// New `account_recovery` blob, all base64.
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
    /// base64 of `approver_pub || nonce || ciphertext`.
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
    /// The device being revoked (one of the actor's own, not the caller).
    pub device_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// `POST /v1/devices/revoke`: the tombstone plus the device's name at
/// revocation (#987), for the security event that follows. `NotFound` because
/// a revoke is bound to the actor's devices, and reporting a no-op as success
/// would tell a user their stolen laptop was cut off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeviceRevoked {
    Ok {
        #[serde(default)]
        device_name: Option<String>,
    },
    /// No live device with that id on the caller's account.
    NotFound,
}

#[derive(Serialize, Deserialize)]
pub struct LogoutDeviceBody {
    /// The device logging out (in practice the signer), bound to the actor's own
    /// account.
    pub device_id: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ResetRecoverBody {
    /// The device performing the reset, kept enrolled under the new identity;
    /// every other device is dropped. `None` → drop all.
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

/// `POST /v1/account/rotate-identity`: the `identity_version` it landed on. A
/// lost CAS race is a 409 with the current head, never this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RotateIdentityResponse {
    Ok { identity_version: i64 },
}
