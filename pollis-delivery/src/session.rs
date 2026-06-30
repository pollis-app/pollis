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

/// One live session. Minted on a verified OTP, consumed by the bootstrap
/// endpoints. `expires_at` is unix seconds.
#[derive(Clone)]
pub struct SessionRecord {
    pub user_id: String,
    pub email: String,
    pub device_id: String,
    pub expires_at: u64,
}

/// What the gate hands back once a token resolves to a live session. `user_id`
/// here is authoritative — handlers bind it, never a body field.
#[derive(Clone)]
pub struct SessionClaims {
    pub user_id: String,
    pub email: String,
    pub device_id: String,
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

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
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
        let mut raw = [0u8; 32];
        OsRng.fill_bytes(&mut raw);
        let token = hex_lower(&raw);
        let record = SessionRecord {
            user_id: user_id.to_string(),
            email: email.to_string(),
            device_id: device_id.to_string(),
            expires_at: now.saturating_add(ttl_secs),
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
pub fn verify_session(
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

    #[test]
    fn unknown_token_rejected() {
        let store = SessionStore::default();
        assert!(store.resolve("deadbeef", 1000).is_none());
    }
}
