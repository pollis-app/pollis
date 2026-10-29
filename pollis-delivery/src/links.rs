//! QR device links: a device is signed in by a QR instead of the email OTP.
//! Protocol, threat model and guardrails: `docs/qr-device-link-design.md`.
//!
//! Two directions, one store:
//!
//! - **v1 (#1207):** the signed-in device shows a QR; the new device `claim`s it with its secret.
//! - **v2 (#1361):** the new device shows its key; a signed-in one accepts; it polls for its session.
//!
//! The DS's part is deliberately narrow:
//!
//! - It stores only hashes, so it can neither forge a link tag nor substitute the wrapped-to key.
//! - Claiming mints an enrollment-only session ([`SessionScope::DeviceLink`] / [`SessionScope::DeviceOffer`]).
//! - Lifetimes, single claim, single accept and single session delivery are
//!   enforced HERE, server-side, not by the clients that display the QR.
//!
//! **Store:** in-memory; a restart drops open links.
//!
//! [`SessionScope::DeviceLink`]: crate::session::SessionScope::DeviceLink
//! [`SessionScope::DeviceOffer`]: crate::session::SessionScope::DeviceOffer

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sha2::{Digest, Sha256};

use crate::writes::{bad_request, gate_and_parse, ok_response, resolve_actor, RawRequest};
use crate::error::{AppError, AuthRejection};
use crate::AppState;

pub use pollis_api::links::*;

/// How long a fresh v1 QR stays claimable (server-set).
pub const LINK_TTL_SECS: u64 = 60;

/// How long a v2 offer stays acceptable; longer, as the approver must first find their phone.
pub const OFFER_TTL_SECS: u64 = 120;

/// How long a CLAIMED link (or accepted offer) stays readable by its approver.
pub const CLAIMED_LINK_TTL_SECS: u64 = 15 * 60;

/// Open (unclaimed) links one account may hold at once.
pub const MAX_OPEN_LINKS_PER_ACCOUNT: usize = 3;

/// Unaccepted v2 offers the whole store may hold; pre-credential, so not per account.
pub const MAX_OPEN_OFFERS: usize = 10_000;

/// Sweep threshold for the map, mirroring the OTP store's amortised collector.
const SWEEP_AT: usize = 1024;

/// Which device showed the QR. Fixed for the record's life: it decides whether
/// the new device's enrollment request must carry a link tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// v1: the signed-in device showed it; the request carries a tag.
    Shown,
    /// v2: the new device showed it; the request carries no tag (the approver
    /// compares the key it read off the QR).
    Offered,
}

#[derive(Clone, Debug)]
enum Phase {
    /// v1: created by `user_id`'s device; waiting for a claim.
    Open { user_id: String, claim_verifier: [u8; 32] },
    /// v2: offered by a device that is not signed in; no account yet.
    Offered { poll_verifier: [u8; 32], device_id: String, device_name: Option<String> },
    /// v2: a device of `user_id` accepted it; the new device has not collected
    /// its session yet.
    Accepted { user_id: String, poll_verifier: [u8; 32], device_id: String, device_name: Option<String> },
    /// Both: `device_id` holds an enrollment-only session for `user_id`.
    Claimed { user_id: String, device_id: String, device_name: Option<String> },
    /// Both: that device filed its enrollment request.
    Requested {
        user_id: String,
        device_id: String,
        device_name: Option<String>,
        request_id: String,
        link_tag: Option<String>,
    },
}

impl Phase {
    /// The account the link belongs to; `None` before a v2 offer is accepted.
    fn user_id(&self) -> Option<&str> {
        match self {
            Phase::Open { user_id, .. }
            | Phase::Accepted { user_id, .. }
            | Phase::Claimed { user_id, .. }
            | Phase::Requested { user_id, .. } => Some(user_id),
            Phase::Offered { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
struct LinkRecord {
    kind: Kind,
    expires_at: u64,
    phase: Phase,
}

/// The outcome of a claim attempt. Every failure is the same 401 on the wire —
/// unknown, wrong secret, expired, already claimed — so a claimer learns
/// nothing about which links exist or why theirs failed.
#[derive(Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed { user_id: String },
    Refused,
}

/// The outcome of accepting a v2 offer.
#[derive(Debug, PartialEq, Eq)]
pub enum AcceptOutcome {
    Accepted { device_id: String, device_name: Option<String> },
    /// Unknown, expired, a v1 link, or already accepted (by anyone).
    Refused,
}

/// The outcome of a v2 poll. `Refused` covers unknown, expired, wrong secret
/// and already collected alike.
#[derive(Debug, PartialEq, Eq)]
pub enum CollectOutcome {
    /// Nobody has accepted it yet.
    Waiting,
    /// Accepted: mint the session now. Returned exactly once.
    Collected { user_id: String, device_id: String },
    Refused,
}

/// In-memory link store; `Clone` shares it.
#[derive(Clone, Default)]
pub struct LinkStore {
    inner: Arc<Mutex<HashMap<String, LinkRecord>>>,
}

fn sweep(map: &mut HashMap<String, LinkRecord>, now: u64) {
    map.retain(|_, rec| now <= rec.expires_at);
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

fn id_is_live(map: &HashMap<String, LinkRecord>, link_id: &str, now: u64) -> bool {
    map.get(link_id).is_some_and(|r| now <= r.expires_at)
}

impl LinkStore {
    /// Register an open v1 link for `user_id`. `false` when the id is taken or
    /// the account is at [`MAX_OPEN_LINKS_PER_ACCOUNT`].
    pub fn create(&self, link_id: &str, user_id: &str, claim_verifier: [u8; 32], now: u64) -> bool {
        let mut guard = self.inner.lock().expect("link store mutex poisoned");
        if guard.len() >= SWEEP_AT {
            sweep(&mut guard, now);
        }
        if id_is_live(&guard, link_id, now) {
            return false;
        }
        let open_for_account = guard
            .values()
            .filter(|r| now <= r.expires_at)
            .filter(|r| matches!(&r.phase, Phase::Open { user_id: u, .. } if u == user_id))
            .count();
        if open_for_account >= MAX_OPEN_LINKS_PER_ACCOUNT {
            return false;
        }
        guard.insert(
            link_id.to_string(),
            LinkRecord {
                kind: Kind::Shown,
                expires_at: now.saturating_add(LINK_TTL_SECS),
                phase: Phase::Open { user_id: user_id.to_string(), claim_verifier },
            },
        );
        true
    }

    /// Claim a v1 link with the secret the QR carried. Atomic check-and-flip
    /// under the mutex: of two concurrent claims with the right secret, exactly
    /// one gets `Claimed`.
    pub fn claim(
        &self,
        link_id: &str,
        claim: &[u8],
        device_id: &str,
        device_name: Option<String>,
        now: u64,
    ) -> ClaimOutcome {
        let mut guard = self.inner.lock().expect("link store mutex poisoned");
        let Some(rec) = guard.get_mut(link_id) else {
            return ClaimOutcome::Refused;
        };
        let Phase::Open { user_id, claim_verifier } = &rec.phase else {
            return ClaimOutcome::Refused;
        };
        let presented: [u8; 32] = Sha256::digest(claim).into();
        // Compare first, unconditionally, so timing does not separate "wrong
        // secret" from "right secret on a dead link".
        let secret_ok = constant_time_eq(&presented, claim_verifier);
        if !secret_ok || now > rec.expires_at {
            return ClaimOutcome::Refused;
        }
        let user_id = user_id.clone();
        rec.phase = Phase::Claimed { user_id: user_id.clone(), device_id: device_id.to_string(), device_name };
        rec.expires_at = now.saturating_add(CLAIMED_LINK_TTL_SECS);
        ClaimOutcome::Claimed { user_id }
    }

    /// Register a v2 offer from a device that is not signed in (#1361). `false`
    /// when the id is taken or the store holds [`MAX_OPEN_OFFERS`].
    pub fn offer(
        &self,
        link_id: &str,
        poll_verifier: [u8; 32],
        device_id: &str,
        device_name: Option<String>,
        now: u64,
    ) -> bool {
        let mut guard = self.inner.lock().expect("link store mutex poisoned");
        if guard.len() >= SWEEP_AT {
            sweep(&mut guard, now);
        }
        if id_is_live(&guard, link_id, now) {
            return false;
        }
        let open_offers = guard
            .values()
            .filter(|r| now <= r.expires_at && matches!(r.phase, Phase::Offered { .. }))
            .count();
        if open_offers >= MAX_OPEN_OFFERS {
            return false;
        }
        guard.insert(
            link_id.to_string(),
            LinkRecord {
                kind: Kind::Offered,
                expires_at: now.saturating_add(OFFER_TTL_SECS),
                phase: Phase::Offered { poll_verifier, device_id: device_id.to_string(), device_name },
            },
        );
        true
    }

    /// Bind a v2 offer to `user_id`, the account of the signed-in device that
    /// scanned it. Once: a second accept — by the same account or another —
    /// is refused, so an offer can never be moved between accounts.
    pub fn accept(&self, link_id: &str, user_id: &str, now: u64) -> AcceptOutcome {
        let mut guard = self.inner.lock().expect("link store mutex poisoned");
        let Some(rec) = guard.get_mut(link_id) else {
            return AcceptOutcome::Refused;
        };
        if now > rec.expires_at {
            return AcceptOutcome::Refused;
        }
        let Phase::Offered { poll_verifier, device_id, device_name } = &rec.phase else {
            return AcceptOutcome::Refused;
        };
        let outcome = AcceptOutcome::Accepted { device_id: device_id.clone(), device_name: device_name.clone() };
        rec.phase = Phase::Accepted {
            user_id: user_id.to_string(),
            poll_verifier: *poll_verifier,
            device_id: device_id.clone(),
            device_name: device_name.clone(),
        };
        rec.expires_at = now.saturating_add(CLAIMED_LINK_TTL_SECS);
        outcome
    }

    /// The new device's poll on its v2 offer, by the secret only it holds.
    /// Hands out the session exactly once: collecting flips the link to
    /// `Claimed`, after which the same secret is refused.
    pub fn collect(&self, link_id: &str, poll_secret: &[u8], now: u64) -> CollectOutcome {
        let mut guard = self.inner.lock().expect("link store mutex poisoned");
        let Some(rec) = guard.get_mut(link_id) else {
            return CollectOutcome::Refused;
        };
        let verifier = match &rec.phase {
            Phase::Offered { poll_verifier, .. } | Phase::Accepted { poll_verifier, .. } => *poll_verifier,
            _ => return CollectOutcome::Refused,
        };
        let presented: [u8; 32] = Sha256::digest(poll_secret).into();
        // Compare first, unconditionally, as `claim` does.
        let secret_ok = constant_time_eq(&presented, &verifier);
        if !secret_ok || now > rec.expires_at {
            return CollectOutcome::Refused;
        }
        let Phase::Accepted { user_id, device_id, device_name, .. } = &rec.phase else {
            return CollectOutcome::Waiting;
        };
        let (user_id, device_id) = (user_id.clone(), device_id.clone());
        rec.phase = Phase::Claimed {
            user_id: user_id.clone(),
            device_id: device_id.clone(),
            device_name: device_name.clone(),
        };
        CollectOutcome::Collected { user_id, device_id }
    }

    /// Record the enrollment request the linked device filed. Only the device
    /// the link names may do this, once, and the tag must match the link's
    /// direction: required on a v1 link, absent on a v2 offer.
    pub fn record_request(
        &self,
        link_id: &str,
        device_id: &str,
        request_id: &str,
        link_tag: Option<&str>,
        now: u64,
    ) -> bool {
        let mut guard = self.inner.lock().expect("link store mutex poisoned");
        let Some(rec) = guard.get_mut(link_id) else {
            return false;
        };
        if now > rec.expires_at {
            return false;
        }
        let tag_matches_kind = match rec.kind {
            Kind::Shown => link_tag.is_some_and(|t| !t.is_empty()),
            Kind::Offered => link_tag.is_none(),
        };
        if !tag_matches_kind {
            return false;
        }
        let Phase::Claimed { user_id, device_id: claimed_by, device_name } = &rec.phase else {
            return false;
        };
        if claimed_by != device_id {
            return false;
        }
        rec.phase = Phase::Requested {
            user_id: user_id.clone(),
            device_id: claimed_by.clone(),
            device_name: device_name.clone(),
            request_id: request_id.to_string(),
            link_tag: link_tag.map(str::to_string),
        };
        true
    }

    /// Enrollment requests of `user_id` bound to a live link; the typed-code list leaves these out.
    pub fn bound_request_ids(&self, user_id: &str, now: u64) -> std::collections::HashSet<String> {
        let guard = self.inner.lock().expect("link store mutex poisoned");
        guard
            .values()
            .filter(|r| now <= r.expires_at)
            .filter_map(|r| match &r.phase {
                Phase::Requested { user_id: u, request_id, .. } if u == user_id => Some(request_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// The approver's view (the v1 creator, or the device that accepted a v2
    /// offer). Answers `Expired` when the link is not `user_id`'s or does not
    /// exist, so status is not an existence oracle across accounts — and an
    /// unaccepted offer belongs to nobody.
    pub fn status(&self, link_id: &str, user_id: &str, now: u64) -> LinkStatusResponse {
        let guard = self.inner.lock().expect("link store mutex poisoned");
        let expired = LinkStatusResponse {
            state: LinkState::Expired,
            new_device_id: None,
            device_name: None,
            request_id: None,
            link_tag: None,
        };
        let Some(rec) = guard.get(link_id) else {
            return expired;
        };
        if rec.phase.user_id() != Some(user_id) || now > rec.expires_at {
            return expired;
        }
        match &rec.phase {
            Phase::Offered { .. } => expired,
            Phase::Open { .. } => LinkStatusResponse { state: LinkState::Open, ..expired },
            Phase::Accepted { device_id, device_name, .. } | Phase::Claimed { device_id, device_name, .. } => {
                LinkStatusResponse {
                    state: LinkState::Claimed,
                    new_device_id: Some(device_id.clone()),
                    device_name: device_name.clone(),
                    ..expired
                }
            }
            Phase::Requested { device_id, device_name, request_id, link_tag, .. } => LinkStatusResponse {
                state: LinkState::Requested,
                new_device_id: Some(device_id.clone()),
                device_name: device_name.clone(),
                request_id: Some(request_id.clone()),
                link_tag: link_tag.clone(),
            },
        }
    }
}

/// A device name the new device chose: capped, IP-shaped text redacted, blank
/// dropped — before it is held or shown to the approving device.
fn clean_device_name(name: Option<String>) -> Option<String> {
    name.map(|n| crate::util::redact_ip_literals(&n.chars().take(64).collect::<String>()))
        .filter(|n| !n.trim().is_empty())
}

fn decode_32(b64: &str) -> Option<[u8; 32]> {
    match crate::util::b64_decode(b64) {
        Ok(v) if v.len() == 32 => v.try_into().ok(),
        _ => None,
    }
}

pub async fn create_link(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<CreateLinkBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let actor = match resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(AuthRejection::Forbidden.into_response()),
    };
    let Some(verifier) = decode_32(&parsed.claim_verifier) else {
        return Ok(bad_request("claim_verifier must be 32 bytes"));
    };
    if parsed.link_id.is_empty() || parsed.link_id.len() > 64 {
        return Ok(bad_request("invalid link_id"));
    }
    let now = crate::util::now_unix();
    if !state.links.create(&parsed.link_id, &actor, verifier, now) {
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "too many open links" })),
        )
            .into_response());
    }
    Ok(ok_response::<CreateLinkBody>(CreateLinkResponse {
        expires_at: now.saturating_add(LINK_TTL_SECS) as i64,
    }))
}

pub async fn claim_link(State(state): State<AppState>, _headers: HeaderMap, body: Bytes) -> Response {
    let parsed: ClaimLinkBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let claim = match crate::util::b64_decode(&parsed.claim) {
        Ok(c) if c.len() == 32 => c,
        _ => return refused(),
    };
    if parsed.device_id.trim().is_empty() || parsed.device_id.len() > 64 {
        return bad_request("invalid device_id");
    }
    let device_name = clean_device_name(parsed.device_name);
    let now = crate::util::now_unix();
    let user_id = match state.links.claim(&parsed.link_id, &claim, &parsed.device_id, device_name, now) {
        ClaimOutcome::Claimed { user_id } => user_id,
        ClaimOutcome::Refused => return refused(),
    };

    let conn = match state.db.conn().await {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    let account = match account_for(&conn, &user_id).await {
        Ok(Some(a)) => a,
        Ok(None) => return refused(),
        Err(e) => return internal(e),
    };
    let ttl = state.otp_config.session_ttl_secs;
    let session_token =
        state
            .sessions
            .mint_device_link(&user_id, &account.email, &parsed.device_id, &parsed.link_id, ttl, now);
    ok_response::<ClaimLinkBody>(ClaimLinkResponse {
        user_id,
        username: account.username,
        email: account.email,
        account_id_pub: account.account_id_pub,
        session_token,
        session_expires_at: now.saturating_add(ttl) as i64,
    })
}

pub async fn offer_link(State(state): State<AppState>, _headers: HeaderMap, body: Bytes) -> Response {
    let parsed: OfferLinkBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let Some(verifier) = decode_32(&parsed.poll_verifier) else {
        return bad_request("poll_verifier must be 32 bytes");
    };
    if parsed.link_id.is_empty() || parsed.link_id.len() > 64 {
        return bad_request("invalid link_id");
    }
    if parsed.device_id.trim().is_empty() || parsed.device_id.len() > 64 {
        return bad_request("invalid device_id");
    }
    let device_name = clean_device_name(parsed.device_name);
    let now = crate::util::now_unix();
    if !state.links.offer(&parsed.link_id, verifier, &parsed.device_id, device_name, now) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "cannot hold another sign-in code right now" })),
        )
            .into_response();
    }
    ok_response::<OfferLinkBody>(OfferLinkResponse { expires_at: now.saturating_add(OFFER_TTL_SECS) as i64 })
}

pub async fn accept_offer(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<AcceptOfferBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let actor = match resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(AuthRejection::Forbidden.into_response()),
    };
    match state.links.accept(&parsed.link_id, &actor, crate::util::now_unix()) {
        AcceptOutcome::Accepted { device_id, device_name } => Ok(ok_response::<AcceptOfferBody>(
            AcceptOfferResponse { new_device_id: device_id, device_name },
        )),
        AcceptOutcome::Refused => Ok(refused()),
    }
}

pub async fn poll_offer(State(state): State<AppState>, _headers: HeaderMap, body: Bytes) -> Response {
    let parsed: PollOfferBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let secret = match crate::util::b64_decode(&parsed.poll_secret) {
        Ok(s) if s.len() == 32 => s,
        _ => return refused(),
    };
    let now = crate::util::now_unix();
    let (user_id, device_id) = match state.links.collect(&parsed.link_id, &secret, now) {
        CollectOutcome::Waiting => return ok_response::<PollOfferBody>(PollOfferResponse { session: None }),
        CollectOutcome::Collected { user_id, device_id } => (user_id, device_id),
        CollectOutcome::Refused => return refused(),
    };

    let conn = match state.db.conn().await {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    let account = match account_for(&conn, &user_id).await {
        Ok(Some(a)) => a,
        Ok(None) => return refused(),
        Err(e) => return internal(e),
    };
    let ttl = state.otp_config.session_ttl_secs;
    let session_token =
        state
            .sessions
            .mint_device_offer(&user_id, &account.email, &device_id, &parsed.link_id, ttl, now);
    ok_response::<PollOfferBody>(PollOfferResponse {
        session: Some(ClaimLinkResponse {
            user_id,
            username: account.username,
            email: account.email,
            account_id_pub: account.account_id_pub,
            session_token,
            session_expires_at: now.saturating_add(ttl) as i64,
        }),
    })
}

struct Account {
    username: String,
    email: String,
    account_id_pub: Option<String>,
}

async fn account_for(conn: &libsql::Connection, user_id: &str) -> anyhow::Result<Option<Account>> {
    let mut rows = conn
        .query(
            "SELECT username, email, account_id_pub FROM users WHERE id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let pub_bytes: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(2).ok().flatten();
    Ok(Some(Account {
        username: row.get(0)?,
        email: row.get(1)?,
        account_id_pub: pub_bytes.as_deref().map(crate::util::b64),
    }))
}

pub async fn link_status(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<LinkStatusBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let actor = match resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(AuthRejection::Forbidden.into_response()),
    };
    let status = state.links.status(&parsed.link_id, &actor, crate::util::now_unix());
    Ok(ok_response::<LinkStatusBody>(status))
}

fn refused() -> Response {
    (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "invalid or expired link" }))).into_response()
}

fn internal(e: impl std::fmt::Display) -> Response {
    tracing::error!("link: internal error: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "internal error" }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verifier(claim: &[u8]) -> [u8; 32] {
        Sha256::digest(claim).into()
    }

    #[test]
    fn a_link_claims_once_with_the_right_secret() {
        let store = LinkStore::default();
        assert!(store.create("l1", "alice", verifier(b"secret-claim-32-bytes-long-xxxxx"), 1000));
        assert_eq!(
            store.claim("l1", b"wrong-claim-32-bytes-long-xxxxxx", "phone", None, 1001),
            ClaimOutcome::Refused
        );
        assert_eq!(
            store.claim("l1", b"secret-claim-32-bytes-long-xxxxx", "phone", None, 1002),
            ClaimOutcome::Claimed { user_id: "alice".into() }
        );
        // Single use: the same secret, again, is refused.
        assert_eq!(
            store.claim("l1", b"secret-claim-32-bytes-long-xxxxx", "attacker", None, 1003),
            ClaimOutcome::Refused
        );
    }

    #[test]
    fn an_unclaimed_link_dies_after_its_lifetime() {
        let store = LinkStore::default();
        store.create("l1", "alice", verifier(b"c"), 1000);
        assert_eq!(store.claim("l1", b"c", "phone", None, 1000 + LINK_TTL_SECS + 1), ClaimOutcome::Refused);
        assert_eq!(store.status("l1", "alice", 1000 + LINK_TTL_SECS + 1).state, LinkState::Expired);
    }

    #[test]
    fn status_is_only_visible_to_the_creating_account() {
        let store = LinkStore::default();
        store.create("l1", "alice", verifier(b"c"), 1000);
        assert_eq!(store.status("l1", "alice", 1001).state, LinkState::Open);
        assert_eq!(store.status("l1", "mallory", 1001).state, LinkState::Expired);
    }

    #[test]
    fn only_the_claiming_device_can_attach_an_enrollment_request() {
        let store = LinkStore::default();
        store.create("l1", "alice", verifier(b"c"), 1000);
        // Not claimed yet → nothing to attach to.
        assert!(!store.record_request("l1", "phone", "req1", Some("tag"), 1001));
        store.claim("l1", b"c", "phone", Some("Pixel".into()), 1002);
        assert!(!store.record_request("l1", "other-device", "req1", Some("tag"), 1003));
        // A v1 link's request must carry its tag.
        assert!(!store.record_request("l1", "phone", "req1", None, 1003));
        assert!(store.record_request("l1", "phone", "req1", Some("tag"), 1003));
        // Once.
        assert!(!store.record_request("l1", "phone", "req2", Some("tag2"), 1004));
        assert!(store.bound_request_ids("alice", 1005).contains("req1"));
        assert!(store.bound_request_ids("mallory", 1005).is_empty());
        let st = store.status("l1", "alice", 1005);
        assert_eq!(st.state, LinkState::Requested);
        assert_eq!(st.request_id.as_deref(), Some("req1"));
        assert_eq!(st.device_name.as_deref(), Some("Pixel"));
    }

    #[test]
    fn an_account_cannot_hold_unbounded_open_links() {
        let store = LinkStore::default();
        for i in 0..MAX_OPEN_LINKS_PER_ACCOUNT {
            assert!(store.create(&format!("l{i}"), "alice", verifier(b"c"), 1000));
        }
        assert!(!store.create("one-more", "alice", verifier(b"c"), 1000));
        // Another account is unaffected, and the cap frees as links expire.
        assert!(store.create("bob-1", "bob", verifier(b"c"), 1000));
        assert!(store.create("later", "alice", verifier(b"c"), 1000 + LINK_TTL_SECS + 1));
    }

    #[test]
    fn a_live_link_id_cannot_be_overwritten() {
        let store = LinkStore::default();
        assert!(store.create("l1", "alice", verifier(b"c"), 1000));
        assert!(!store.create("l1", "mallory", verifier(b"m"), 1001));
        assert_eq!(store.claim("l1", b"m", "phone", None, 1002), ClaimOutcome::Refused);
        // Nor by an offer.
        assert!(!store.offer("l1", verifier(b"p"), "desk", None, 1002));
    }

    #[test]
    fn an_offer_signs_in_one_device_for_the_accepting_account_once() {
        let store = LinkStore::default();
        assert!(store.offer("o1", verifier(b"poll"), "desk", Some("Mac".into()), 1000));
        // Nobody has accepted it: the new device waits, and it belongs to no
        // account, so no status reveals it.
        assert_eq!(store.collect("o1", b"poll", 1001), CollectOutcome::Waiting);
        assert_eq!(store.status("o1", "alice", 1001).state, LinkState::Expired);

        assert_eq!(
            store.accept("o1", "alice", 1002),
            AcceptOutcome::Accepted { device_id: "desk".into(), device_name: Some("Mac".into()) }
        );
        // Bound once: neither another account nor the same one can re-accept.
        assert_eq!(store.accept("o1", "mallory", 1003), AcceptOutcome::Refused);
        assert_eq!(store.accept("o1", "alice", 1003), AcceptOutcome::Refused);
        assert_eq!(store.status("o1", "alice", 1003).state, LinkState::Claimed);
        assert_eq!(store.status("o1", "mallory", 1003).state, LinkState::Expired);

        // A photo of the QR holds no poll secret.
        assert_eq!(store.collect("o1", b"guess", 1004), CollectOutcome::Refused);
        assert_eq!(
            store.collect("o1", b"poll", 1004),
            CollectOutcome::Collected { user_id: "alice".into(), device_id: "desk".into() }
        );
        // Delivered once.
        assert_eq!(store.collect("o1", b"poll", 1005), CollectOutcome::Refused);
    }

    #[test]
    fn an_unaccepted_offer_dies_after_its_lifetime() {
        let store = LinkStore::default();
        store.offer("o1", verifier(b"poll"), "desk", None, 1000);
        let late = 1000 + OFFER_TTL_SECS + 1;
        assert_eq!(store.accept("o1", "alice", late), AcceptOutcome::Refused);
        assert_eq!(store.collect("o1", b"poll", late), CollectOutcome::Refused);
        // And its id is free again.
        assert!(store.offer("o1", verifier(b"poll2"), "desk", None, late));
    }

    #[test]
    fn a_v1_link_cannot_be_accepted_and_an_offer_cannot_be_claimed() {
        let store = LinkStore::default();
        store.create("l1", "alice", verifier(b"c"), 1000);
        assert_eq!(store.accept("l1", "mallory", 1001), AcceptOutcome::Refused);
        assert_eq!(store.collect("l1", b"c", 1001), CollectOutcome::Refused);
        store.offer("o1", verifier(b"poll"), "desk", None, 1000);
        assert_eq!(store.claim("o1", b"poll", "phone", None, 1001), ClaimOutcome::Refused);
    }

    #[test]
    fn an_offer_request_carries_no_tag_and_only_the_offering_device_files_it() {
        let store = LinkStore::default();
        store.offer("o1", verifier(b"poll"), "desk", None, 1000);
        store.accept("o1", "alice", 1001);
        // Not collected yet: the device has no session, so nothing to attach.
        assert!(!store.record_request("o1", "desk", "req1", None, 1002));
        store.collect("o1", b"poll", 1002);
        assert!(!store.record_request("o1", "other", "req1", None, 1003));
        assert!(!store.record_request("o1", "desk", "req1", Some("tag"), 1003));
        assert!(store.record_request("o1", "desk", "req1", None, 1003));
        let st = store.status("o1", "alice", 1004);
        assert_eq!(st.state, LinkState::Requested);
        assert_eq!(st.request_id.as_deref(), Some("req1"));
        assert_eq!(st.link_tag, None);
        assert!(store.bound_request_ids("alice", 1004).contains("req1"));
    }

    #[test]
    fn the_store_bounds_open_offers() {
        let store = LinkStore::default();
        {
            let mut guard = store.inner.lock().unwrap();
            for i in 0..MAX_OPEN_OFFERS {
                guard.insert(
                    format!("o{i}"),
                    LinkRecord {
                        kind: Kind::Offered,
                        expires_at: 2000,
                        phase: Phase::Offered { poll_verifier: [0; 32], device_id: "d".into(), device_name: None },
                    },
                );
            }
        }
        assert!(!store.offer("one-more", verifier(b"p"), "d", None, 1000));
        // Once they lapse there is room again.
        assert!(store.offer("one-more", verifier(b"p"), "d", None, 2001));
    }
}
