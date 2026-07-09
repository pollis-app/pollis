//! Per-client-IP rate limiting for the unauthenticated signup-OTP endpoints
//! (`request-otp` / `verify-otp`).
//!
//! The per-EMAIL throttle + lockout in [`crate::otp`] stops abuse of a *single*
//! address, but nothing stopped one client from spraying `request-otp` across
//! thousands of addresses (email-bombing arbitrary mailboxes, burning Resend
//! quota/reputation) or `verify-otp` across many addresses (cross-email
//! guessing). This adds the per-IP throttle the OTP bootstrap design always
//! called for (`docs/otp-server-bootstrap-design.md`: "Per-email resend
//! throttle + IP throttle").
//!
//! **Store:** in-memory fixed-window counters (the DS is single-container, same
//! as the OTP/session stores). Behind [`RateLimiter`] so a scaled-out DS can
//! swap it for a shared store without touching the handlers. Reusable beyond the
//! OTP endpoints — `check` is keyed by an arbitrary bucket string.
//!
//! **Client IP:** the DS terminates TLS at a reverse proxy (Cloudflare) and
//! serves plain HTTP, so the socket peer is the proxy, not the client. The real
//! client IP is read from `CF-Connecting-IP` (set/overwritten by Cloudflare — a
//! client cannot forge it *through* Cloudflare), falling back to the first
//! `X-Forwarded-For` hop. Requests with neither header (local/dev/test, never
//! real internet traffic) share one bucket so the limiter is still exercised
//! rather than silently disabled.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;

/// Rate-limit tunables for the OTP endpoints, read from DS env in
/// [`RateLimitConfig::from_env`]. Windows are per client IP.
#[derive(Clone)]
pub struct RateLimitConfig {
    /// Max `request-otp` calls per IP per window.
    pub request_otp_max: u32,
    /// `request-otp` window length, seconds.
    pub request_otp_window_secs: u64,
    /// Max `verify-otp` calls per IP per window.
    pub verify_otp_max: u32,
    /// `verify-otp` window length, seconds.
    pub verify_otp_window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        // Generous for legitimate use (a user requests one or two codes and
        // submits a handful), tight enough to stop bulk abuse from one IP.
        Self {
            request_otp_max: 10,
            request_otp_window_secs: 600,
            verify_otp_max: 30,
            verify_otp_window_secs: 600,
        }
    }
}

impl RateLimitConfig {
    /// Build from DS environment, falling back to [`Default`] per field. Env:
    /// `RL_REQUEST_OTP_MAX`, `RL_REQUEST_OTP_WINDOW_SECS`, `RL_VERIFY_OTP_MAX`,
    /// `RL_VERIFY_OTP_WINDOW_SECS`.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Some(v) = env_u32("RL_REQUEST_OTP_MAX") {
            cfg.request_otp_max = v;
        }
        if let Some(v) = env_u64("RL_REQUEST_OTP_WINDOW_SECS") {
            cfg.request_otp_window_secs = v;
        }
        if let Some(v) = env_u32("RL_VERIFY_OTP_MAX") {
            cfg.verify_otp_max = v;
        }
        if let Some(v) = env_u64("RL_VERIFY_OTP_WINDOW_SECS") {
            cfg.verify_otp_window_secs = v;
        }
        cfg
    }
}

fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok().and_then(|s| s.parse().ok())
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|s| s.parse().ok())
}

/// The outcome of a rate-limit check.
#[derive(Debug, PartialEq, Eq)]
pub enum RateLimitOutcome {
    Allowed,
    /// The client exceeded `max` in the current window → the caller should 429.
    Limited,
}

/// One IP's counter within the current fixed window.
struct Window {
    count: u32,
    window_start: u64,
}

/// In-memory per-key fixed-window rate limiter. `Clone` is shallow (shared
/// `Arc`) so it rides on the `Clone` `AppState`.
#[derive(Clone, Default)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<String, Window>>>,
}

/// Above this many tracked keys, a `check` opportunistically drops windows whose
/// span has fully elapsed, so an ever-changing IP set can't grow the map without
/// bound on a long-lived container.
const PRUNE_THRESHOLD: usize = 10_000;

impl RateLimiter {
    /// Record one hit for `key` and report whether it is within `max` per
    /// `window_secs`. Fixed window: the first hit starts a window; once the
    /// window elapses the counter resets. A key over its limit stays [`Limited`]
    /// until its window rolls over.
    ///
    /// [`Limited`]: RateLimitOutcome::Limited
    pub fn check(&self, key: &str, max: u32, window_secs: u64, now: u64) -> RateLimitOutcome {
        let mut guard = self.inner.lock().expect("rate limiter mutex poisoned");

        if guard.len() > PRUNE_THRESHOLD {
            guard.retain(|_, w| now.saturating_sub(w.window_start) < window_secs);
        }

        let win = guard.entry(key.to_string()).or_insert(Window {
            count: 0,
            window_start: now,
        });
        if now.saturating_sub(win.window_start) >= window_secs {
            win.count = 0;
            win.window_start = now;
        }
        win.count = win.count.saturating_add(1);
        if win.count > max {
            RateLimitOutcome::Limited
        } else {
            RateLimitOutcome::Allowed
        }
    }
}

/// The client IP for rate-limit keying. Prefers `CF-Connecting-IP` (Cloudflare
/// sets it and a client cannot forge it through Cloudflare), then the first
/// `X-Forwarded-For` hop. Absent both (local/dev/test), returns a shared
/// sentinel so the limiter is still exercised rather than bypassed.
pub fn client_ip(headers: &HeaderMap) -> String {
    if let Some(ip) = header_str(headers, "cf-connecting-ip") {
        return ip.to_string();
    }
    if let Some(xff) = header_str(headers, "x-forwarded-for") {
        // `X-Forwarded-For: client, proxy1, proxy2` — the first hop is the client.
        if let Some(first) = xff.split(',').next() {
            let first = first.trim();
            if !first.is_empty() {
                return first.to_string();
            }
        }
    }
    "unknown".to_string()
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_max_then_limits() {
        let rl = RateLimiter::default();
        // max = 3 per 60s.
        for _ in 0..3 {
            assert_eq!(rl.check("1.2.3.4", 3, 60, 1000), RateLimitOutcome::Allowed);
        }
        assert_eq!(rl.check("1.2.3.4", 3, 60, 1000), RateLimitOutcome::Limited);
        // Still limited later in the same window.
        assert_eq!(rl.check("1.2.3.4", 3, 60, 1030), RateLimitOutcome::Limited);
    }

    #[test]
    fn window_resets_after_it_elapses() {
        let rl = RateLimiter::default();
        for _ in 0..3 {
            rl.check("1.2.3.4", 3, 60, 1000);
        }
        assert_eq!(rl.check("1.2.3.4", 3, 60, 1000), RateLimitOutcome::Limited);
        // A full window later, the counter resets.
        assert_eq!(rl.check("1.2.3.4", 3, 60, 1061), RateLimitOutcome::Allowed);
    }

    #[test]
    fn keys_are_independent() {
        let rl = RateLimiter::default();
        for _ in 0..3 {
            rl.check("1.1.1.1", 3, 60, 1000);
        }
        assert_eq!(rl.check("1.1.1.1", 3, 60, 1000), RateLimitOutcome::Limited);
        // A different IP has its own fresh window.
        assert_eq!(rl.check("2.2.2.2", 3, 60, 1000), RateLimitOutcome::Allowed);
    }

    #[test]
    fn client_ip_prefers_cf_then_xff_then_sentinel() {
        let mut h = HeaderMap::new();
        assert_eq!(client_ip(&h), "unknown");

        h.insert("x-forwarded-for", "9.9.9.9, 10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&h), "9.9.9.9");

        h.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());
        assert_eq!(client_ip(&h), "203.0.113.7");
    }
}
