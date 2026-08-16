//! First-contact bootstrap, gated by the OTP session rather than a device
//! signature: identity establishment, device registration, cert publication and
//! enrollment requests.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

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
    /// Raw ML-DSA-44 MLS signing pubkey, base64 (STANDARD). Bound by cert v2
    /// alongside the classic key, so both must be presented to re-derive the
    /// signed payload here.
    pub mls_signature_pub_pq: String,
    /// The publishing user. IGNORED when a session is present (bound from the
    /// session instead). REQUIRED on the cert-validity-alone (subsequent-device)
    /// path, where there is no session to bind the user from — the cert
    /// verification against this user's `account_id_pub` is the load-bearing proof.
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
}
