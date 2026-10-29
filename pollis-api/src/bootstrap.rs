//! First-contact bootstrap, gated by the OTP session rather than a device
//! signature: identity establishment, device registration, cert publication and
//! enrollment requests.
//!
//! Wire types only; see `pollis-delivery` for handling.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct EstablishIdentityBody {
    /// New account identity public key, base64 (STANDARD).
    pub account_id_pub: String,
    /// `account_recovery` blob, all base64 (STANDARD).
    pub salt: String,
    pub nonce: String,
    pub wrapped_key: String,
}

#[derive(Serialize, Deserialize)]
pub struct RegisterDeviceBody {
    /// Must equal the session's device. Bound from the session regardless; a
    /// mismatch is rejected so a token can't register some other device.
    pub device_id: String,
    #[serde(default)]
    pub device_name: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct PublishCertBody {
    pub device_id: String,
    /// ML-DSA-44 device cert, base64 (STANDARD).
    pub device_cert: String,
    /// Unix seconds the cert was issued at (stored as TEXT — lossless u64 round
    /// trip for later verification, mirroring `device.rs`).
    pub cert_issued_at: i64,
    pub cert_identity_version: u32,
    /// Raw 32-byte Ed25519 MLS signing pubkey, base64 (STANDARD) — the column the
    /// device-signature gate verifies against.
    pub mls_signature_pub: String,
    /// Raw ML-DSA-44 MLS signing pubkey, base64 (STANDARD), bound by cert v2.
    pub mls_signature_pub_pq: String,
    /// The publishing user: ignored with a session, required on the subsequent-device path.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct EnrollmentRequestBody {
    pub request_id: String,
    /// New device's ephemeral X25519 public key, base64 (STANDARD). The private
    /// half never leaves the requesting device.
    pub new_device_ephemeral_pub: String,
    pub verification_code: String,
    pub created_at: String,
    pub expires_at: String,
    /// QR device link (#1207): base64 `HMAC-SHA256(mac_key, link_id ‖ request_id
    /// ‖ new_device_id ‖ ephemeral_pub)`. Required when the session is a
    /// device-link session, refused otherwise.
    #[serde(default)]
    pub link_tag: Option<String>,
}
