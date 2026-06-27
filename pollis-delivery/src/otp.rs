//! Server-side OTP: generation, salted-hash storage, attempt-limited
//! constant-time verification, and the Resend email send — all moved off the
//! client (which used to hold a baked-in Resend key + an in-process OTP map).
//! See `docs/otp-server-bootstrap-design.md`.
//!
//! This also FIXES the client-side OTP's unlimited-guess bug: each code now has
//! a per-email attempt counter, locks out (and is deleted) past
//! [`OtpConfig::max_attempts`], compares in constant time, and is deleted on the
//! first success (single-use).
//!
//! **Store:** in-memory (the DS is single-container — mirrors the OTP map the
//! client used to keep). Behind [`OtpStore`] so a scaled-out DS can swap it for a
//! Turso table without touching the handlers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use rand::rngs::OsRng;
use rand::{Rng, RngCore};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use ulid::Ulid;

use crate::writes::bad_request;
use crate::AppState;

/// Tunables for the OTP + session machinery, read from DS env in
/// [`OtpConfig::from_env`].
#[derive(Clone)]
pub struct OtpConfig {
    /// Resend API key (DS env `RESEND_API_KEY`). `None` → email send is skipped
    /// (every request still 200s; useful only with `dev_otp`).
    pub resend_api_key: Option<String>,
    /// `DEV_OTP` override — when set, the email send is skipped and this exact
    /// code is the only one that verifies. Mirrors pollis-core's `DEV_OTP` so the
    /// integration harness + local dev keep working without a real mailbox.
    pub dev_otp: Option<String>,
    /// OTP lifetime, seconds (env `OTP_TTL_SECS`, default 600).
    pub ttl_secs: u64,
    /// Session-token lifetime, seconds (default 600).
    pub session_ttl_secs: u64,
    /// Minimum seconds between two emails for the same address.
    pub resend_throttle_secs: u64,
    /// Wrong-guess lockout threshold; the `(max+1)`-th wrong guess locks out and
    /// deletes the code.
    pub max_attempts: u32,
}

impl Default for OtpConfig {
    fn default() -> Self {
        Self {
            resend_api_key: None,
            dev_otp: None,
            ttl_secs: 600,
            session_ttl_secs: 600,
            resend_throttle_secs: 30,
            max_attempts: 5,
        }
    }
}

impl OtpConfig {
    /// Build from DS environment. `RESEND_API_KEY` (the key the client no longer
    /// ships), `DEV_OTP` (harness/local override), `OTP_TTL_SECS` (optional).
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        cfg.resend_api_key = std::env::var("RESEND_API_KEY").ok().filter(|s| !s.is_empty());
        cfg.dev_otp = std::env::var("DEV_OTP").ok().filter(|s| !s.is_empty());
        if let Some(ttl) = std::env::var("OTP_TTL_SECS").ok().and_then(|s| s.parse().ok()) {
            cfg.ttl_secs = ttl;
        }
        cfg
    }
}

/// One stored OTP. The code itself is never kept — only `SHA-256(salt || code)`.
struct OtpRecord {
    code_hash: [u8; 32],
    salt: [u8; 16],
    expires_at: u64,
    attempts: u32,
    last_sent_at: u64,
    locked: bool,
}

/// In-memory OTP store keyed on the normalized email. `Clone` is shallow (shared
/// `Arc`) so it rides on the `Clone` `AppState`.
#[derive(Clone, Default)]
pub struct OtpStore {
    inner: Arc<Mutex<HashMap<String, OtpRecord>>>,
}

/// Normalize an email for store keying so request/verify always agree: trim +
/// lowercase. (The `users` table is still queried with the as-typed address.)
fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

fn salted_hash(salt: &[u8; 16], code: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(code.trim().as_bytes());
    h.finalize().into()
}

/// Constant-time byte compare — replicated from pollis-core's
/// `device_enrollment::constant_time_eq` to keep the dependency surface small.
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

/// Outcome of preparing a code for an email.
pub enum PrepareOutcome {
    /// A fresh code was stored; email this plaintext.
    Send(String),
    /// Within the resend-throttle window; do NOT email, but still 200 the caller.
    Throttled,
}

/// Outcome of verifying a submitted code.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    Ok,
    Invalid,
    LockedOut,
    Expired,
    NotFound,
}

impl OtpStore {
    /// Store a fresh `code` for `email` (replacing any prior one), unless the
    /// last send is within the resend-throttle window (→ [`PrepareOutcome::Throttled`]).
    pub fn prepare(
        &self,
        email: &str,
        code: &str,
        ttl_secs: u64,
        resend_throttle_secs: u64,
        now: u64,
    ) -> PrepareOutcome {
        let key = normalize_email(email);
        let mut guard = self.inner.lock().expect("otp store mutex poisoned");
        if let Some(rec) = guard.get(&key) {
            if !rec.locked
                && now < rec.expires_at
                && now.saturating_sub(rec.last_sent_at) < resend_throttle_secs
            {
                return PrepareOutcome::Throttled;
            }
        }
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        guard.insert(
            key,
            OtpRecord {
                code_hash: salted_hash(&salt, code),
                salt,
                expires_at: now.saturating_add(ttl_secs),
                attempts: 0,
                last_sent_at: now,
                locked: false,
            },
        );
        PrepareOutcome::Send(code.to_string())
    }

    /// Verify a submitted `code` against the stored record. Constant-time
    /// compare; increments the attempt counter on a wrong guess and locks out +
    /// deletes past `max_attempts`; deletes the record on success (single-use)
    /// and on expiry.
    pub fn verify(&self, email: &str, code: &str, max_attempts: u32, now: u64) -> VerifyOutcome {
        let key = normalize_email(email);
        let mut guard = self.inner.lock().expect("otp store mutex poisoned");
        let rec = match guard.get_mut(&key) {
            Some(r) => r,
            None => return VerifyOutcome::NotFound,
        };
        if rec.locked {
            return VerifyOutcome::LockedOut;
        }
        if now > rec.expires_at {
            guard.remove(&key);
            return VerifyOutcome::Expired;
        }
        let provided = salted_hash(&rec.salt, code);
        if constant_time_eq(&provided, &rec.code_hash) {
            // Single-use: a correct code can never be replayed.
            guard.remove(&key);
            return VerifyOutcome::Ok;
        }
        rec.attempts += 1;
        if rec.attempts > max_attempts {
            // Lock out and delete — the bug fix: no unlimited guessing.
            guard.remove(&key);
            return VerifyOutcome::LockedOut;
        }
        VerifyOutcome::Invalid
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── POST /v1/auth/request-otp ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RequestOtpBody {
    pub email: String,
}

/// POST /v1/auth/request-otp — generate + store a 6-digit OTP and email it via
/// Resend. **Always 200** regardless of whether the email maps to an account
/// (anti-enumeration). Honors `DEV_OTP` (skip send, force the code) so the
/// harness/local dev work without a mailbox.
pub async fn request_otp(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let parsed: RequestOtpBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let email = parsed.email.trim().to_string();
    // Empty email: nothing to do, but still 200 (don't reveal validation state).
    if email.is_empty() {
        return ok_200();
    }

    let cfg = &state.otp_config;
    let code = match &cfg.dev_otp {
        Some(dev) => dev.clone(),
        None => format!("{:06}", OsRng.gen_range(0..1_000_000u32)),
    };

    let outcome = state.otp.prepare(
        &email,
        &code,
        cfg.ttl_secs,
        cfg.resend_throttle_secs,
        now_unix(),
    );

    match outcome {
        PrepareOutcome::Throttled => return ok_200(),
        PrepareOutcome::Send(code) => {
            // DEV_OTP: skip the real send entirely.
            if cfg.dev_otp.is_some() {
                tracing::info!("DEV_OTP active — skipping email send for {email}");
                return ok_200();
            }
            if let Some(key) = &cfg.resend_api_key {
                if let Err(e) = send_otp_email(key, &email, &code).await {
                    // A send failure leaks nothing about account existence; log
                    // and still 200 so the response is uniform.
                    tracing::error!("OTP email send failed for {email}: {e:#}");
                }
            } else {
                tracing::warn!("RESEND_API_KEY unset — OTP email NOT sent for {email}");
            }
        }
    }
    ok_200()
}

async fn send_otp_email(api_key: &str, email: &str, code: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "from": "Pollis <noreply@mail.pollis.com>",
        "to": [email],
        "subject": "Your Pollis sign-in code",
        "text": format!("Your verification code is: {code}\n\nThis code expires in 10 minutes."),
    });
    let resp = client
        .post("https://api.resend.com/emails")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let txt = resp.text().await.unwrap_or_default();
        anyhow::bail!("Resend {}: {txt}", "non-success");
    }
    Ok(())
}

// ── POST /v1/auth/verify-otp ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct VerifyOtpBody {
    pub email: String,
    pub code: String,
    pub device_id: String,
    /// Informational only — verify-otp NEVER writes `account_id_pub` (identity is
    /// established by the separate, CAS-guarded `establish-identity`). Accepted
    /// for forward-compat with the client and deliberately ignored here.
    #[serde(default)]
    pub account_id_pub: Option<String>,
}

/// POST /v1/auth/verify-otp — constant-time, attempt-limited code check; then
/// create-or-load the account and mint an OTP-session token. On success returns
/// `{user_id, username, is_new_account, has_identity, session_token,
/// session_expires_at}`. A wrong/expired/locked code → 401/429; never touches
/// the account on a failed code.
pub async fn verify_otp(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let parsed: VerifyOtpBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let email = parsed.email.trim().to_string();
    let device_id = parsed.device_id.trim().to_string();
    if email.is_empty() || device_id.is_empty() {
        return bad_request("email and device_id required");
    }

    let cfg = &state.otp_config;
    match state
        .otp
        .verify(&email, &parsed.code, cfg.max_attempts, now_unix())
    {
        VerifyOutcome::Ok => {}
        VerifyOutcome::LockedOut => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({ "error": "too many attempts" })),
            )
                .into_response()
        }
        VerifyOutcome::Invalid | VerifyOutcome::Expired | VerifyOutcome::NotFound => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "invalid code" })),
            )
                .into_response()
        }
    }

    // Code is good: create or load the account. Mirrors pollis-core
    // `auth::verify_otp` — server-gen ULID id + a default username from the email
    // prefix plus a 4-char ULID suffix for uniqueness.
    let conn = match state.db.conn() {
        Ok(c) => c,
        Err(e) => return internal(e),
    };

    let existing = match conn
        .query(
            "SELECT id, username, account_id_pub FROM users WHERE email = ?1",
            libsql::params![email.clone()],
        )
        .await
    {
        Ok(mut rows) => match rows.next().await {
            Ok(r) => r,
            Err(e) => return internal(e.into()),
        },
        Err(e) => return internal(e.into()),
    };

    let (user_id, username, has_identity, is_new_account) = match existing {
        Some(row) => {
            let id: String = match row.get(0) {
                Ok(v) => v,
                Err(e) => return internal(e.into()),
            };
            let uname: String = row.get(1).unwrap_or_else(|_| {
                email.split('@').next().unwrap_or("user").to_string()
            });
            let pub_bytes: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(2).ok().flatten();
            (id, uname, pub_bytes.is_some(), false)
        }
        None => {
            let user_id = Ulid::new().to_string();
            let suffix = &user_id[user_id.len().saturating_sub(4)..];
            let email_prefix = email.split('@').next().unwrap_or("user");
            let default_username = format!("{email_prefix}_{suffix}");
            if let Err(e) = conn
                .execute(
                    "INSERT INTO users (id, email, username) VALUES (?1, ?2, ?3)",
                    libsql::params![user_id.clone(), email.clone(), default_username.clone()],
                )
                .await
            {
                return internal(e.into());
            }
            (user_id, default_username, false, true)
        }
    };

    let now = now_unix();
    let session_token = state
        .sessions
        .mint(&user_id, &email, &device_id, cfg.session_ttl_secs, now);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "user_id": user_id,
            "username": username,
            "is_new_account": is_new_account,
            "has_identity": has_identity,
            "session_token": session_token,
            "session_expires_at": now + cfg.session_ttl_secs,
        })),
    )
        .into_response()
}

// ── small response helpers ───────────────────────────────────────────────────

fn ok_200() -> Response {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

fn internal(e: anyhow::Error) -> Response {
    tracing::error!("verify-otp internal error: {e:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "internal error" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_use_then_replay_fails() {
        let store = OtpStore::default();
        store.prepare("a@x.com", "123456", 600, 0, 1000);
        assert_eq!(store.verify("a@x.com", "123456", 5, 1000), VerifyOutcome::Ok);
        // Replay: the record was deleted on success.
        assert_eq!(
            store.verify("a@x.com", "123456", 5, 1000),
            VerifyOutcome::NotFound
        );
    }

    #[test]
    fn lockout_after_six_wrong_then_correct_fails() {
        let store = OtpStore::default();
        store.prepare("a@x.com", "123456", 600, 0, 1000);
        // 5 wrong guesses are merely invalid.
        for _ in 0..5 {
            assert_eq!(
                store.verify("a@x.com", "000000", 5, 1000),
                VerifyOutcome::Invalid
            );
        }
        // The 6th locks out and deletes the code.
        assert_eq!(
            store.verify("a@x.com", "000000", 5, 1000),
            VerifyOutcome::LockedOut
        );
        // The correct code no longer works.
        assert_ne!(
            store.verify("a@x.com", "123456", 5, 1000),
            VerifyOutcome::Ok
        );
    }

    #[test]
    fn expired_code_rejected() {
        let store = OtpStore::default();
        store.prepare("a@x.com", "123456", 600, 0, 1000);
        assert_eq!(
            store.verify("a@x.com", "123456", 5, 2000),
            VerifyOutcome::Expired
        );
    }

    #[test]
    fn throttle_skips_resend() {
        let store = OtpStore::default();
        assert!(matches!(
            store.prepare("a@x.com", "111111", 600, 30, 1000),
            PrepareOutcome::Send(_)
        ));
        assert!(matches!(
            store.prepare("a@x.com", "222222", 600, 30, 1010),
            PrepareOutcome::Throttled
        ));
    }
}
