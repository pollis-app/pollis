//! Per-client-IP rate limiting for the unauthenticated signup-OTP endpoints
//! (`request-otp` / `verify-otp`).
//!
//! Per-email limits in [`crate::otp`] can't stop one client spraying many addresses; this is per IP.
//!
//! **Store:** in-memory fixed windows (single container), keyed by tier plus client identity.
//!
//! **Client identity:** the Worker's `X-Pollis-Client-Bucket`; without the Worker, the client IP.
//!
//! **The IP is never a key:** the map holds a per-process HMAC, via [`RateLimiter::client_key`] only.

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
    /// Max other (authenticated) write calls per IP per window; a flood backstop.
    pub write_max: u32,
    /// Authenticated-write window length, seconds.
    pub write_window_secs: u64,
    /// Max invite-link redemption attempts per IP per window (#847).
    pub invite_redeem_max: u32,
    /// Invite-link redemption window length, seconds.
    pub invite_redeem_window_secs: u64,
    /// Max READ calls per IP per window (#987); reads are POSTs, so they'd otherwise spend `write`.
    pub read_max: u32,
    /// Read window length, seconds.
    pub read_window_secs: u64,
    /// Max slug lookups and account probes per IP per window (#987): guessable inputs.
    pub probe_max: u32,
    /// Probe window length, seconds.
    pub probe_window_secs: u64,
    /// Max GET calls per IP per window; `/health` and `/version` are exempt, the rest is bearer-gated.
    pub get_max: u32,
    /// GET window length, seconds.
    pub get_window_secs: u64,
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
            write_max: 1200,
            write_window_secs: 60,
            // A real user redeems a link once; 20 per 10 minutes covers typos.
            invite_redeem_max: 20,
            invite_redeem_window_secs: 600,
            // Far above a cold launch's reads, far below a scrape.
            read_max: 3000,
            read_window_secs: 60,
            // Covers retries and multi-account installs without resembling a search.
            probe_max: 60,
            probe_window_secs: 600,
            // Far above a metrics scraper, far below a token search.
            get_max: 120,
            get_window_secs: 600,
        }
    }
}

/// A tier max that never limits: [`RateLimiter::check`] saturates its counter
/// at `u32::MAX` and limits only when the count EXCEEDS the max.
pub const UNLIMITED: u32 = u32::MAX;

impl RateLimitConfig {
    /// Build from DS environment, falling back to [`Default`] per field. Every
    /// tier is tunable: `RL_{REQUEST_OTP,VERIFY_OTP,WRITE,READ,PROBE,GET,
    /// INVITE_REDEEM}_{MAX,WINDOW_SECS}`.
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

/// A tier's `RL_*_MAX`: a number, or `off` for [`UNLIMITED`] (dev's OTP tiers).
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

/// The outcome of a rate-limit check.
#[derive(Debug, PartialEq, Eq)]
pub enum RateLimitOutcome {
    Allowed,
    /// The client exceeded `max` in the current window → the caller should 429.
    Limited,
}

/// One key's counter within the current fixed window.
struct Window {
    count: u32,
    window_start: u64,
    /// The window length this key is counted over, carried on the entry.
    ///
    /// One map serves every tier, so the pruner judges each entry by its own window.
    window_secs: u64,
}

type HmacSha256 = Hmac<Sha256>;

/// Bytes of the HMAC output kept in a [`ClientKey`] (128 bits).
const CLIENT_HASH_BYTES: usize = 16;

/// Bytes of the HMAC kept in an ops-log email tag ([`RateLimiter::email_tag`]).
/// 64 bits is plenty to tell a few thousand addresses apart within one process.
const EMAIL_TAG_BYTES: usize = 8;

/// The `request-otp` tier name, shared with the dev ops log.
pub(crate) const OTP_REQUEST_TIER: &str = "otp_request";

/// A rate-limit bucket: a tier name plus a keyed, per-process hash of the client
/// IP — `{tier}:{hex(HMAC-SHA256(process_key, ip)[..16])}`.
///
/// The only constructor is [`RateLimiter::client_key`], so a raw IP can't get in.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClientKey(String);

impl ClientKey {
    /// The pseudonym half of the key; the same in every tier, so the ops log can correlate.
    pub(crate) fn pseudonym(&self) -> &str {
        self.0.split_once(':').map_or(self.0.as_str(), |(_, h)| h)
    }
}

/// In-memory per-key fixed-window rate limiter; clones share the key and the map.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<ClientKey, Window>>>,
    /// HMAC-SHA256 keyed with this limiter's 32 random bytes, kept nowhere else.
    mac: Arc<HmacSha256>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Above this many tracked keys, a `check` opportunistically drops windows whose
/// span has fully elapsed, so an ever-changing IP set can't grow the map without
/// bound on a long-lived container.
const PRUNE_THRESHOLD: usize = 10_000;

impl RateLimiter {
    /// A fresh limiter with a fresh random key, so hashes differ across limiters and restarts.
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        // HMAC accepts a key of any length; 32 bytes cannot fail.
        let mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts any key length");
        // Best-effort: don't leave a second copy of the key on the stack frame.
        key.fill(0);
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            mac: Arc::new(mac),
        }
    }

    /// The bucket for the request's client in `tier`; the IP is hashed in the same expression.
    pub fn client_key(&self, tier: &str, headers: &HeaderMap) -> ClientKey {
        self.key_for_ip(tier, client_identity(headers))
    }

    /// Hash `ip` into a [`ClientKey`]. Private: callers outside this module go
    /// through [`Self::client_key`], which takes headers, not an IP.
    fn key_for_ip(&self, tier: &str, ip: &str) -> ClientKey {
        let mut mac = (*self.mac).clone();
        mac.update(ip.as_bytes());
        let digest = mac.finalize().into_bytes();
        ClientKey(format!("{tier}:{}", hex::encode(&digest[..CLIENT_HASH_BYTES])))
    }

    /// A short keyed tag for a normalized email (ops log), domain-separated from client pseudonyms.
    pub(crate) fn email_tag(&self, normalized_email: &str) -> String {
        let mut mac = (*self.mac).clone();
        mac.update(b"ops-email\0");
        mac.update(normalized_email.as_bytes());
        let digest = mac.finalize().into_bytes();
        hex::encode(&digest[..EMAIL_TAG_BYTES])
    }

    /// Record one hit for `key`; fixed window, so an over-limit key stays [`Limited`] until rollover.
    ///
    /// [`Limited`]: RateLimitOutcome::Limited
    pub fn check(&self, key: &ClientKey, max: u32, window_secs: u64, now: u64) -> RateLimitOutcome {
        let mut guard = self.inner.lock().expect("rate limiter mutex poisoned");

        if guard.len() > PRUNE_THRESHOLD {
            // Judge every entry by ITS OWN window, not the caller's.
            guard.retain(|_, w| now.saturating_sub(w.window_start) < w.window_secs);
        }

        // Probed before `entry` to avoid cloning a `ClientKey` on every request.
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
        // Keep the stored span in step, so a re-tuned limit applies on the next hit.
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

/// The Worker's per-client bucket header (`worker/index.ts`). Set by the
/// Durable Object after it deletes every IP header; never client-controlled.
const CLIENT_BUCKET_HEADER: &str = "x-pollis-client-bucket";

/// The client identity for keying: the Worker's bucket, else the IP. Its only caller hashes it.
fn client_identity(headers: &HeaderMap) -> &str {
    if let Some(bucket) = header_str(headers, CLIENT_BUCKET_HEADER) {
        return bucket;
    }
    client_ip(headers)
}

/// The client IP without the Worker: `CF-Connecting-IP`, the first XFF hop, else a sentinel.
fn client_ip(headers: &HeaderMap) -> &str {
    if let Some(ip) = header_str(headers, "cf-connecting-ip") {
        return ip;
    }
    // `X-Forwarded-For: client, proxy1, proxy2` — the first hop is the client.
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

/// The 429 body for the per-IP throttle — distinct from the per-email lockout's
/// "too many attempts" so the two limits are tellable apart in logs/clients.
pub fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({ "error": "too many requests" })),
    )
        .into_response()
}

/// Pick the rate-limit tier for a request, or `None` to exempt it; the one place policy lives.
fn classify(method: &Method, path: &str, cfg: &RateLimitConfig) -> Option<(&'static str, u32, u64)> {
    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        // Liveness probes stay exempt: a throttled health check reads as an outage.
        if path == "/health" || path == "/version" {
            return None;
        }
        // Other GETs are bearer-token operator surface, so they get an attempt bound.
        return Some(("get", cfg.get_max, cfg.get_window_secs));
    }
    match path {
        "/v1/auth/request-otp" => Some((
            OTP_REQUEST_TIER,
            cfg.request_otp_max,
            cfg.request_otp_window_secs,
        )),
        // `/v1/link/claim` (#1207) consumes a guessable-in-principle secret
        // pre-credential, exactly like verify-otp, so it shares that tier.
        "/v1/auth/verify-otp"
        | "/v1/auth/request-email-change-otp"
        | "/v1/auth/verify-email-change"
        | "/v1/link/claim" => {
            Some(("otp_verify", cfg.verify_otp_max, cfg.verify_otp_window_secs))
        }
        // #847: sheds volume; the durable per-USER bound is `groups::apply_redeem_invite_link`.
        "/v1/invite-links/redeem" => Some((
            "invite_redeem",
            cfg.invite_redeem_max,
            cfg.invite_redeem_window_secs,
        )),
        // #987: guessable-input reads (`group-by-slug`, `account-probe`) need a guessing bound.
        // `/v1/invites/create` too: its username-or-email input is guessable.
        // `/v1/link/offer` (#1361): pre-credential, and holds memory until it lapses.
        "/v1/directory/group-by-slug"
        | "/v1/auth/account-probe"
        | "/v1/invites/create"
        | "/v1/link/offer" => {
            Some(("probe", cfg.probe_max, cfg.probe_window_secs))
        }
        // Every other read (#987); a cold launch issues far more reads than writes.
        // `/v1/link/status` (#1207) is a QR poll, so a read.
        // `/v1/link/poll-offer` (#1361): a 32-byte secret, so the bound is volume.
        "/v1/link/status" | "/v1/link/poll-offer" => Some(("read", cfg.read_max, cfg.read_window_secs)),
        p if is_read_path(p) => Some(("read", cfg.read_max, cfg.read_window_secs)),
        _ => Some(("write", cfg.write_max, cfg.write_window_secs)),
    }
}

/// Whether a path is one of the #987 read endpoints.
///
/// Prefix-matched, so a new read lands here by construction and a write cannot.
fn is_read_path(path: &str) -> bool {
    path.starts_with("/v1/read/")
        || path.starts_with("/v1/directory/")
        || path == "/v1/conversations/catch-up"
        || path == "/v1/mls/conversation-state"
        || path == "/v1/welcomes/fetch"
        || path == "/v1/messages/lookup"
}

/// Axum middleware: per-IP rate limiting for the whole service, keyed by [`ClientKey`] (#345).
pub async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if let Some((tier, max, window)) = classify(req.method(), req.uri().path(), &state.ratelimit_config)
    {
        let key = state.ratelimit.client_key(tier, req.headers());
        let limited = state
            .ratelimit
            .check(&key, max, window, crate::util::now_unix())
            == RateLimitOutcome::Limited;
        // Dev-only observation (`ops_log.rs`); it only RECORDS the decision above.
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

    /// `off` never limits, while the same traffic against a numeric max does.
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

    /// The pruner must judge each entry by its OWN window.
    #[test]
    fn a_short_window_prune_does_not_evict_a_live_long_window() {
        const OTP_WINDOW: u64 = 600;
        const WRITE_WINDOW: u64 = 60;
        let limiter = RateLimiter::default();
        let t0 = 1_000_000;
        let victim = limiter.key_for_ip("otp_request", "198.51.100.1");

        // An OTP client burns its budget and is now Limited.
        for _ in 0..5 {
            limiter.check(&victim, 5, OTP_WINDOW, t0);
        }
        assert_eq!(
            limiter.check(&victim, 5, OTP_WINDOW, t0),
            RateLimitOutcome::Limited,
            "premise: the OTP key is over its limit before anything is pruned"
        );

        // Unrelated write traffic fills the shared map past the prune threshold.
        for i in 0..=PRUNE_THRESHOLD {
            let filler = limiter.key_for_ip("write", &i.to_string());
            limiter.check(&filler, 10_000, WRITE_WINDOW, t0);
        }

        // 90s later the write windows have elapsed but the OTP window has NOT.
        // This write call is what triggers the prune.
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

    /// Reads must not spend the write budget, and the two guessable-input ones
    /// must not spend the read budget (#987).
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
        // A guessable-input WRITE belongs in the probe tier too (L3).
        assert_eq!(tier("/v1/invites/create"), Some("probe"));
    }

    /// GETs are no longer exempt wholesale.
    ///
    /// What's left over GET is behind a STATIC bearer token; the liveness probes stay exempt.
    #[test]
    fn get_routes_are_limited_except_the_liveness_probes() {
        let cfg = RateLimitConfig::default();
        let tier = |p: &str| classify(&Method::GET, p, &cfg).map(|(t, _, _)| t);
        assert_eq!(tier("/health"), None);
        assert_eq!(tier("/version"), None);
        assert_eq!(tier("/v1/retention/metrics"), Some("get"));
        assert_eq!(tier("/v1/config"), Some("get"));
        // A path that no longer routes still gets a tier — the limiter runs
        // before routing, so an enumeration sweep is bounded too.
        assert_eq!(tier("/v1/commits/anything"), Some("get"));
    }

    /// The probe tier is tighter than the read tier, which is looser than the
    /// write tier. Asserting the ORDER rather than the numbers keeps the point
    /// (guessable input gets the smallest budget) true through re-tuning.
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
        // max = 3 per 60s.
        for _ in 0..3 {
            assert_eq!(rl.check(&k, 3, 60, 1000), RateLimitOutcome::Allowed);
        }
        assert_eq!(rl.check(&k, 3, 60, 1000), RateLimitOutcome::Limited);
        // Still limited later in the same window.
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
        // A full window later, the counter resets.
        assert_eq!(rl.check(&k, 3, 60, 1061), RateLimitOutcome::Allowed);
    }

    /// Limiting is still per IP after hashing.
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
        // A different IP has its own fresh window.
        assert_eq!(
            rl.check(&rl.client_key("t", &headers_for("2.2.2.2")), 3, 60, 1000),
            RateLimitOutcome::Allowed
        );
        // And the same IP in a different tier has its own budget too.
        assert_eq!(
            rl.check(&rl.client_key("u", &headers_for("1.1.1.1")), 3, 60, 1000),
            RateLimitOutcome::Allowed
        );
    }

    /// No plain IP is ever readable from the limiter's memory, nor from a key's `Debug`.
    #[test]
    fn stored_keys_never_contain_the_ip() {
        let rl = RateLimiter::default();
        let ips = ["203.0.113.7", "2001:db8::1"];
        for ip in ips {
            let mut h = HeaderMap::new();
            h.insert("cf-connecting-ip", ip.parse().unwrap());
            rl.check(&rl.client_key("otp_request", &h), 10, 600, 1000);
        }
        // No header at all → the shared sentinel, which must be hashed too.
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

    /// Within one process the mapping is stable, or the limiter could not count.
    #[test]
    fn same_ip_maps_to_same_key_within_a_limiter() {
        let rl = RateLimiter::default();
        assert_eq!(rl.key_for_ip("t", "203.0.113.7"), rl.key_for_ip("t", "203.0.113.7"));
        // A clone (every `AppState` clone) shares the key, so requests handled
        // through different clones still count into the same bucket.
        let clone = rl.clone();
        assert_eq!(rl.key_for_ip("t", "203.0.113.7"), clone.key_for_ip("t", "203.0.113.7"));
        assert_ne!(rl.key_for_ip("t", "203.0.113.7"), rl.key_for_ip("t", "203.0.113.8"));
    }

    /// The hashing key is per instance: one IP hashes differently in two limiters.
    #[test]
    fn different_limiters_hash_the_same_ip_differently() {
        let a = RateLimiter::new();
        let b = RateLimiter::new();
        assert_ne!(a.key_for_ip("t", "203.0.113.7"), b.key_for_ip("t", "203.0.113.7"));
    }

    /// The ops-log pseudonym is the stored key minus its tier, so one client
    /// correlates across tiers and nothing new is derived from the IP.
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

    /// Email tags are stable within a limiter, differ per address and per
    /// process, and never contain the address.
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

    /// Behind the Worker only the bucket arrives, and it wins over any IP header.
    #[test]
    fn the_worker_bucket_is_the_identity_when_present() {
        let mut h = HeaderMap::new();
        h.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());
        assert_eq!(client_identity(&h), "203.0.113.7");
        h.insert(CLIENT_BUCKET_HEADER, "0123456789abcdef0123456789abcdef".parse().unwrap());
        assert_eq!(client_identity(&h), "0123456789abcdef0123456789abcdef");

        // Two clients with distinct buckets are limited independently.
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
