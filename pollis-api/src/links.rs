//! QR device links: a device is signed in by a QR instead of the email OTP.
//! Protocol and threat model: `docs/qr-device-link-design.md`.
//!
//! Two directions share one store and one status read:
//!
//! - **v1 (#1207), shown by the signed-in device:** `create`, then the new
//!   device `claim`s it with the secret the QR carries.
//! - **v2 (#1361), shown by the NEW device:** `offer`, then a signed-in device
//!   that scanned it `accept-offer`s, then the new device collects its session
//!   with `poll-offer`. The QR carries the new device's ephemeral public key,
//!   never a secret.
//!
//! Wire types only; see `pollis-delivery/src/links.rs`.

use serde::{Deserialize, Serialize};

/// `POST /v1/link/create` — device-signed, from the enrolled device showing the
/// QR. The DS binds the account from the signature and sets the expiry itself.
#[derive(Serialize, Deserialize)]
pub struct CreateLinkBody {
    /// Client-chosen id (a ULID); the QR carries it.
    pub link_id: String,
    /// `SHA-256(claim)`, base64 (STANDARD), where `claim = HKDF(t,
    /// "pollis-link-claim-v1")` and `t` is the token only the QR carries. The DS
    /// stores this and nothing that lets it derive the token or the MAC key.
    pub claim_verifier: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateLinkResponse {
    /// Unix seconds. Server-set; the QR is dead after this.
    pub expires_at: i64,
}

/// `POST /v1/link/claim` — pre-credential, from the phone that scanned the QR.
/// Single use: the first valid claim wins and every later one gets the same 401
/// as a wrong secret.
#[derive(Serialize, Deserialize)]
pub struct ClaimLinkBody {
    pub link_id: String,
    /// `claim`, base64 (STANDARD). The DS checks `SHA-256(claim)` against the
    /// stored verifier in constant time.
    pub claim: String,
    /// The phone's device id; the minted session binds to it.
    pub device_id: String,
    /// Shown on the approving device ("<name> wants to sign in"). Untrusted
    /// display text — the approval is authenticated by the link tag, not this.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// The claim's answer: an enrollment-only session and the account it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimLinkResponse {
    pub user_id: String,
    pub username: String,
    pub email: String,
    /// The account's published identity key, base64 — the phone checks the
    /// unwrapped key against it, exactly as linking does.
    pub account_id_pub: Option<String>,
    pub session_token: String,
    /// Unix seconds.
    pub session_expires_at: i64,
}

/// `POST /v1/link/status` — device-signed, from the device that created the
/// link; the link must belong to the signer's account.
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
    /// new_device_id ‖ ephemeral_pub)`. The approver verifies it before wrapping.
    /// Always `None` for a v2 offer: its approver compares the request's key
    /// with the one it read off the QR instead.
    pub link_tag: Option<String>,
}

/// `POST /v1/link/offer` — pre-credential, from the NEW device showing a v2 QR (#1361).
#[derive(Serialize, Deserialize)]
pub struct OfferLinkBody {
    /// Client-chosen id (a ULID); the QR carries it.
    pub link_id: String,
    /// `SHA-256(poll_secret)`, base64 (STANDARD); the secret is NOT in the QR, so a photo cannot collect the session.
    pub poll_verifier: String,
    /// The new device's id; the session it collects binds to it.
    pub device_id: String,
    /// Shown on the approving device. Untrusted display text.
    #[serde(default)]
    pub device_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferLinkResponse {
    /// Unix seconds. Server-set; the new device shows a fresh QR after this.
    pub expires_at: i64,
}

/// `POST /v1/link/accept-offer` — device-signed; binds the offer to the signer's account, once.
#[derive(Serialize, Deserialize)]
pub struct AcceptOfferBody {
    pub link_id: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptOfferResponse {
    /// The device that offered the link — the one its session will name.
    pub new_device_id: String,
    pub device_name: Option<String>,
}

/// `POST /v1/link/poll-offer` — pre-credential, from the new device waiting on
/// its v2 QR. Every failure (unknown, expired, wrong secret, already collected)
/// is the same 401.
#[derive(Serialize, Deserialize)]
pub struct PollOfferBody {
    pub link_id: String,
    /// `poll_secret`, base64 (STANDARD).
    pub poll_secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollOfferResponse {
    /// `None` while nobody has accepted the offer. Once accepted, the
    /// enrollment-only session — delivered exactly once — in the claim's shape.
    pub session: Option<ClaimLinkResponse>,
}
