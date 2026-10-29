//! QR device link (#1207): sign a new phone in from an enrolled, PIN-verified device.
//! Protocol, threat model and guardrails: `docs/qr-device-link-design.md`.
//!
//! v1 approval is authenticated by a link tag, not a typed code: the DS never
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

/// Versioned so an old client refuses a future format instead of misparsing.
pub const LINK_PAYLOAD_PREFIX: &str = "pollis-link:v1:";

/// HKDF info for the claim secret the DS verifies (`SHA-256(claim)` stored).
const LINK_CLAIM_INFO: &[u8] = b"pollis-link-claim-v1";
/// HKDF info for the MAC key the DS never learns.
const LINK_MAC_INFO: &[u8] = b"pollis-link-mac-v1";

/// What the creator renders: the QR payload and when it stops working.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceLinkHandle {
    pub link_id: String,
    /// `pollis-link:v1:<link_id>:<base64url(t)>`
    pub qr_payload: String,
    /// Unix seconds, server-set.
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceLinkStatus {
    Open,
    Claimed { device_name: Option<String> },
    /// Key authenticated by the link tag; safe to show Approve.
    ReadyToApprove { device_name: Option<String>, new_device_id: String, request_id: String },
    /// The request's tag does NOT verify; never approvable.
    Tampered,
    /// Past its lifetime, already used, or unknown.
    Expired,
}

/// The new device's link state, read by `start_device_enrollment`.
pub struct PendingDeviceLink {
    pub link_id: String,
    pub kind: PendingLinkKind,
}

pub enum PendingLinkKind {
    /// v1: scanned a signed-in device's QR; tag with the MAC key from its secret.
    Claimed { mac_key: Zeroizing<[u8; 32]> },
    /// v2 (#1361): showed a QR; enroll with exactly the key it carried. No tag.
    Offered { ephemeral_secret: Zeroizing<[u8; 32]> },
}

/// v2 (#1361): shown by the NEW device; distinct so each side refuses the other's.
pub const OFFER_PAYLOAD_PREFIX: &str = "pollis-link:v2:";

/// What a new device renders while it waits to be signed in (#1361).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceOfferHandle {
    pub link_id: String,
    /// `pollis-link:v2:<link_id>:<base64url(ephemeral pub)>`. Carries no secret.
    pub qr_payload: String,
    /// Unix seconds, server-set. Show a fresh code after this.
    pub expires_at: i64,
}

/// The new device's offer in progress: the secrets the QR does NOT carry.
pub struct DeviceOfferInProgress {
    pub link_id: String,
    /// Never in the QR, so a photo of it cannot collect the session.
    pub poll_secret: Zeroizing<[u8; 32]>,
    /// Private half of the key the QR shows.
    pub ephemeral_secret: Zeroizing<[u8; 32]>,
    pub device_id: String,
    pub expires_at: i64,
}

/// What [`await_device_offer`] resolves to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceOfferWait {
    /// Holds an enrollment-only session; must now enroll.
    SignedIn { profile: crate::commands::auth::UserProfile },
    /// The code lapsed (or was cancelled); show a fresh one.
    Expired,
}

/// What the scanning device learns when it accepts a code (#1361).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptedDeviceOffer {
    pub link_id: String,
    pub new_device_id: String,
    pub device_name: Option<String>,
}

/// How [`finish_device_offer`] ended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceOfferOutcome {
    Linked { device_name: Option<String> },
    /// The request's key is NOT the one the QR showed — refused, nothing sent.
    Tampered,
    Expired,
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

/// Parse a v1 payload into `(link_id, token)`.
pub(crate) fn parse_payload(payload: &str) -> Result<(String, Zeroizing<Vec<u8>>)> {
    if payload.trim().starts_with(OFFER_PAYLOAD_PREFIX) {
        return Err(Error::Other(anyhow::anyhow!(
            "That code is shown by a device that wants to sign in. Scan it with Pollis on your signed-in phone instead."
        )));
    }
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

/// Parse a v2 payload into `(link_id, ephemeral_pub)`.
pub(crate) fn parse_offer_payload(payload: &str) -> Result<(String, [u8; 32])> {
    let payload = payload.trim();
    if payload.starts_with(LINK_PAYLOAD_PREFIX) {
        return Err(Error::Other(anyhow::anyhow!(
            "That code signs in a new device. Enter it on the device you're signing in instead."
        )));
    }
    let rest = payload
        .strip_prefix(OFFER_PAYLOAD_PREFIX)
        .ok_or_else(|| Error::Other(anyhow::anyhow!("That isn't a Pollis sign-in code.")))?;
    let (link_id, key_b64) = rest
        .split_once(':')
        .ok_or_else(|| Error::Other(anyhow::anyhow!("That sign-in code is incomplete.")))?;
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(key_b64)
        .map_err(|_| Error::Other(anyhow::anyhow!("That sign-in code is damaged.")))?;
    if link_id.is_empty() || link_id.len() > 64 || link_id.contains(':') {
        return Err(Error::Other(anyhow::anyhow!("That sign-in code is damaged.")));
    }
    let key: [u8; 32] = key
        .try_into()
        .map_err(|_| Error::Other(anyhow::anyhow!("That sign-in code is damaged.")))?;
    Ok((link_id.to_string(), key))
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Create a link and return the QR. Verifies the PIN here so no UI can skip it.
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

/// Wait until the link leaves the state the caller last saw (`since`, the
/// `state` tag of a [`DeviceLinkStatus`]), then return the new status.
pub async fn await_device_link(
    state: &Arc<AppState>,
    user_id: String,
    link_id: String,
    since: String,
) -> Result<DeviceLinkStatus> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(16 * 60);
    let mut delay = std::time::Duration::from_millis(500);
    loop {
        let status = verified_status(state, &user_id, &link_id).await?.0;
        if status_tag(&status) != since || matches!(status, DeviceLinkStatus::Expired) {
            return Ok(status);
        }
        if std::time::Instant::now() + delay >= deadline {
            return Ok(DeviceLinkStatus::Expired);
        }
        tokio::time::sleep(delay).await;
        delay = std::cmp::min(delay * 2, std::time::Duration::from_secs(4));
    }
}

fn status_tag(s: &DeviceLinkStatus) -> &'static str {
    match s {
        DeviceLinkStatus::Open => "open",
        DeviceLinkStatus::Claimed { .. } => "claimed",
        DeviceLinkStatus::ReadyToApprove { .. } => "ready_to_approve",
        DeviceLinkStatus::Tampered => "tampered",
        DeviceLinkStatus::Expired => "expired",
    }
}

/// How the approver authenticates the new device's enrollment key.
enum ApproverCheck {
    /// v1: this device created the link and holds its token.
    Tag(Zeroizing<Vec<u8>>),
    /// v2: this device scanned the new device's QR and holds the key it showed.
    ScannedKey([u8; 32]),
}

/// [`poll_device_link`] plus, when approvable, the request facts the wrap
/// needs — read and verified in one pass so approval wraps to exactly the key
/// whose tag was checked.
async fn verified_status(
    state: &Arc<AppState>,
    user_id: &str,
    link_id: &str,
) -> Result<(DeviceLinkStatus, Option<(String, String, Vec<u8>)>)> {
    let token = state.device_link_tokens.lock().await.get(link_id).cloned();
    let scanned_key = state.device_offer_accepts.lock().await.get(link_id).copied();
    let check = match (token, scanned_key) {
        (Some(t), _) => ApproverCheck::Tag(t),
        (None, Some(k)) => ApproverCheck::ScannedKey(k),
        (None, None) => return Ok((DeviceLinkStatus::Expired, None)),
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
            state.device_offer_accepts.lock().await.remove(link_id);
            Ok((DeviceLinkStatus::Expired, None))
        }
        LinkState::Requested => {
            let Some(request_id) = st.request_id else {
                return Ok((DeviceLinkStatus::Tampered, None));
            };
            let Some(row) = crate::commands::ds_reads::enrollment_request(state, &request_id, true).await? else {
                return Ok((DeviceLinkStatus::Tampered, None));
            };
            let Some(eph_b64) = row.new_device_ephemeral_pub.as_deref() else {
                return Ok((DeviceLinkStatus::Tampered, None));
            };
            let ephemeral_pub = crate::commands::ds_reads::decode_b64("new_device_ephemeral_pub", eph_b64)?;
            let key_ok = match &check {
                ApproverCheck::Tag(token) => {
                    let presented = st
                        .link_tag
                        .as_deref()
                        .map(|t| base64::engine::general_purpose::STANDARD.decode(t).unwrap_or_default())
                        .unwrap_or_default();
                    let mac_key = kdf(token.as_ref(), LINK_MAC_INFO);
                    let expected = link_tag(&mac_key, link_id, &request_id, &row.new_device_id, &ephemeral_pub);
                    presented.len() == 32 && constant_time_eq(&presented, &expected)
                }
                // Nothing the DS can compute stands in for the scanned key.
                ApproverCheck::ScannedKey(scanned) => constant_time_eq(&ephemeral_pub, scanned),
            };
            let ok = row.user_id == user_id && row.status == "pending" && key_ok;
            if !ok {
                return Ok((DeviceLinkStatus::Tampered, None));
            }
            Ok((
                DeviceLinkStatus::ReadyToApprove {
                    device_name: st.device_name,
                    new_device_id: row.new_device_id.clone(),
                    request_id: request_id.clone(),
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
    let via = if state.device_offer_accepts.lock().await.contains_key(&link_id) {
        "qr_scan"
    } else {
        "qr_link"
    };
    crate::commands::device_enrollment::wrap_and_approve(
        state,
        &approver_device_id,
        &request_id,
        &user_id,
        &new_device_id,
        &ephemeral_pub,
        via,
    )
    .await?;
    state.device_link_tokens.lock().await.remove(&link_id);
    state.device_offer_accepts.lock().await.remove(&link_id);
    Ok(())
}

/// Forget a link on this device. The DS copy dies with its TTL; without the
/// token (or the scanned key) nothing can approve it anyway.
pub async fn cancel_device_link(state: &Arc<AppState>, link_id: String) -> Result<()> {
    state.device_link_tokens.lock().await.remove(&link_id);
    state.device_offer_accepts.lock().await.remove(&link_id);
    Ok(())
}

/// New device: offer a link and return the QR to show. Replaces any earlier
/// offer of this device (a refreshed QR).
pub async fn create_device_offer(state: &Arc<AppState>, device_name: Option<String>) -> Result<DeviceOfferHandle> {
    let mut poll_secret = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(poll_secret.as_mut());
    let mut ephemeral_secret = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(ephemeral_secret.as_mut());
    let ephemeral_pub = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*ephemeral_secret));
    let verifier: [u8; 32] = Sha256::digest(poll_secret.as_ref()).into();
    let link_id = Ulid::new().to_string();
    let device_id = Ulid::new().to_string();

    let resp = crate::commands::mls::ds_post_plain(
        state,
        &pollis_api::links::OfferLinkBody {
            link_id: link_id.clone(),
            poll_verifier: b64(&verifier),
            device_id: device_id.clone(),
            device_name,
        },
    )
    .await?;
    let status = resp.status();
    if status.as_u16() == 429 {
        return Err(Error::Other(anyhow::anyhow!("Too many sign-in codes. Wait a minute and try again.")));
    }
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("link offer failed ({status}): {txt}")));
    }
    let v = crate::commands::mls::decode_response::<pollis_api::links::OfferLinkBody>(resp).await?;

    let qr_payload = format!(
        "{OFFER_PAYLOAD_PREFIX}{link_id}:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ephemeral_pub.as_bytes())
    );
    *state.device_offer.lock().await = Some(DeviceOfferInProgress {
        link_id: link_id.clone(),
        poll_secret,
        ephemeral_secret,
        device_id,
        expires_at: v.expires_at,
    });
    Ok(DeviceOfferHandle { link_id, qr_payload, expires_at: v.expires_at })
}

/// New device: wait until a signed-in device accepts the code, then sign in.
pub async fn await_device_offer(state: &Arc<AppState>, link_id: String) -> Result<DeviceOfferWait> {
    let mut delay = std::time::Duration::from_millis(500);
    loop {
        let (poll_secret, expires_at) = match state.device_offer.lock().await.as_ref() {
            Some(o) if o.link_id == link_id => (b64(o.poll_secret.as_ref()), o.expires_at),
            // Cancelled, or replaced by a fresher code.
            _ => return Ok(DeviceOfferWait::Expired),
        };
        let resp = crate::commands::mls::ds_post_plain(
            state,
            &pollis_api::links::PollOfferBody { link_id: link_id.clone(), poll_secret },
        )
        .await?;
        let status = resp.status();
        if status.as_u16() == 401 {
            return Ok(DeviceOfferWait::Expired);
        }
        if !status.is_success() {
            let txt = resp.text().await.unwrap_or_default();
            return Err(Error::Other(anyhow::anyhow!("link poll failed ({status}): {txt}")));
        }
        let v = crate::commands::mls::decode_response::<pollis_api::links::PollOfferBody>(resp).await?;
        if let Some(session) = v.session {
            let Some(offer) = state.device_offer.lock().await.take() else {
                return Ok(DeviceOfferWait::Expired);
            };
            let device_id = offer.device_id.clone();
            *state.device_link_pending.lock().await = Some(PendingDeviceLink {
                link_id: offer.link_id.clone(),
                kind: PendingLinkKind::Offered { ephemeral_secret: offer.ephemeral_secret },
            });
            let profile = crate::commands::auth::sign_in_with_link_session(state, session, device_id).await?;
            return Ok(DeviceOfferWait::SignedIn { profile });
        }
        // A little past the server's expiry, so the lapse is reported by the
        // same 401 every other failure gets.
        let now = chrono::Utc::now().timestamp();
        if now > expires_at + 5 {
            return Ok(DeviceOfferWait::Expired);
        }
        tokio::time::sleep(delay).await;
        delay = std::cmp::min(delay * 2, std::time::Duration::from_secs(3));
    }
}

/// New device: stop offering. The DS copy dies with its TTL; without the poll
/// secret nobody can collect it.
pub async fn cancel_device_offer(state: &Arc<AppState>) -> Result<()> {
    *state.device_offer.lock().await = None;
    Ok(())
}

/// Signed-in device: accept a scanned v2 code. Verifies the PIN first, since
/// accepting hands this account's sign-in to the device showing the code.
pub async fn accept_device_offer(
    state: &Arc<AppState>,
    user_id: String,
    pin: String,
    payload: String,
) -> Result<AcceptedDeviceOffer> {
    let (link_id, scanned_key) = parse_offer_payload(&payload)?;
    crate::commands::pin::verify_pin(state, &user_id, &pin).await?;

    let resp = crate::commands::mls::ds_post(
        state,
        &pollis_api::links::AcceptOfferBody { link_id: link_id.clone(), user_id: Some(user_id.clone()) },
    )
    .await?;
    let status = resp.status();
    if status.as_u16() == 401 {
        return Err(Error::Other(anyhow::anyhow!(
            "That code has expired or was already used. Ask the other device to show a new one."
        )));
    }
    if !status.is_success() {
        let txt = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!("link accept failed ({status}): {txt}")));
    }
    let v = crate::commands::mls::decode_response::<pollis_api::links::AcceptOfferBody>(resp).await?;
    state.device_offer_accepts.lock().await.insert(link_id.clone(), scanned_key);
    Ok(AcceptedDeviceOffer { link_id, new_device_id: v.new_device_id, device_name: v.device_name })
}

/// After [`accept_device_offer`], approve the new device's request if its key is
/// the scanned one. No second Approve: the user already scanned and gave the PIN.
pub async fn finish_device_offer(
    state: &Arc<AppState>,
    user_id: String,
    link_id: String,
) -> Result<DeviceOfferOutcome> {
    let mut since = "claimed".to_string();
    loop {
        let status = await_device_link(state, user_id.clone(), link_id.clone(), since.clone()).await?;
        match status {
            DeviceLinkStatus::ReadyToApprove { device_name, .. } => {
                approve_device_link(state, user_id, link_id).await?;
                return Ok(DeviceOfferOutcome::Linked { device_name });
            }
            DeviceLinkStatus::Tampered => {
                state.device_offer_accepts.lock().await.remove(&link_id);
                return Ok(DeviceOfferOutcome::Tampered);
            }
            DeviceLinkStatus::Expired => {
                state.device_offer_accepts.lock().await.remove(&link_id);
                return Ok(DeviceOfferOutcome::Expired);
            }
            other => since = status_tag(&other).to_string(),
        }
    }
}

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

    *state.device_link_pending.lock().await =
        Some(PendingDeviceLink { link_id, kind: PendingLinkKind::Claimed { mac_key } });
    // Signed in by the other direction: stop offering this device's own code.
    *state.device_offer.lock().await = None;
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
    fn an_offer_payload_round_trips_and_the_two_directions_refuse_each_other() {
        let key = [7u8; 32];
        let p = format!(
            "{OFFER_PAYLOAD_PREFIX}01HOFFER:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)
        );
        let (id, k) = parse_offer_payload(&p).unwrap();
        assert_eq!(id, "01HOFFER");
        assert_eq!(k, key);
        for bad in [
            "pollis-link:v2:01HOFFER",
            "pollis-link:v2::BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc",
            "pollis-link:v2:01HOFFER:BwcH",
            "pollis-link:v2:01HOFFER:not-base64!!",
            "https://example.com",
        ] {
            assert!(parse_offer_payload(bad).is_err(), "{bad} must be refused");
        }
        let v1 = format!(
            "{LINK_PAYLOAD_PREFIX}01HLINK:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([5u8; 32])
        );
        let e = parse_offer_payload(&v1).unwrap_err().to_string();
        assert!(e.contains("device you're signing in"), "{e}");
        let e = parse_payload(&p).unwrap_err().to_string();
        assert!(e.contains("signed-in phone"), "{e}");
    }

    #[test]
    fn the_claim_and_mac_keys_are_domain_separated() {
        let t = [9u8; 32];
        assert_ne!(*kdf(&t, LINK_CLAIM_INFO), *kdf(&t, LINK_MAC_INFO));
    }

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
