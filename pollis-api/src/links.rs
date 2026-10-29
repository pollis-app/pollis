//! QR device links: a device is signed in by a QR instead of the email OTP.
//! Protocol and threat model: `docs/qr-device-link-design.md`.
//!
//! - **v1 (#1207)**, shown by the signed-in device: `create`, then the new
//!   device `claim`s it with the QR's secret.
//! - **v2 (#1361)**, shown by the new device: `offer`, a signed-in device
//!   `accept-offer`s it, the new device collects its session with `poll-offer`.
//!   The QR carries only the new device's ephemeral public key.

use serde::{Deserialize, Serialize};

/// `POST /v1/link/create`: device-signed, from the enrolled device showing the
/// QR. The DS binds the account and sets the expiry.
#[derive(Serialize, Deserialize)]
pub struct CreateLinkBody {
    /// Client-chosen id (a ULID); the QR carries it.
    pub link_id: String,
    /// `SHA-256(claim)`, base64, where `claim = HKDF(t, "pollis-link-claim-v1")`
    /// and only the QR carries `t`; the DS cannot derive the token or MAC key.
    pub claim_verifier: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateLinkResponse {
    /// Unix seconds, server-set.
    pub expires_at: i64,
}

/// `POST /v1/link/claim`: pre-credential, from the scanning phone. Single use;
/// later claims get the same 401 as a wrong secret.
#[derive(Serialize, Deserialize)]
pub struct ClaimLinkBody {
    pub link_id: String,
    /// `claim`, base64, checked in constant time against the stored verifier.
    pub claim: String,
    /// The phone's device id; the minted session binds to it.
    pub device_id: String,
    /// Shown on the approving device. Untrusted; the link tag authenticates.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// The claim's answer: a device-link-scoped session (enrollment only, not the
/// soft reset) and the account, in the verify-otp shape minus signup fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimLinkResponse {
    pub user_id: String,
    pub username: String,
    pub email: String,
    /// The account's published identity key, base64, to check the unwrapped key.
    pub account_id_pub: Option<String>,
    pub session_token: String,
    /// Unix seconds.
    pub session_expires_at: i64,
}

/// `POST /v1/link/status`: device-signed by the link's creator.
#[derive(Serialize, Deserialize)]
pub struct LinkStatusBody {
    pub link_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    /// Showing; nobody has claimed it.
    Open,
    /// A phone claimed it and holds an enrollment-only session.
    Claimed,
    /// That phone filed its enrollment request; approve it with the tag.
    Requested,
    /// Past its lifetime, unknown, or swept.
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkStatusResponse {
    pub state: LinkState,
    pub new_device_id: Option<String>,
    pub device_name: Option<String>,
    /// Set once `Requested`: the enrollment request the phone filed.
    pub request_id: Option<String>,
    /// Set once `Requested`: base64 `HMAC-SHA256(mac_key, link_id ‖ request_id ‖
    /// new_device_id ‖ ephemeral_pub)`, verified by the approver before wrapping.
    /// Always `None` for a v2 offer, whose approver compares the request's key
    /// with the QR's.
    pub link_tag: Option<String>,
}

/// `POST /v1/link/offer`: pre-credential, from the new device showing a v2 QR
/// (#1361). Nothing here lets the DS or a QR photographer act as that device.
#[derive(Serialize, Deserialize)]
pub struct OfferLinkBody {
    /// Client-chosen id (a ULID); the QR carries it.
    pub link_id: String,
    /// `SHA-256(poll_secret)`, base64. The 32-byte secret stays on the new device
    /// (not in the QR), so a photo of the QR cannot collect the session.
    pub poll_verifier: String,
    /// The new device's id; the session it collects binds to it.
    pub device_id: String,
    /// Shown on the approving device. Untrusted.
    #[serde(default)]
    pub device_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferLinkResponse {
    /// Unix seconds, server-set; a fresh QR is shown after this.
    pub expires_at: i64,
}

/// `POST /v1/link/accept-offer`: device-signed by the scanning device. Binds the
/// offer to the signer's account, once; then followed via `/v1/link/status`.
#[derive(Serialize, Deserialize)]
pub struct AcceptOfferBody {
    pub link_id: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptOfferResponse {
    /// The device that offered the link, named by its session.
    pub new_device_id: String,
    pub device_name: Option<String>,
}

/// `POST /v1/link/poll-offer`: pre-credential, from the waiting new device.
/// Every failure is the same 401.
#[derive(Serialize, Deserialize)]
pub struct PollOfferBody {
    pub link_id: String,
    /// `poll_secret`, base64.
    pub poll_secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollOfferResponse {
    /// `None` until accepted; then the enrollment-only session, delivered once.
    pub session: Option<ClaimLinkResponse>,
}
