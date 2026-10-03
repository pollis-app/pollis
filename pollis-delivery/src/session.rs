//! Short-lived OTP-session tokens — the gate for the bootstrap writes that
//! *establish* a device's signing credential and so cannot be device-signed
//! (account-identity establishment, device registration, the first cert
//! publish). See `docs/otp-server-bootstrap-design.md`.
//!
//! A session is minted by [`verify-otp`](crate::otp) once the OTP is proven and
//! carries a capability scoped to exactly one `user_id`. The token is an opaque
//! 256-bit bearer (NOT a JWT): the raw token is returned to the client once and
//! the DS stores only its SHA-256 hash, so a dump of this map never yields a
//! usable token. TTL is short (default 10 min). The gate binds `user_id` from
//! the stored record — NEVER from the request body — the same property
//! `resolve_actor` gives the device-signature path.
//!
//! **Store:** in-memory (the DS is single-container, mirroring the OTP store).
//! Behind a small struct so a horizontally-scaled DS can swap it for a Turso
//! `otp_session` table without touching the handlers.

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
/// An OTP session can do everything the bootstrap path needs, including the
/// pre-enrollment soft reset (`rotate-identity` under a session wipes devices
/// and memberships). A session minted by claiming a QR device link must not:
/// all it exists for is enrolling ONE new device, whose key still only moves
/// when an enrolled device approves. The scope travels with the record so a
/// gate can refuse by type rather than by convention — see
/// [`verify_session`] (OTP only) vs [`verify_session_for_enrollment`].
/// `docs/qr-device-link-design.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionScope {
    /// Minted by `verify-otp`: proved control of the mailbox.
    Otp,
    /// Minted by `link/claim`: proved possession of a QR shown by an enrolled,
    /// PIN-verified device of this account. Enrollment-only.
    DeviceLink { link_id: String },
}

/// One live session. Minted on a verified OTP (or a claimed device link),
/// consumed by the bootstrap endpoints. `expires_at` is unix seconds.
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

/// In-memory session store keyed by `SHA-256(token)` so the raw token is never
/// at rest. `Clone` is shallow (shared `Arc`) so it rides on the `Clone`
/// `AppState`.
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
    /// Mint a fresh session for `(user_id, email, device_id)` valid for
    /// `ttl_secs` from `now`. Returns the raw token to hand the client exactly
    /// once; only its hash is retained here.
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

    /// Resolve a raw token to its live claims, or `None` if unknown/expired. An
    /// expired record is removed on lookup.
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

    /// Single-use teardown: drop the token so it can't be replayed (called when
    /// the bootstrap sequence completes at cert publish).
    pub fn invalidate(&self, token: &str) {
        self.inner
            .lock()
            .expect("session store mutex poisoned")
            .remove(&hash_token(token));
    }
}

/// Pull the raw session token off the request headers.
pub fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers.get(SESSION_HEADER)?.to_str().ok()
}

/// The session gate, sibling to [`crate::auth::verify_request`]. Returns the
/// authenticated [`SessionClaims`] (bind `user_id` from here, never the body) or
/// [`AuthRejection::Unauthorized`] for a missing/unknown/expired token. Never
/// fails open.
///
/// **OTP sessions only.** A [`SessionScope::DeviceLink`] session is refused here
/// with the same 401 as an unknown token, so every gate written before QR links
/// existed — establish-identity, the soft reset, reset-and-recover — keeps
/// exactly the powers it had, and a new session-gated endpoint is OTP-only
/// unless it deliberately calls [`verify_session_for_enrollment`] (#1207).
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

/// The session gate for the enrollment path a linked device walks:
/// `register-device`, `enrollment-request`, and the new device's enrollment
/// read. Accepts an OTP **or** a device-link session; nothing else may call it.
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

    /// #1207: a device-link session must not pass the OTP gate that guards the
    /// soft reset, reset-and-recover and establish-identity — it is
    /// enrollment-only.
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

    /// An OTP session passes both gates, unchanged.
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
