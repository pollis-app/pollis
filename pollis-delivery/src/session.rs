//! Short-lived OTP-session tokens — the gate for the bootstrap writes that
//! *establish* a device's signing credential and so cannot be device-signed
//! (account-identity establishment, device registration, the first cert
//! publish). See `docs/otp-server-bootstrap-design.md`.
//!
//! An opaque 256-bit bearer stored only as its SHA-256; `user_id` binds from the record, never the body.
//!
//! **Store:** in-memory; the DS is single-container.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::AuthRejection;

/// Header carrying the raw session token on bootstrap requests.
pub const SESSION_HEADER: &str = "x-pollis-session";

/// What a session may authorize (#1207).
///
/// A QR device-link session may only enroll ONE device; the scope lets a gate refuse by type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionScope {
    /// Minted by `verify-otp`: proved control of the mailbox.
    Otp,
    /// Minted by `link/claim`: proved possession of a QR shown by an enrolled,
    /// PIN-verified device of this account. Enrollment-only.
    DeviceLink { link_id: String },
    /// Minted by `link/poll-offer` (#1361): enrollment-only, like `DeviceLink`, with no link tag.
    DeviceOffer { link_id: String },
}

/// One live session; `expires_at` is unix seconds.
#[derive(Clone)]
pub struct SessionRecord {
    pub user_id: String,
    pub email: String,
    pub device_id: String,
    pub expires_at: u64,
    pub scope: SessionScope,
}

/// What the gate hands back once a token resolves to a live session. `user_id`
/// here is authoritative — handlers bind it, never a body field.
#[derive(Clone)]
pub struct SessionClaims {
    pub user_id: String,
    pub email: String,
    pub device_id: String,
    pub scope: SessionScope,
}

/// Keyed by `SHA-256(token)` so the raw token is never at rest.
#[derive(Clone, Default)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<[u8; 32], SessionRecord>>>,
}

fn hash_token(token: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    h.finalize().into()
}

impl SessionStore {
    /// Mint a session valid for `ttl_secs`; returns the raw token, keeping only its hash.
    pub fn mint(
        &self,
        user_id: &str,
        email: &str,
        device_id: &str,
        ttl_secs: u64,
        now: u64,
    ) -> String {
        self.mint_scoped(user_id, email, device_id, SessionScope::Otp, ttl_secs, now)
    }

    /// [`mint`](Self::mint) for a claimed QR device link: an enrollment-only
    /// session for `device_id` on the link's account (#1207).
    pub fn mint_device_link(
        &self,
        user_id: &str,
        email: &str,
        device_id: &str,
        link_id: &str,
        ttl_secs: u64,
        now: u64,
    ) -> String {
        let scope = SessionScope::DeviceLink { link_id: link_id.to_string() };
        self.mint_scoped(user_id, email, device_id, scope, ttl_secs, now)
    }

    /// [`mint`](Self::mint) for a collected v2 offer: an enrollment-only
    /// session for the device that showed the QR (#1361).
    pub fn mint_device_offer(
        &self,
        user_id: &str,
        email: &str,
        device_id: &str,
        link_id: &str,
        ttl_secs: u64,
        now: u64,
    ) -> String {
        let scope = SessionScope::DeviceOffer { link_id: link_id.to_string() };
        self.mint_scoped(user_id, email, device_id, scope, ttl_secs, now)
    }

    fn mint_scoped(
        &self,
        user_id: &str,
        email: &str,
        device_id: &str,
        scope: SessionScope,
        ttl_secs: u64,
        now: u64,
    ) -> String {
        let mut raw = [0u8; 32];
        OsRng.fill_bytes(&mut raw);
        let token = hex::encode(raw);
        let record = SessionRecord {
            user_id: user_id.to_string(),
            email: email.to_string(),
            device_id: device_id.to_string(),
            expires_at: now.saturating_add(ttl_secs),
            scope,
        };
        self.inner
            .lock()
            .expect("session store mutex poisoned")
            .insert(hash_token(&token), record);
        token
    }

    /// Resolve a raw token to its live claims; an expired record is removed.
    pub fn resolve(&self, token: &str, now: u64) -> Option<SessionClaims> {
        let key = hash_token(token);
        let mut guard = self.inner.lock().expect("session store mutex poisoned");
        match guard.get(&key) {
            Some(rec) if now <= rec.expires_at => Some(SessionClaims {
                user_id: rec.user_id.clone(),
                email: rec.email.clone(),
                device_id: rec.device_id.clone(),
                scope: rec.scope.clone(),
            }),
            Some(_) => {
                guard.remove(&key);
                None
            }
            None => None,
        }
    }

    /// Single-use teardown so the token can't be replayed.
    pub fn invalidate(&self, token: &str) {
        self.inner
            .lock()
            .expect("session store mutex poisoned")
            .remove(&hash_token(token));
    }
}

pub fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers.get(SESSION_HEADER)?.to_str().ok()
}

/// The session gate; bind `user_id` from the returned claims, never the body. Never fails open.
///
/// **OTP sessions only.** Device-link sessions get the same 401 as an unknown token (#1207).
pub fn verify_session(
    headers: &HeaderMap,
    store: &SessionStore,
    now: u64,
) -> Result<SessionClaims, AuthRejection> {
    let claims = verify_session_for_enrollment(headers, store, now)?;
    if claims.scope != SessionScope::Otp {
        return Err(AuthRejection::Unauthorized);
    }
    Ok(claims)
}

/// The enrollment-path gate: accepts an OTP **or** a device-link session.
pub fn verify_session_for_enrollment(
    headers: &HeaderMap,
    store: &SessionStore,
    now: u64,
) -> Result<SessionClaims, AuthRejection> {
    let token = session_token(headers).ok_or(AuthRejection::Unauthorized)?;
    if token.is_empty() {
        return Err(AuthRejection::Unauthorized);
    }
    store.resolve(token, now).ok_or(AuthRejection::Unauthorized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_then_resolve_binds_user() {
        let store = SessionStore::default();
        let token = store.mint("u1", "u1@x.com", "dev1", 600, 1000);
        let claims = store.resolve(&token, 1000).expect("live");
        assert_eq!(claims.user_id, "u1");
        assert_eq!(claims.device_id, "dev1");
    }

    #[test]
    fn expired_token_rejected_and_removed() {
        let store = SessionStore::default();
        let token = store.mint("u1", "u1@x.com", "dev1", 600, 1000);
        assert!(store.resolve(&token, 2000).is_none());
        // Removed on lookup — a later in-window check still fails.
        assert!(store.resolve(&token, 1000).is_none());
    }

    #[test]
    fn invalidate_makes_token_unusable() {
        let store = SessionStore::default();
        let token = store.mint("u1", "u1@x.com", "dev1", 600, 1000);
        store.invalidate(&token);
        assert!(store.resolve(&token, 1000).is_none());
    }

    fn headers_with(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(SESSION_HEADER, token.parse().unwrap());
        h
    }

    /// #1207: a device-link session must not pass the OTP gate.
    #[test]
    fn a_device_link_session_is_refused_by_the_otp_gate() {
        let store = SessionStore::default();
        let token = store.mint_device_link("u1", "u1@x.com", "dev2", "link-1", 600, 1000);
        assert!(verify_session(&headers_with(&token), &store, 1000).is_err());
        let claims = verify_session_for_enrollment(&headers_with(&token), &store, 1000)
            .expect("the enrollment gate accepts it");
        assert_eq!(claims.user_id, "u1");
        assert_eq!(claims.device_id, "dev2");
        assert_eq!(claims.scope, SessionScope::DeviceLink { link_id: "link-1".into() });
    }

    /// #1361: a session collected from a v2 offer is enrollment-only too.
    #[test]
    fn a_device_offer_session_is_refused_by_the_otp_gate() {
        let store = SessionStore::default();
        let token = store.mint_device_offer("u1", "u1@x.com", "desk", "offer-1", 600, 1000);
        assert!(verify_session(&headers_with(&token), &store, 1000).is_err());
        let claims = verify_session_for_enrollment(&headers_with(&token), &store, 1000)
            .expect("the enrollment gate accepts it");
        assert_eq!(claims.device_id, "desk");
        assert_eq!(claims.scope, SessionScope::DeviceOffer { link_id: "offer-1".into() });
    }

    #[test]
    fn an_otp_session_passes_both_gates() {
        let store = SessionStore::default();
        let token = store.mint("u1", "u1@x.com", "dev1", 600, 1000);
        assert_eq!(verify_session(&headers_with(&token), &store, 1000).unwrap().scope, SessionScope::Otp);
        assert!(verify_session_for_enrollment(&headers_with(&token), &store, 1000).is_ok());
    }

    #[test]
    fn unknown_token_rejected() {
        let store = SessionStore::default();
        assert!(store.resolve("deadbeef", 1000).is_none());
    }
}
