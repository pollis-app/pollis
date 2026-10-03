//! QR device link (#1207) — sign a new phone in from an enrolled, PIN-verified
//! device by scanning a QR, instead of the email OTP plus a typed SAS.
//! Protocol, threat model and guardrails: `docs/qr-device-link-design.md`.
//!
//! Two sides:
//!
//! - **Creator** (the enrolled device showing the QR): [`create_device_link`]
//!   (PIN required) → [`poll_device_link`] → [`approve_device_link`] /
//!   [`cancel_device_link`]. It holds the link token `t` in memory only.
//! - **Claimer** (the new phone): [`claim_device_link`] signs in with the
//!   enrollment-only session the claim mints, then the ordinary
//!   `start_device_enrollment` carries a link tag keyed by the QR's secret.
//!
//! The approval is authenticated by that tag, not by a typed code: the DS never
//! sees `t` (only `SHA-256(claim)`), so it cannot derive the MAC key and cannot
//! substitute the ephemeral key the account identity is wrapped to.

use std::sync::Arc;

use base64::Engine as _;
use hmac::{Hmac, Mac};
use rand::RngCore;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::state::AppState;

/// QR payload prefix. Versioned so a future change to the token or fields is a
/// clean refusal on an old client, not a misparse.
pub const LINK_PAYLOAD_PREFIX: &str = "pollis-link:v1:";

/// HKDF info for the claim secret the DS verifies (`SHA-256(claim)` stored).
const LINK_CLAIM_INFO: &[u8] = b"pollis-link-claim-v1";
/// HKDF info for the MAC key the DS never learns.
const LINK_MAC_INFO: &[u8] = b"pollis-link-mac-v1";

/// What the creator renders: the QR payload and when it stops working.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceLinkHandle {
    pub link_id: String,
    /// `pollis-link:v1:<link_id>:<base64url(t)>` — render as a QR, or show for
    /// manual entry.
    pub qr_payload: String,
    /// Unix seconds, server-set.
    pub expires_at: i64,
}

/// The creator's view of its link.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceLinkStatus {
    /// Showing; nobody has scanned it.
    Open,
    /// A phone scanned it and is signing in.
    Claimed { device_name: Option<String> },
    /// That phone is waiting for approval, and its key is authenticated by the
    /// link tag. Safe to show Approve.
    ReadyToApprove { device_name: Option<String>, new_device_id: String },
    /// The request's tag does NOT verify — the server (or someone between)
    /// altered it. Never approvable; the UI says so and offers a fresh QR.
    Tampered,
    /// Past its lifetime, already used, or unknown.
    Expired,
}

/// The claimer's in-memory link state, set by [`claim_device_link`] and read by
/// `start_device_enrollment` to tag its request.
pub struct PendingDeviceLink {
    pub link_id: String,
    pub mac_key: Zeroizing<[u8; 32]>,
}

fn kdf(token: &[u8], info: &[u8]) -> Zeroizing<[u8; 32]> {
    let hk = hkdf::Hkdf::<Sha256>::new(None, token);
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(info, out.as_mut()).expect("32 bytes is a valid HKDF-SHA256 length");
    out
}

/// The link tag: `HMAC-SHA256(mac_key, link_id ‖ request_id ‖ new_device_id ‖
/// ephemeral_pub)`, each field length-prefixed (u32 BE) so no two field splits
/// can produce the same MAC input.
pub(crate) fn link_tag(
    mac_key: &[u8; 32],
    link_id: &str,
    request_id: &str,
    new_device_id: &str,
    ephemeral_pub: &[u8],
) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(mac_key).expect("HMAC takes any key length");
    for field in [link_id.as_bytes(), request_id.as_bytes(), new_device_id.as_bytes(), ephemeral_pub] {
        mac.update(&(field.len() as u32).to_be_bytes());
        mac.update(field);
    }
    mac.finalize().into_bytes().into()
}

/// Parse a QR payload into `(link_id, token)`. Refuses anything that is not
/// exactly the v1 shape with a 32-byte token.
pub(crate) fn parse_payload(payload: &str) -> Result<(String, Zeroizing<Vec<u8>>)> {
    let rest = payload
        .trim()
        .strip_prefix(LINK_PAYLOAD_PREFIX)
        .ok_or_else(|| Error::Other(anyhow::anyhow!("That isn't a Pollis sign-in code.")))?;
    let (link_id, token_b64) = rest
        .split_once(':')
        .ok_or_else(|| Error::Other(anyhow::anyhow!("That sign-in code is incomplete.")))?;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token_b64)
        .map_err(|_| Error::Other(anyhow::anyhow!("That sign-in code is damaged.")))?;
    if link_id.is_empty() || link_id.len() > 64 || token.len() != 32 {
        return Err(Error::Other(anyhow::anyhow!("That sign-in code is damaged.")));
    }
    Ok((link_id.to_string(), Zeroizing::new(token)))
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ── Creator side ─────────────────────────────────────────────────────────────

/// Create a link and return the QR to show. Requires the device PIN — verified
/// here, with the unlock attempt counter, so no UI can skip it.
pub async fn create_device_link(state: &Arc<AppState>, user_id: String, pin: String) -> Result<DeviceLinkHandle> {
    crate::commands::pin::verify_pin(state, &user_id, &pin).await?;

    let mut token = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(token.as_mut());
    let claim = kdf(token.as_ref(), LINK_CLAIM_INFO);
    let verifier: [u8; 32] = Sha256::digest(claim.as_ref()).into();
    let link_id = Ulid::new().to_string();

    let resp: pollis_api::links::CreateLinkResponse = crate::commands::mls::ds_post_json(
        state,
        &pollis_api::links::CreateLinkBody {
            link_id: link_id.clone(),
            claim_verifier: b64(&verifier),
            user_id: Some(user_id.clone()),
        },
    )
    .await?;

    state
        .device_link_tokens
        .lock()
        .await
        .insert(link_id.clone(), Zeroizing::new(token.to_vec()));

    let qr_payload = format!(
        "{LINK_PAYLOAD_PREFIX}{link_id}:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token.as_ref())
    );
    Ok(DeviceLinkHandle { link_id, qr_payload, expires_at: resp.expires_at })
}

/// Where the link stands, with the request's tag verified when there is one.
pub async fn poll_device_link(state: &Arc<AppState>, user_id: String, link_id: String) -> Result<DeviceLinkStatus> {
    Ok(verified_status(state, &user_id, &link_id).await?.0)
}

/// [`poll_device_link`] plus, when approvable, the request facts the wrap
/// needs — read and verified in one pass so approval wraps to exactly the key
/// whose tag was checked.
async fn verified_status(
    state: &Arc<AppState>,
    user_id: &str,
    link_id: &str,
) -> Result<(DeviceLinkStatus, Option<(String, String, Vec<u8>)>)> {
    let token = match state.device_link_tokens.lock().await.get(link_id) {
        Some(t) => t.clone(),
        None => return Ok((DeviceLinkStatus::Expired, None)),
    };
    let st: pollis_api::links::LinkStatusResponse = crate::commands::mls::ds_post_json(
        state,
        &pollis_api::links::LinkStatusBody { link_id: link_id.to_string(), user_id: Some(user_id.to_string()) },
    )
    .await?;
    use pollis_api::links::LinkState;
    match st.state {
        LinkState::Open => Ok((DeviceLinkStatus::Open, None)),
        LinkState::Claimed => Ok((DeviceLinkStatus::Claimed { device_name: st.device_name }, None)),
        LinkState::Expired => {
            state.device_link_tokens.lock().await.remove(link_id);
            Ok((DeviceLinkStatus::Expired, None))
        }
        LinkState::Requested => {
            let (Some(request_id), Some(tag_b64)) = (st.request_id, st.link_tag) else {
                return Ok((DeviceLinkStatus::Tampered, None));
            };
            let Some(row) = crate::commands::ds_reads::enrollment_request(state, &request_id, true).await? else {
                return Ok((DeviceLinkStatus::Tampered, None));
            };
            let Some(eph_b64) = row.new_device_ephemeral_pub.as_deref() else {
                return Ok((DeviceLinkStatus::Tampered, None));
            };
            let ephemeral_pub = crate::commands::ds_reads::decode_b64("new_device_ephemeral_pub", eph_b64)?;
            let presented = base64::engine::general_purpose::STANDARD.decode(&tag_b64).unwrap_or_default();
            let mac_key = kdf(token.as_ref(), LINK_MAC_INFO);
            let expected = link_tag(&mac_key, link_id, &request_id, &row.new_device_id, &ephemeral_pub);
            let ok = row.user_id == user_id
                && row.status == "pending"
                && presented.len() == 32
                && constant_time_eq(&presented, &expected);
            if !ok {
                return Ok((DeviceLinkStatus::Tampered, None));
            }
            Ok((
                DeviceLinkStatus::ReadyToApprove {
                    device_name: st.device_name,
                    new_device_id: row.new_device_id.clone(),
                },
                Some((request_id, row.new_device_id, ephemeral_pub)),
            ))
        }
    }
}

/// Approve the phone: re-verify the tag, then wrap `account_id_key` to its
/// key. Refuses anything not `ReadyToApprove` at this moment.
pub async fn approve_device_link(state: &Arc<AppState>, user_id: String, link_id: String) -> Result<()> {
    let approver_device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("device_id not set — login incomplete")))?;
    let (status, facts) = verified_status(state, &user_id, &link_id).await?;
    let Some((request_id, new_device_id, ephemeral_pub)) = facts else {
        return Err(Error::Other(anyhow::anyhow!(match status {
            DeviceLinkStatus::Tampered => "This sign-in request was altered in transit and can't be approved.",
            DeviceLinkStatus::Expired => "This sign-in code has expired. Show a new one.",
            _ => "The phone hasn't finished signing in yet.",
        })));
    };
    crate::commands::device_enrollment::wrap_and_approve(
        state,
        &approver_device_id,
        &request_id,
        &user_id,
        &new_device_id,
        &ephemeral_pub,
        "qr_link",
    )
    .await?;
    state.device_link_tokens.lock().await.remove(&link_id);
    Ok(())
}

/// Forget a link on this device. The DS copy dies with its TTL; without the
/// token nothing can approve it anyway.
pub async fn cancel_device_link(state: &Arc<AppState>, link_id: String) -> Result<()> {
    state.device_link_tokens.lock().await.remove(&link_id);
    Ok(())
}

// ── Claimer side ─────────────────────────────────────────────────────────────

/// Scan → signed in with an enrollment-only session. Returns the profile with
/// `enrollment_required = true`; the caller then runs the ordinary
/// `start_device_enrollment`, which tags the request with this link.
pub async fn claim_device_link(
    state: &Arc<AppState>,
    payload: String,
    device_name: Option<String>,
) -> Result<crate::commands::auth::UserProfile> {
    let (link_id, token) = parse_payload(&payload)?;
    let claim = kdf(&token, LINK_CLAIM_INFO);
    let mac_key = kdf(&token, LINK_MAC_INFO);
    let candidate_device_id = Ulid::new().to_string();

    let resp = crate::commands::mls::ds_post_plain(
        state,
        &pollis_api::links::ClaimLinkBody {
            link_id: link_id.clone(),
            claim: b64(claim.as_ref()),
            device_id: candidate_device_id.clone(),
            device_name,
        },
    )
    .await?;
    let status = resp.status();
    if status.as_u16() == 401 {
        return Err(Error::Other(anyhow::anyhow!(
            "That code has expired or was already used. Show a new one on your other device."
        )));
    }
    if status.as_u16() == 429 {
        return Err(Error::Other(anyhow::anyhow!("Too many attempts. Wait a minute and try again.")));
    }
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("link claim failed ({status}): {txt}")));
    }
    let v = crate::commands::mls::decode_response::<pollis_api::links::ClaimLinkBody>(resp).await?;

    *state.device_link_pending.lock().await = Some(PendingDeviceLink { link_id, mac_key });
    crate::commands::auth::sign_in_with_link_session(state, v, candidate_device_id).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_round_trips_and_malformed_ones_are_refused() {
        let token = [5u8; 32];
        let p = format!(
            "{LINK_PAYLOAD_PREFIX}01HLINK:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token)
        );
        let (id, t) = parse_payload(&p).unwrap();
        assert_eq!(id, "01HLINK");
        assert_eq!(t.as_slice(), &token);
        for bad in [
            "pollis-link:v2:01HLINK:AAAA",
            "pollis-link:v1:01HLINK",
            "pollis-link:v1::BQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU",
            "pollis-link:v1:01HLINK:not-base64!!",
            "pollis-link:v1:01HLINK:BQUF",
            "https://example.com",
        ] {
            assert!(parse_payload(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn the_claim_and_mac_keys_are_domain_separated() {
        let t = [9u8; 32];
        assert_ne!(*kdf(&t, LINK_CLAIM_INFO), *kdf(&t, LINK_MAC_INFO));
    }

    /// The tag binds every field: change any one and it no longer verifies —
    /// in particular the ephemeral key, which is what a hostile server would
    /// swap.
    #[test]
    fn the_link_tag_binds_every_field() {
        let k = [3u8; 32];
        let base = link_tag(&k, "link", "req", "dev", &[1u8; 32]);
        assert_ne!(base, link_tag(&k, "linkX", "req", "dev", &[1u8; 32]));
        assert_ne!(base, link_tag(&k, "link", "reqX", "dev", &[1u8; 32]));
        assert_ne!(base, link_tag(&k, "link", "req", "devX", &[1u8; 32]));
        assert_ne!(base, link_tag(&k, "link", "req", "dev", &[2u8; 32]));
        assert_ne!(base, link_tag(&[4u8; 32], "link", "req", "dev", &[1u8; 32]));
        // Length-prefixing: moving a byte across a field boundary changes it.
        assert_ne!(link_tag(&k, "ab", "c", "dev", &[1u8; 32]), link_tag(&k, "a", "bc", "dev", &[1u8; 32]));
    }
}
