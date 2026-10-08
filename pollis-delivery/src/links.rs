//! QR device links (#1207): an enrolled, PIN-verified device shows a QR; an
//! unauthenticated phone scans it and is enrolled without the email OTP.
//! Protocol, threat model and guardrails: `docs/qr-device-link-design.md`.
//!
//! The DS's part is deliberately narrow:
//!
//! - It stores `SHA-256(claim)` and nothing that derives the link token `t` or
//!   the MAC key, so it can neither forge a link tag nor substitute the key the
//!   account identity is wrapped to — the approver verifies the tag first.
//! - A claim mints a [`SessionScope::DeviceLink`](crate::session::SessionScope)
//!   session, which the OTP-only gates refuse: it can register the claiming
//!   device and file its enrollment request, nothing else.
//! - Single use and the 60-second lifetime are enforced HERE, server-side, not
//!   by the clients that display the QR.
//!
//! **Store:** in-memory, like the OTP and session stores (the DS is
//! single-container). A restart drops open links; the user scans a fresh QR.

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

/// How long a freshly created QR stays claimable. Server-set — the creating
/// client cannot ask for longer.
pub const LINK_TTL_SECS: u64 = 60;

/// How long a CLAIMED link stays readable by its creator: long enough for the
/// phone to register, file its enrollment request (10-minute TTL) and be
/// approved, after which the record is swept.
pub const CLAIMED_LINK_TTL_SECS: u64 = 15 * 60;

/// Open (unclaimed) links one account may hold at once. Each costs memory and
/// is a live QR; a client re-rendering a fresh one every minute never needs
/// more than one or two.
pub const MAX_OPEN_LINKS_PER_ACCOUNT: usize = 3;

/// Sweep threshold for the map, mirroring the OTP store's amortised collector.
const SWEEP_AT: usize = 1024;

#[derive(Clone, Debug)]
enum Phase {
    Open,
    Claimed { device_id: String, device_name: Option<String> },
    Requested { device_id: String, device_name: Option<String>, request_id: String, link_tag: String },
}

#[derive(Clone, Debug)]
struct LinkRecord {
    user_id: String,
    claim_verifier: [u8; 32],
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

/// In-memory link store. `Clone` is shallow (shared `Arc`) so it rides on the
/// `Clone` `AppState`.
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

impl LinkStore {
    /// Register an open link for `user_id`. `false` when the id is taken or the
    /// account is at [`MAX_OPEN_LINKS_PER_ACCOUNT`].
    pub fn create(&self, link_id: &str, user_id: &str, claim_verifier: [u8; 32], now: u64) -> bool {
        let mut guard = self.inner.lock().expect("link store mutex poisoned");
        if guard.len() >= SWEEP_AT {
            sweep(&mut guard, now);
        }
        if let Some(existing) = guard.get(link_id) {
            if now <= existing.expires_at {
                return false;
            }
        }
        let open_for_account = guard
            .values()
            .filter(|r| r.user_id == user_id && now <= r.expires_at && matches!(r.phase, Phase::Open))
            .count();
        if open_for_account >= MAX_OPEN_LINKS_PER_ACCOUNT {
            return false;
        }
        guard.insert(
            link_id.to_string(),
            LinkRecord {
                user_id: user_id.to_string(),
                claim_verifier,
                expires_at: now.saturating_add(LINK_TTL_SECS),
                phase: Phase::Open,
            },
        );
        true
    }

    /// Claim a link with the secret the QR carried. Atomic check-and-flip under
    /// the mutex: of two concurrent claims with the right secret, exactly one
    /// gets `Claimed`.
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
        let presented: [u8; 32] = Sha256::digest(claim).into();
        // Compare first, unconditionally, so timing does not separate "wrong
        // secret" from "right secret on a dead link".
        let secret_ok = constant_time_eq(&presented, &rec.claim_verifier);
        if !secret_ok || now > rec.expires_at || !matches!(rec.phase, Phase::Open) {
            return ClaimOutcome::Refused;
        }
        rec.phase = Phase::Claimed { device_id: device_id.to_string(), device_name };
        rec.expires_at = now.saturating_add(CLAIMED_LINK_TTL_SECS);
        ClaimOutcome::Claimed { user_id: rec.user_id.clone() }
    }

    /// Record the enrollment request the claiming device filed, with its tag.
    /// Only the device that claimed the link may do this, once.
    pub fn record_request(
        &self,
        link_id: &str,
        device_id: &str,
        request_id: &str,
        link_tag: &str,
        now: u64,
    ) -> bool {
        let mut guard = self.inner.lock().expect("link store mutex poisoned");
        let Some(rec) = guard.get_mut(link_id) else {
            return false;
        };
        if now > rec.expires_at {
            return false;
        }
        let Phase::Claimed { device_id: claimed_by, device_name } = &rec.phase else {
            return false;
        };
        if claimed_by != device_id {
            return false;
        }
        rec.phase = Phase::Requested {
            device_id: claimed_by.clone(),
            device_name: device_name.clone(),
            request_id: request_id.to_string(),
            link_tag: link_tag.to_string(),
        };
        true
    }

    /// Enrollment requests of `user_id` currently bound to a live link — the
    /// typed-code approval list leaves these out (they are approved on the
    /// link tag by the device showing the QR).
    pub fn bound_request_ids(&self, user_id: &str, now: u64) -> std::collections::HashSet<String> {
        let guard = self.inner.lock().expect("link store mutex poisoned");
        guard
            .values()
            .filter(|r| r.user_id == user_id && now <= r.expires_at)
            .filter_map(|r| match &r.phase {
                Phase::Requested { request_id, .. } => Some(request_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// The creator's view. `None` when the link is not `user_id`'s (or does not
    /// exist) — the handler answers that with the same `Expired` an absent
    /// link gets, so status is not an existence oracle across accounts.
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
        if rec.user_id != user_id || now > rec.expires_at {
            return expired;
        }
        match &rec.phase {
            Phase::Open => LinkStatusResponse { state: LinkState::Open, ..expired },
            Phase::Claimed { device_id, device_name } => LinkStatusResponse {
                state: LinkState::Claimed,
                new_device_id: Some(device_id.clone()),
                device_name: device_name.clone(),
                ..expired
            },
            Phase::Requested { device_id, device_name, request_id, link_tag } => LinkStatusResponse {
                state: LinkState::Requested,
                new_device_id: Some(device_id.clone()),
                device_name: device_name.clone(),
                request_id: Some(request_id.clone()),
                link_tag: Some(link_tag.clone()),
            },
        }
    }
}

// ── POST /v1/link/create ─────────────────────────────────────────────────────

pub async fn create_link(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<CreateLinkBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let actor = match resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(AuthRejection::Forbidden.into_response()),
    };
    let verifier: [u8; 32] = match crate::util::b64_decode(&parsed.claim_verifier) {
        Ok(v) if v.len() == 32 => v.try_into().expect("checked length"),
        _ => return Ok(bad_request("claim_verifier must be 32 bytes")),
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

// ── POST /v1/link/claim ──────────────────────────────────────────────────────

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
    // Free text the claiming device chose: redact anything IP-shaped before it
    // is held or shown to the approving device.
    let device_name = parsed
        .device_name
        .map(|n| crate::util::redact_ip_literals(&n.chars().take(64).collect::<String>()))
        .filter(|n| !n.trim().is_empty());
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

// ── POST /v1/link/status ─────────────────────────────────────────────────────

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
        assert!(!store.record_request("l1", "phone", "req1", "tag", 1001));
        store.claim("l1", b"c", "phone", Some("Pixel".into()), 1002);
        assert!(!store.record_request("l1", "other-device", "req1", "tag", 1003));
        assert!(store.record_request("l1", "phone", "req1", "tag", 1003));
        // Once.
        assert!(!store.record_request("l1", "phone", "req2", "tag2", 1004));
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
    }
}
