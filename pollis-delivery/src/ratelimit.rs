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
    /// Max other (authenticated) write calls per IP per window — a generous
    /// backstop against a flood from one client; device-signed writes are already
    /// credential-gated, so this only catches egregious volume.
    pub write_max: u32,
    /// Authenticated-write window length, seconds.
    pub write_window_secs: u64,
    /// Max invite-link redemption attempts per IP per window (#847). The
    /// generic `write` tier is 1200/60s — fine as a flood backstop, useless as a
    /// brute-force bound on a join code, so redemption gets its own tier.
    pub invite_redeem_max: u32,
    /// Invite-link redemption window length, seconds.
    pub invite_redeem_window_secs: u64,
    /// Max READ calls per IP per window (#987). Every read is a POST — the
    /// canonical signing message excludes the query string, so a signed GET's
    /// parameters would be unauthenticated — which means reads would otherwise
    /// spend the `write` budget. A cold launch legitimately issues dozens of
    /// them in a second, so reads get their own, larger allowance rather than
    /// competing with sends for one.
    pub read_max: u32,
    /// Read window length, seconds.
    pub read_window_secs: u64,
    /// Max slug lookups and account probes per IP per window (#987). These two
    /// are the only endpoints whose INPUT is guessable — a group name and, for
    /// an unauthenticated caller, nothing at all — so the generic backstop is
    /// the wrong bound for them, exactly as it was for invite redemption.
    pub probe_max: u32,
    /// Probe window length, seconds.
    pub probe_window_secs: u64,
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
            // A real user redeems a link once. 20 attempts per 10 minutes from
            // one IP is generous for retries and typos, and far below anything
            // that resembles a search.
            invite_redeem_max: 20,
            invite_redeem_window_secs: 600,
            // A cold launch is one bootstrap plus a conversation-state batch
            // plus a catch-up per open conversation; a busy session adds a few
            // per interaction. 3000/60s is far above that and far below a scrape.
            read_max: 3000,
            read_window_secs: 60,
            // A real user looks up a handful of slugs, and probes their own
            // account id once per launch. 60 per 10 minutes covers retries and
            // multi-account installs without resembling a search.
            probe_max: 60,
            probe_window_secs: 600,
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
        if let Some(v) = env_u32("RL_WRITE_MAX") {
            cfg.write_max = v;
        }
        if let Some(v) = env_u64("RL_WRITE_WINDOW_SECS") {
            cfg.write_window_secs = v;
        }
        if let Some(v) = env_u32("RL_READ_MAX") {
            cfg.read_max = v;
        }
        if let Some(v) = env_u64("RL_READ_WINDOW_SECS") {
            cfg.read_window_secs = v;
        }
        if let Some(v) = env_u32("RL_PROBE_MAX") {
            cfg.probe_max = v;
        }
        if let Some(v) = env_u64("RL_PROBE_WINDOW_SECS") {
            cfg.probe_window_secs = v;
        }
        if let Some(v) = env_u32("RL_INVITE_REDEEM_MAX") {
            cfg.invite_redeem_max = v;
        }
        if let Some(v) = env_u64("RL_INVITE_REDEEM_WINDOW_SECS") {
            cfg.invite_redeem_window_secs = v;
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

/// One key's counter within the current fixed window.
struct Window {
    count: u32,
    window_start: u64,
    /// The window length this key is counted over, carried on the entry.
    ///
    /// One map serves all six tiers and their windows differ by an order of
    /// magnitude (60s for `write`/`read`, 600s for the OTP, invite-redeem and
    /// probe tiers), so the pruner cannot use the calling tier's window to judge
    /// somebody else's entry — see [`RateLimiter::check`].
    window_secs: u64,
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
            // Judge every entry by ITS OWN window, not the caller's.
            //
            // This used to prune with `window_secs` — the window of whichever
            // tier happened to trip the threshold. One map holds all six tiers,
            // so a `write` call (60s) would evict live 600s entries: an OTP
            // attempt counter that was 90 seconds into its ten-minute window
            // simply vanished, and the next attempt started from zero. That is a
            // rate-limit reset on the brute-force-sensitive tier, triggered by
            // unrelated traffic on a busy server.
            guard.retain(|_, w| now.saturating_sub(w.window_start) < w.window_secs);
        }

        let win = guard.entry(key.to_string()).or_insert(Window {
            count: 0,
            window_start: now,
            window_secs,
        });
        // A key's tier is fixed by its call site, but keep the stored span in
        // step with the caller so a re-tuned limit takes effect on the next hit
        // rather than at the next eviction.
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

/// The 429 body for the per-IP throttle — distinct from the per-email lockout's
/// "too many attempts" so the two limits are tellable apart in logs/clients.
pub fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({ "error": "too many requests" })),
    )
        .into_response()
}

/// Pick the rate-limit tier for a request, or `None` to exempt it. One place
/// decides policy for the whole service, so no handler re-implements throttling.
/// Reads (`GET`) and the health/version probes are exempt (cheap, idempotent,
/// and DDoS-fronted by Cloudflare); the unauthenticated OTP endpoints get tight
/// limits (the cheap-abuse surface); every other write gets a generous backstop.
fn classify(method: &Method, path: &str, cfg: &RateLimitConfig) -> Option<(&'static str, u32, u64)> {
    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        return None;
    }
    match path {
        "/v1/auth/request-otp" => Some((
            "otp_request",
            cfg.request_otp_max,
            cfg.request_otp_window_secs,
        )),
        "/v1/auth/verify-otp"
        | "/v1/auth/request-email-change-otp"
        | "/v1/auth/verify-email-change" => {
            Some(("otp_verify", cfg.verify_otp_max, cfg.verify_otp_window_secs))
        }
        // #847 — its own tier, keyed per IP. The durable per-USER bound lives in
        // `groups::apply_redeem_invite_link`; this one sheds volume, that one
        // survives a restart and an IP rotation.
        "/v1/invite-links/redeem" => Some((
            "invite_redeem",
            cfg.invite_redeem_max,
            cfg.invite_redeem_window_secs,
        )),
        // #987 — the two guessable-input reads. `group-by-slug` is deliberately
        // not membership-gated (gating it would remove the join flow, not
        // tighten it), and `account-probe` is deliberately unauthenticated (it
        // runs before any credential exists). Both therefore need a bound that
        // is about GUESSING, which the generic write backstop is not.
        "/v1/directory/group-by-slug" | "/v1/auth/account-probe" => {
            Some(("probe", cfg.probe_max, cfg.probe_window_secs))
        }
        // Every other read (#987). They are POSTs, so without this they would
        // spend the write budget — and a cold launch issues far more reads than
        // a user ever issues writes.
        p if is_read_path(p) => Some(("read", cfg.read_max, cfg.read_window_secs)),
        _ => Some(("write", cfg.write_max, cfg.write_window_secs)),
    }
}

/// Whether a path is one of the #987 read endpoints.
///
/// Prefix-matched on the three families the reads live under, so a new read
/// endpoint lands in the read tier by construction rather than by remembering to
/// list it here — and a new WRITE cannot accidentally land there, because writes
/// do not live under these prefixes.
fn is_read_path(path: &str) -> bool {
    path.starts_with("/v1/read/")
        || path.starts_with("/v1/directory/")
        || path == "/v1/conversations/catch-up"
        || path == "/v1/mls/conversation-state"
        || path == "/v1/welcomes/fetch"
        || path == "/v1/messages/lookup"
}

/// Axum middleware: per-IP rate limiting for the whole service, keyed by
/// `{tier}:{ip}` so each tier has its own budget. Runs before the handler and
/// short-circuits to 429 when a client exceeds its tier. Replaces per-handler
/// checks so throttling lives in exactly one place (#345).
pub async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if let Some((tier, max, window)) = classify(req.method(), req.uri().path(), &state.ratelimit_config)
    {
        let ip = client_ip(req.headers());
        if state
            .ratelimit
            .check(&format!("{tier}:{ip}"), max, window, crate::util::now_unix())
            == RateLimitOutcome::Limited
        {
            return too_many_requests();
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pruner must judge each entry by its OWN window.
    ///
    /// One map serves all six tiers, and their windows differ by an order of
    /// magnitude. The prune used the CALLING tier's window, so a `write` call
    /// (60s) crossing the threshold evicted live 600s entries — an OTP attempt
    /// counter part-way through its ten-minute window vanished, and the next
    /// attempt started from zero. A rate-limit reset on the brute-force tier,
    /// caused by unrelated traffic.
    ///
    /// Reproduced here at the smallest scale that trips it: an OTP key counted
    /// to its limit, the map pushed past `PRUNE_THRESHOLD` with filler, then a
    /// short-window call to force the prune.
    #[test]
    fn a_short_window_prune_does_not_evict_a_live_long_window() {
        const OTP_WINDOW: u64 = 600;
        const WRITE_WINDOW: u64 = 60;
        let limiter = RateLimiter::default();
        let t0 = 1_000_000;

        // An OTP client burns its budget and is now Limited.
        for _ in 0..5 {
            limiter.check("otp:victim", 5, OTP_WINDOW, t0);
        }
        assert_eq!(
            limiter.check("otp:victim", 5, OTP_WINDOW, t0),
            RateLimitOutcome::Limited,
            "premise: the OTP key is over its limit before anything is pruned"
        );

        // Unrelated write traffic fills the shared map past the prune threshold.
        for i in 0..=PRUNE_THRESHOLD {
            limiter.check(&format!("write:{i}"), 10_000, WRITE_WINDOW, t0);
        }

        // 90s later the write windows have elapsed but the OTP window has NOT.
        // This write call is what triggers the prune.
        let t1 = t0 + 90;
        limiter.check("write:trigger", 10_000, WRITE_WINDOW, t1);

        assert_eq!(
            limiter.check("otp:victim", 5, OTP_WINDOW, t1),
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
