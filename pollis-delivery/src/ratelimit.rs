//! Per-client rate limiting for the whole service, by tier: in-memory
//! fixed-window counters (the DS is single-container).
//!
//! In production the container never sees a client IP, only the Worker's
//! `X-Pollis-Client-Bucket`; local runs fall back to the IP headers. Either way
//! the identity is re-hashed under a per-process random key before it reaches
//! the map, and [`ClientKey`] can only be built by hashing, so a raw IP cannot
//! be stored by construction. See `.codesight/wiki/overview.md`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;

use axum::{
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use crate::AppState;

/// Per-tier budgets (max calls per client per window, window in seconds); see
/// `classify` for which requests land in which tier.
#[derive(Clone)]
pub struct RateLimitConfig {
    pub request_otp_max: u32,
    pub request_otp_window_secs: u64,
    pub verify_otp_max: u32,
    pub verify_otp_window_secs: u64,
    /// A flood backstop only; device-signed writes are already credential-gated.
    pub write_max: u32,
    pub write_window_secs: u64,
    /// A brute-force bound on join codes, which the write tier is not (#847).
    pub invite_redeem_max: u32,
    pub invite_redeem_window_secs: u64,
    /// Reads are POSTs, and a cold launch issues dozens at once (#987).
    pub read_max: u32,
    pub read_window_secs: u64,
    /// Endpoints whose input is guessable (#987).
    pub probe_max: u32,
    pub probe_window_secs: u64,
    /// GETs other than the liveness probes: operator endpoints behind a static
    /// bearer token, which needs an attempt bound.
    pub get_max: u32,
    pub get_window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            request_otp_max: 10,
            request_otp_window_secs: 600,
            verify_otp_max: 30,
            verify_otp_window_secs: 600,
            write_max: 1200,
            write_window_secs: 60,
            invite_redeem_max: 20,
            invite_redeem_window_secs: 600,
            read_max: 3000,
            read_window_secs: 60,
            probe_max: 60,
            probe_window_secs: 600,
            get_max: 120,
            get_window_secs: 600,
        }
    }
}

/// A tier max that never limits: [`RateLimiter::check`] saturates its counter
/// at `u32::MAX` and limits only when the count EXCEEDS the max.
pub const UNLIMITED: u32 = u32::MAX;

impl RateLimitConfig {
    /// `RL_{TIER}_{MAX,WINDOW_SECS}` per field, else [`Default`].
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Some(v) = env_max("RL_REQUEST_OTP_MAX") {
            cfg.request_otp_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_REQUEST_OTP_WINDOW_SECS") {
            cfg.request_otp_window_secs = v;
        }
        if let Some(v) = env_max("RL_VERIFY_OTP_MAX") {
            cfg.verify_otp_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_VERIFY_OTP_WINDOW_SECS") {
            cfg.verify_otp_window_secs = v;
        }
        if let Some(v) = env_max("RL_GET_MAX") {
            cfg.get_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_GET_WINDOW_SECS") {
            cfg.get_window_secs = v;
        }
        if let Some(v) = env_max("RL_WRITE_MAX") {
            cfg.write_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_WRITE_WINDOW_SECS") {
            cfg.write_window_secs = v;
        }
        if let Some(v) = env_max("RL_READ_MAX") {
            cfg.read_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_READ_WINDOW_SECS") {
            cfg.read_window_secs = v;
        }
        if let Some(v) = env_max("RL_PROBE_MAX") {
            cfg.probe_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_PROBE_WINDOW_SECS") {
            cfg.probe_window_secs = v;
        }
        if let Some(v) = env_max("RL_INVITE_REDEEM_MAX") {
            cfg.invite_redeem_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_INVITE_REDEEM_WINDOW_SECS") {
            cfg.invite_redeem_window_secs = v;
        }
        cfg
    }
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok().and_then(|s| s.parse().ok())
}

/// A tier's `RL_*_MAX`: a number, or `off` for [`UNLIMITED`] (dev's OTP tiers,
/// for the e2e suites).
fn env_max(key: &str) -> Option<u32> {
    let raw = std::env::var(key).ok()?;
    parse_max(&raw)
}

/// A tier max for operator JSON: the number, or `"off"` for [`UNLIMITED`].
pub fn max_json(max: u32) -> serde_json::Value {
    if max == UNLIMITED {
        serde_json::Value::from("off")
    } else {
        serde_json::Value::from(max)
    }
}

fn parse_max(raw: &str) -> Option<u32> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("off") {
        return Some(UNLIMITED);
    }
    raw.parse().ok()
}

#[derive(Debug, PartialEq, Eq)]
pub enum RateLimitOutcome {
    Allowed,
    Limited,
}

struct Window {
    count: u32,
    window_start: u64,
    /// Carried per entry: one map serves every tier, so the pruner must judge
    /// each entry by its own window, not the caller's.
    window_secs: u64,
}

type HmacSha256 = Hmac<Sha256>;

/// 128 bits: a collision would merely share a budget.
const CLIENT_HASH_BYTES: usize = 16;

const EMAIL_TAG_BYTES: usize = 8;

/// The `request-otp` tier name, shared with the dev ops log.
pub(crate) const OTP_REQUEST_TIER: &str = "otp_request";

/// `{tier}:{hex(HMAC-SHA256(process_key, identity)[..16])}`. The only
/// constructor hashes, so a `ClientKey` holding a raw IP cannot exist (which is
/// also why `Debug` is safe).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClientKey(String);

impl ClientKey {
    /// The hex HMAC without the tier prefix: the same in every tier, so the dev
    /// ops log can correlate one client across endpoints.
    pub(crate) fn pseudonym(&self) -> &str {
        self.0.split_once(':').map_or(self.0.as_str(), |(_, h)| h)
    }
}

/// In-memory per-key fixed-window rate limiter. `Clone` is shallow, so every
/// clone shares the process key and the map.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<ClientKey, Window>>>,
    /// Pre-keyed with 32 random bytes; the raw key is kept nowhere else.
    mac: Arc<HmacSha256>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Above this many tracked keys, a `check` drops elapsed windows so the map
/// cannot grow without bound.
const PRUNE_THRESHOLD: usize = 10_000;

impl RateLimiter {
    /// A fresh limiter with a fresh random hashing key, so pseudonyms do not
    /// survive a restart.
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        let mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts any key length");
        // Best-effort: don't leave a second copy of the key on the stack frame.
        key.fill(0);
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            mac: Arc::new(mac),
        }
    }

    /// The bucket for the request's client in `tier`. The identity is hashed in
    /// the same expression, so the raw IP never leaves this module.
    pub fn client_key(&self, tier: &str, headers: &HeaderMap) -> ClientKey {
        self.key_for_ip(tier, client_identity(headers))
    }

    fn key_for_ip(&self, tier: &str, ip: &str) -> ClientKey {
        let mut mac = (*self.mac).clone();
        mac.update(ip.as_bytes());
        let digest = mac.finalize().into_bytes();
        ClientKey(format!("{tier}:{}", hex::encode(&digest[..CLIENT_HASH_BYTES])))
    }

    /// A short keyed tag for a normalized email, so the dev ops log can tell a
    /// retry loop from fresh addresses without holding the address.
    /// Domain-separated from client pseudonyms.
    pub(crate) fn email_tag(&self, normalized_email: &str) -> String {
        let mut mac = (*self.mac).clone();
        mac.update(b"ops-email\0");
        mac.update(normalized_email.as_bytes());
        let digest = mac.finalize().into_bytes();
        hex::encode(&digest[..EMAIL_TAG_BYTES])
    }

    /// Record one hit for `key` and report whether it is within `max` per
    /// `window_secs` (fixed window).
    pub fn check(&self, key: &ClientKey, max: u32, window_secs: u64, now: u64) -> RateLimitOutcome {
        let mut guard = self.inner.lock().expect("rate limiter mutex poisoned");

        if guard.len() > PRUNE_THRESHOLD {
            // Each entry's own window: the caller's would let a 60s tier evict
            // live 600s OTP counters.
            guard.retain(|_, w| now.saturating_sub(w.window_start) < w.window_secs);
        }

        // Not `entry`: it takes an owned key, costing a clone per request.
        if !guard.contains_key(key) {
            guard.insert(
                key.clone(),
                Window {
                    count: 0,
                    window_start: now,
                    window_secs,
                },
            );
        }
        let win = guard.get_mut(key).expect("inserted above when absent");
        // So a re-tuned window takes effect on the next hit.
        win.window_secs = window_secs;
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

/// Set by the Worker (`worker/index.ts`) after it deletes every IP header;
/// never client-controlled.
const CLIENT_BUCKET_HEADER: &str = "x-pollis-client-bucket";

/// The Worker's bucket, else (local runs) the client IP. Its only caller hashes
/// the result immediately.
fn client_identity(headers: &HeaderMap) -> &str {
    if let Some(bucket) = header_str(headers, CLIENT_BUCKET_HEADER) {
        return bucket;
    }
    client_ip(headers)
}

/// `CF-Connecting-IP`, then the first `X-Forwarded-For` hop, else a shared
/// sentinel so the limiter is still exercised rather than bypassed.
fn client_ip(headers: &HeaderMap) -> &str {
    if let Some(ip) = header_str(headers, "cf-connecting-ip") {
        return ip;
    }
    if let Some(first) = header_str(headers, "x-forwarded-for")
        .and_then(|xff| xff.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return first;
    }
    "unknown"
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Distinct from the per-email lockout's "too many attempts", so the two limits
/// are tellable apart.
pub fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({ "error": "too many requests" })),
    )
        .into_response()
}

/// Pick the rate-limit tier for a request, or `None` to exempt it. The single
/// place throttling policy is decided.
fn classify(method: &Method, path: &str, cfg: &RateLimitConfig) -> Option<(&'static str, u32, u64)> {
    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        // A rate-limited health check reads as an outage.
        if path == "/health" || path == "/version" {
            return None;
        }
        return Some(("get", cfg.get_max, cfg.get_window_secs));
    }
    match path {
        "/v1/auth/request-otp" => Some((
            OTP_REQUEST_TIER,
            cfg.request_otp_max,
            cfg.request_otp_window_secs,
        )),
        // `/v1/link/claim` (#1207) consumes a secret pre-credential, like
        // verify-otp.
        "/v1/auth/verify-otp"
        | "/v1/auth/request-email-change-otp"
        | "/v1/auth/verify-email-change"
        | "/v1/link/claim" => {
            Some(("otp_verify", cfg.verify_otp_max, cfg.verify_otp_window_secs))
        }
        // #847; the durable per-user bound is in `groups::apply_redeem_invite_link`.
        "/v1/invite-links/redeem" => Some((
            "invite_redeem",
            cfg.invite_redeem_max,
            cfg.invite_redeem_window_secs,
        )),
        // Guessable input, not the verb, decides this tier (#987): an ungated
        // slug lookup, the pre-credential probe, and invite creation (whose
        // "no such user" answer would let any group admin walk the identifier
        // space). `/v1/link/offer` (#1361) holds memory until it lapses.
        "/v1/directory/group-by-slug"
        | "/v1/auth/account-probe"
        | "/v1/invites/create"
        | "/v1/link/offer" => {
            Some(("probe", cfg.probe_max, cfg.probe_window_secs))
        }
        // QR-link polls (#1207, #1361) are reads whatever their path family.
        "/v1/link/status" | "/v1/link/poll-offer" => Some(("read", cfg.read_max, cfg.read_window_secs)),
        p if is_read_path(p) => Some(("read", cfg.read_max, cfg.read_window_secs)),
        _ => Some(("write", cfg.write_max, cfg.write_window_secs)),
    }
}

/// Whether a path is a read endpoint (#987). Prefix-matched, so a new read lands
/// in the read tier by construction; no write lives under these prefixes.
fn is_read_path(path: &str) -> bool {
    path.starts_with("/v1/read/")
        || path.starts_with("/v1/directory/")
        || path == "/v1/conversations/catch-up"
        || path == "/v1/mls/conversation-state"
        || path == "/v1/welcomes/fetch"
        || path == "/v1/messages/lookup"
}

/// Axum middleware: per-client, per-tier rate limiting for the whole service
/// (#345); 429 when a client exceeds its tier.
pub async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if let Some((tier, max, window)) = classify(req.method(), req.uri().path(), &state.ratelimit_config)
    {
        let key = state.ratelimit.client_key(tier, req.headers());
        let limited = state
            .ratelimit
            .check(&key, max, window, crate::util::now_unix())
            == RateLimitOutcome::Limited;
        // Dev-only observation; it records but does not change the decision.
        if let Some(ops) = state.ops_log.as_ref() {
            if limited || tier == OTP_REQUEST_TIER {
                return crate::ops_log::observe(ops, &state.ratelimit, tier, &key, limited, req, next)
                    .await;
            }
        }
        if limited {
            return too_many_requests();
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tier_max_parses_as_a_number_or_off() {
        assert_eq!(parse_max("10"), Some(10));
        assert_eq!(parse_max(" off "), Some(UNLIMITED));
        assert_eq!(parse_max("OFF"), Some(UNLIMITED));
        assert_eq!(parse_max("unlimited"), None, "an unknown word falls back to the default, never to off");
        assert_eq!(parse_max(""), None);
        assert_eq!(max_json(UNLIMITED), serde_json::json!("off"));
        assert_eq!(max_json(10), serde_json::json!(10));
    }

    #[test]
    fn an_unlimited_tier_never_limits() {
        let limiter = RateLimiter::default();
        let open = limiter.key_for_ip("otp_request", "198.51.100.7");
        let capped = limiter.key_for_ip("otp_request", "198.51.100.8");
        for _ in 0..10_000 {
            assert_eq!(limiter.check(&open, UNLIMITED, 600, 1), RateLimitOutcome::Allowed);
        }
        for _ in 0..10 {
            limiter.check(&capped, 10, 600, 1);
        }
        assert_eq!(limiter.check(&capped, 10, 600, 1), RateLimitOutcome::Limited);
    }

    /// The pruner must judge each entry by its own window, not the caller's.
    #[test]
    fn a_short_window_prune_does_not_evict_a_live_long_window() {
        const OTP_WINDOW: u64 = 600;
        const WRITE_WINDOW: u64 = 60;
        let limiter = RateLimiter::default();
        let t0 = 1_000_000;
        let victim = limiter.key_for_ip("otp_request", "198.51.100.1");

        for _ in 0..5 {
            limiter.check(&victim, 5, OTP_WINDOW, t0);
        }
        assert_eq!(
            limiter.check(&victim, 5, OTP_WINDOW, t0),
            RateLimitOutcome::Limited,
            "premise: the OTP key is over its limit before anything is pruned"
        );

        for i in 0..=PRUNE_THRESHOLD {
            let filler = limiter.key_for_ip("write", &i.to_string());
            limiter.check(&filler, 10_000, WRITE_WINDOW, t0);
        }

        // The write windows have elapsed, the OTP window has not; this call prunes.
        let t1 = t0 + 90;
        let trigger = limiter.key_for_ip("write", "trigger");
        limiter.check(&trigger, 10_000, WRITE_WINDOW, t1);

        assert_eq!(
            limiter.check(&victim, 5, OTP_WINDOW, t1),
            RateLimitOutcome::Limited,
            "a throttled OTP client got its counter reset by unrelated write \
             traffic — the prune judged a 600s window by a 60s cutoff"
        );
    }

    #[test]
    fn reads_probes_and_writes_are_separate_tiers() {
        let cfg = RateLimitConfig::default();
        let tier = |p: &str| classify(&Method::POST, p, &cfg).map(|(t, _, _)| t);
        assert_eq!(tier("/v1/messages/send"), Some("write"));
        assert_eq!(tier("/v1/read/devices"), Some("read"));
        assert_eq!(tier("/v1/directory/bootstrap"), Some("read"));
        assert_eq!(tier("/v1/conversations/catch-up"), Some("read"));
        assert_eq!(tier("/v1/mls/conversation-state"), Some("read"));
        assert_eq!(tier("/v1/welcomes/fetch"), Some("read"));
        assert_eq!(tier("/v1/directory/group-by-slug"), Some("probe"));
        assert_eq!(tier("/v1/auth/account-probe"), Some("probe"));
        assert_eq!(tier("/v1/invites/create"), Some("probe"));
    }

    #[test]
    fn get_routes_are_limited_except_the_liveness_probes() {
        let cfg = RateLimitConfig::default();
        let tier = |p: &str| classify(&Method::GET, p, &cfg).map(|(t, _, _)| t);
        assert_eq!(tier("/health"), None);
        assert_eq!(tier("/version"), None);
        assert_eq!(tier("/v1/retention/metrics"), Some("get"));
        assert_eq!(tier("/v1/config"), Some("get"));
        // The limiter runs before routing, so an enumeration sweep is bounded too.
        assert_eq!(tier("/v1/commits/anything"), Some("get"));
    }

    /// Asserts the order, not the numbers, so it survives re-tuning.
    #[test]
    fn the_probe_budget_is_the_tightest_of_the_three() {
        let cfg = RateLimitConfig::default();
        let per_sec = |max: u32, win: u64| max as f64 / win as f64;
        assert!(
            per_sec(cfg.probe_max, cfg.probe_window_secs)
                < per_sec(cfg.write_max, cfg.write_window_secs)
        );
        assert!(
            per_sec(cfg.write_max, cfg.write_window_secs)
                < per_sec(cfg.read_max, cfg.read_window_secs)
        );
    }

    #[test]
    fn allows_up_to_max_then_limits() {
        let rl = RateLimiter::default();
        let k = rl.key_for_ip("t", "1.2.3.4");
        for _ in 0..3 {
            assert_eq!(rl.check(&k, 3, 60, 1000), RateLimitOutcome::Allowed);
        }
        assert_eq!(rl.check(&k, 3, 60, 1000), RateLimitOutcome::Limited);
        assert_eq!(rl.check(&k, 3, 60, 1030), RateLimitOutcome::Limited);
    }

    #[test]
    fn window_resets_after_it_elapses() {
        let rl = RateLimiter::default();
        let k = rl.key_for_ip("t", "1.2.3.4");
        for _ in 0..3 {
            rl.check(&k, 3, 60, 1000);
        }
        assert_eq!(rl.check(&k, 3, 60, 1000), RateLimitOutcome::Limited);
        assert_eq!(rl.check(&k, 3, 60, 1061), RateLimitOutcome::Allowed);
    }

    #[test]
    fn limiting_is_still_per_ip() {
        let rl = RateLimiter::default();
        let headers_for = |ip: &str| {
            let mut h = HeaderMap::new();
            h.insert("cf-connecting-ip", ip.parse().unwrap());
            h
        };
        for _ in 0..3 {
            rl.check(&rl.client_key("t", &headers_for("1.1.1.1")), 3, 60, 1000);
        }
        assert_eq!(
            rl.check(&rl.client_key("t", &headers_for("1.1.1.1")), 3, 60, 1000),
            RateLimitOutcome::Limited
        );
        assert_eq!(
            rl.check(&rl.client_key("t", &headers_for("2.2.2.2")), 3, 60, 1000),
            RateLimitOutcome::Allowed
        );
        assert_eq!(
            rl.check(&rl.client_key("u", &headers_for("1.1.1.1")), 3, 60, 1000),
            RateLimitOutcome::Allowed
        );
    }

    #[test]
    fn stored_keys_never_contain_the_ip() {
        let rl = RateLimiter::default();
        let ips = ["203.0.113.7", "2001:db8::1"];
        for ip in ips {
            let mut h = HeaderMap::new();
            h.insert("cf-connecting-ip", ip.parse().unwrap());
            rl.check(&rl.client_key("otp_request", &h), 10, 600, 1000);
        }
        rl.check(&rl.client_key("otp_request", &HeaderMap::new()), 10, 600, 1000);
        let guard = rl.inner.lock().unwrap();
        assert_eq!(guard.len(), ips.len() + 1);
        for key in guard.keys() {
            let debug = format!("{key:?}");
            for ip in ips.iter().chain(["unknown"].iter()) {
                assert!(!key.0.contains(ip), "stored key {:?} contains {ip}", key.0);
                assert!(!debug.contains(ip), "Debug of {debug} contains {ip}");
            }
            let (tier, hash) = key.0.split_once(':').expect("tier:hash");
            assert_eq!(tier, "otp_request");
            assert_eq!(hash.len(), CLIENT_HASH_BYTES * 2);
            assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn same_ip_maps_to_same_key_within_a_limiter() {
        let rl = RateLimiter::default();
        assert_eq!(rl.key_for_ip("t", "203.0.113.7"), rl.key_for_ip("t", "203.0.113.7"));
        let clone = rl.clone();
        assert_eq!(rl.key_for_ip("t", "203.0.113.7"), clone.key_for_ip("t", "203.0.113.7"));
        assert_ne!(rl.key_for_ip("t", "203.0.113.7"), rl.key_for_ip("t", "203.0.113.8"));
    }

    #[test]
    fn different_limiters_hash_the_same_ip_differently() {
        let a = RateLimiter::new();
        let b = RateLimiter::new();
        assert_ne!(a.key_for_ip("t", "203.0.113.7"), b.key_for_ip("t", "203.0.113.7"));
    }

    #[test]
    fn the_pseudonym_is_tier_independent_and_never_the_ip() {
        let rl = RateLimiter::default();
        let a = rl.key_for_ip("otp_request", "203.0.113.7");
        let b = rl.key_for_ip("write", "203.0.113.7");
        assert_eq!(a.pseudonym(), b.pseudonym());
        assert_eq!(a.pseudonym().len(), CLIENT_HASH_BYTES * 2);
        assert!(!a.pseudonym().contains("203.0.113.7"));
        assert_ne!(a.pseudonym(), rl.key_for_ip("otp_request", "203.0.113.8").pseudonym());
    }

    #[test]
    fn email_tags_are_keyed_and_stable() {
        let rl = RateLimiter::default();
        let t = rl.email_tag("alice@example.com");
        assert_eq!(t, rl.email_tag("alice@example.com"));
        assert_ne!(t, rl.email_tag("bob@example.com"));
        assert_eq!(t.len(), EMAIL_TAG_BYTES * 2);
        assert!(!t.contains("alice"));
        assert_ne!(t, RateLimiter::new().email_tag("alice@example.com"));
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

    #[test]
    fn the_worker_bucket_is_the_identity_when_present() {
        let mut h = HeaderMap::new();
        h.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());
        assert_eq!(client_identity(&h), "203.0.113.7");
        h.insert(CLIENT_BUCKET_HEADER, "0123456789abcdef0123456789abcdef".parse().unwrap());
        assert_eq!(client_identity(&h), "0123456789abcdef0123456789abcdef");

        let rl = RateLimiter::default();
        let bucket = |b: &str| {
            let mut h = HeaderMap::new();
            h.insert(CLIENT_BUCKET_HEADER, b.parse().unwrap());
            h
        };
        rl.check(&rl.client_key("t", &bucket("aa")), 1, 60, 1000);
        assert_eq!(rl.check(&rl.client_key("t", &bucket("aa")), 1, 60, 1000), RateLimitOutcome::Limited);
        assert_eq!(rl.check(&rl.client_key("t", &bucket("bb")), 1, 60, 1000), RateLimitOutcome::Allowed);
    }
}
