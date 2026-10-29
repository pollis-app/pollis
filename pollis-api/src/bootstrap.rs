//! First-contact bootstrap, gated by the OTP session rather than a device
//! signature: identity establishment, device registration, cert publication and
//! enrollment requests. Wire types only.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct EstablishIdentityBody {
    /// New account identity public key, base64.
    pub account_id_pub: String,
    /// `account_recovery` blob, all base64.
    pub salt: String,
    pub nonce: String,
    pub wrapped_key: String,
}

#[derive(Serialize, Deserialize)]
pub struct RegisterDeviceBody {
    /// Must equal the session's device; a mismatch is rejected.
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct PublishCertBody {
    pub device_id: String,
    /// ML-DSA-44 device cert, base64.
    pub device_cert: String,
    /// Unix seconds the cert was issued at (stored as TEXT).
    pub cert_issued_at: i64,
    pub cert_identity_version: u32,
    /// Raw 32-byte Ed25519 MLS signing pubkey, base64.
    pub mls_signature_pub: String,
    /// Raw ML-DSA-44 MLS signing pubkey, base64. Cert v2 binds both keys, so both
    /// are needed to re-derive the signed payload; this one verifies device
    /// signatures.
    pub mls_signature_pub_pq: String,
    /// The publishing user. Ignored with a session; required on the
    /// subsequent-device path (no session), where verifying the cert against
    /// this user's `account_id_pub` is the proof.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct EnrollmentRequestBody {
    pub request_id: String,
    /// New device's ephemeral X25519 public key, base64; the private half stays
    /// on the device.
    pub new_device_ephemeral_pub: String,
    pub verification_code: String,
    pub created_at: String,
    pub expires_at: String,
    /// QR device link (#1207): base64 `HMAC-SHA256(mac_key, link_id ‖ request_id
    /// ‖ new_device_id ‖ ephemeral_pub)`. Required for a device-link session,
    /// refused otherwise.
    #[serde(default)]
    pub link_tag: Option<String>,
}
