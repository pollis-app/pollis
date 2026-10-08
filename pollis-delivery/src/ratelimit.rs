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
//! OTP endpoints — `check` is keyed by a tier plus a client identity.
//!
//! **Client identity:** the DS sits behind Cloudflare and serves plain HTTP, so
//! the socket peer is the proxy, not the client. In production the container
//! never sees a client IP at all: the Worker's Durable Object
//! (`worker/index.ts`) deletes every IP-bearing header and sends
//! `X-Pollis-Client-Bucket` — a keyed hash of the IP under a key only it holds —
//! which a client cannot set (the object deletes any inbound copy). Without the
//! Worker (local runs, tests) the DS falls back to `CF-Connecting-IP`, then the
//! first `X-Forwarded-For` hop; requests with none of these share one bucket so
//! the limiter is still exercised rather than silently disabled.
//!
//! **The IP is never a key.** The limiter needs to tell clients APART, not to
//! know who they are, so the map holds `{tier}:{h}` where `h` is the first 16
//! bytes of HMAC-SHA256 over the client identity above (the Worker's bucket, or
//! in a local run the IP) under a 32-byte key drawn from the OS CSPRNG when the
//! [`RateLimiter`] is built. That key lives only in this process's
//! memory — it is not logged, persisted or configurable — so a memory dump or a
//! debug print of the map yields per-process pseudonyms that cannot be reversed
//! by enumerating the IPv4 space, and that stop meaning anything at the next
//! restart. [`ClientKey`] is the only thing [`RateLimiter::check`] accepts, and
//! the only way to build one is [`RateLimiter::client_key`], which hashes — so a
//! raw IP cannot reach the map by construction.

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
    /// Max GET calls per IP per window. `/health` and `/version` are exempt (a
    /// load balancer and the deploy tripwire poll them, and an unreachable
    /// health check is an outage); everything else answered over GET is an
    /// operator endpoint behind a bearer token, and a bearer token is a thing
    /// that gets GUESSED. GETs used to be exempt wholesale, which was defensible
    /// only while the one substantial GET was an open read; there is no longer
    /// one of those.
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
            // An operator scraper polls metrics on the order of once a minute
            // and a human reads /v1/config by hand. 120 per 10 minutes is far
            // above both and far below a token search.
            get_max: 120,
            get_window_secs: 600,
        }
    }
}

impl RateLimitConfig {
    /// Build from DS environment, falling back to [`Default`] per field. Every
    /// tier is tunable: `RL_{REQUEST_OTP,VERIFY_OTP,WRITE,READ,PROBE,GET,
    /// INVITE_REDEEM}_{MAX,WINDOW_SECS}`.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Some(v) = env_parse::<u32>("RL_REQUEST_OTP_MAX") {
            cfg.request_otp_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_REQUEST_OTP_WINDOW_SECS") {
            cfg.request_otp_window_secs = v;
        }
        if let Some(v) = env_parse::<u32>("RL_VERIFY_OTP_MAX") {
            cfg.verify_otp_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_VERIFY_OTP_WINDOW_SECS") {
            cfg.verify_otp_window_secs = v;
        }
        if let Some(v) = env_parse::<u32>("RL_GET_MAX") {
            cfg.get_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_GET_WINDOW_SECS") {
            cfg.get_window_secs = v;
        }
        if let Some(v) = env_parse::<u32>("RL_WRITE_MAX") {
            cfg.write_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_WRITE_WINDOW_SECS") {
            cfg.write_window_secs = v;
        }
        if let Some(v) = env_parse::<u32>("RL_READ_MAX") {
            cfg.read_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_READ_WINDOW_SECS") {
            cfg.read_window_secs = v;
        }
        if let Some(v) = env_parse::<u32>("RL_PROBE_MAX") {
            cfg.probe_max = v;
        }
        if let Some(v) = env_parse::<u64>("RL_PROBE_WINDOW_SECS") {
            cfg.probe_window_secs = v;
        }
        if let Some(v) = env_parse::<u32>("RL_INVITE_REDEEM_MAX") {
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

type HmacSha256 = Hmac<Sha256>;

/// Bytes of the HMAC output kept in a [`ClientKey`]. 128 bits makes a collision
/// between two clients (which would merely share a budget) negligible while
/// halving what each entry stores.
const CLIENT_HASH_BYTES: usize = 16;

/// A rate-limit bucket: a tier name plus a keyed, per-process hash of the client
/// IP — `{tier}:{hex(HMAC-SHA256(process_key, ip)[..16])}`.
///
/// The field is private and the only constructor is [`RateLimiter::client_key`],
/// so a `ClientKey` holding a raw IP cannot exist. `Debug` is safe to derive for
/// the same reason: there is nothing in here but the tier and the pseudonym.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClientKey(String);

/// In-memory per-key fixed-window rate limiter. `Clone` is shallow (shared
/// `Arc`s) so it rides on the `Clone` `AppState`, and every clone hashes with the
/// same process key and counts into the same map.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<ClientKey, Window>>>,
    /// HMAC-SHA256 already keyed with this limiter's 32 random bytes. The raw
    /// key is not kept anywhere else; cloning the keyed state per lookup avoids
    /// re-running the key schedule on every request.
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
    /// A fresh limiter with an empty map and a fresh random hashing key. Each
    /// instance's key is independent, so the same IP hashes differently in two
    /// limiters (and therefore across restarts).
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

    /// The bucket for the request's client in `tier`. The public way to build a
    /// [`ClientKey`]: the client IP is read from `headers` and hashed in the same
    /// expression, so the raw IP never leaves this module — not to the
    /// middleware, not to a handler, not to a log line.
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

    /// Record one hit for `key` and report whether it is within `max` per
    /// `window_secs`. Fixed window: the first hit starts a window; once the
    /// window elapses the counter resets. A key over its limit stays [`Limited`]
    /// until its window rolls over.
    ///
    /// [`Limited`]: RateLimitOutcome::Limited
    pub fn check(&self, key: &ClientKey, max: u32, window_secs: u64, now: u64) -> RateLimitOutcome {
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

        // Probed before `entry`, deliberately: `entry` takes an OWNED key, so
        // every request would clone a `ClientKey` the map almost always already
        // holds, and then drop it. Two hash lookups on the hit
        // path are cheaper than one heap allocation on every request the DS
        // serves.
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

/// The Worker's per-client bucket header (`worker/index.ts`). Set by the
/// Durable Object after it deletes every IP header; never client-controlled.
const CLIENT_BUCKET_HEADER: &str = "x-pollis-client-bucket";

/// The client identity for rate-limit keying — PRIVATE, and its only caller
/// hashes the result immediately ([`RateLimiter::client_key`]). Prefers the
/// Worker's bucket (production: the only identity the container receives), and
/// only without it — a local run with no Worker in front — the client IP.
fn client_identity(headers: &HeaderMap) -> &str {
    if let Some(bucket) = header_str(headers, CLIENT_BUCKET_HEADER) {
        return bucket;
    }
    client_ip(headers)
}

/// The client IP, for a DS running without the Worker in front. Prefers
/// `CF-Connecting-IP`, then the first `X-Forwarded-For` hop. Absent both,
/// returns a shared sentinel so the limiter is still exercised rather than
/// bypassed.
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

/// Pick the rate-limit tier for a request, or `None` to exempt it. One place
/// decides policy for the whole service, so no handler re-implements throttling.
/// Reads (`GET`) and the health/version probes are exempt (cheap, idempotent,
/// and DDoS-fronted by Cloudflare); the unauthenticated OTP endpoints get tight
/// limits (the cheap-abuse surface); every other write gets a generous backstop.
fn classify(method: &Method, path: &str, cfg: &RateLimitConfig) -> Option<(&'static str, u32, u64)> {
    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        // The liveness probes stay exempt: a rate-limited health check reads as
        // an outage to whatever is watching it, and neither answer discloses
        // anything worth grinding for.
        if path == "/health" || path == "/version" {
            return None;
        }
        // Everything else served over GET is operator surface gated by a static
        // bearer token (`/v1/config`, `/v1/retention/metrics`). A static secret
        // with no bound on attempts is a secret being guessed, so the GET verb
        // gets a tier rather than a blanket exemption.
        return Some(("get", cfg.get_max, cfg.get_window_secs));
    }
    match path {
        "/v1/auth/request-otp" => Some((
            "otp_request",
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
        //
        // `/v1/invites/create` joins them: it is a WRITE, but its input is a
        // username-or-email typed by a human and its answer distinguishes "no
        // such user" from every other outcome. Anyone can make a group and be its
        // admin, so any account could walk the username and email space at the
        // generic write rate. The guessable input, not the verb, is what decides
        // the tier.
        "/v1/directory/group-by-slug" | "/v1/auth/account-probe" | "/v1/invites/create" => {
            Some(("probe", cfg.probe_max, cfg.probe_window_secs))
        }
        // Every other read (#987). They are POSTs, so without this they would
        // spend the write budget — and a cold launch issues far more reads than
        // a user ever issues writes.
        // `/v1/link/status` is polled by the device showing a QR (#1207): a
        // read, not a write, whatever its path family.
        "/v1/link/status" => Some(("read", cfg.read_max, cfg.read_window_secs)),
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
/// `{tier}:{keyed hash of ip}` (see [`ClientKey`]) so each tier has its own
/// budget and the raw IP is never stored. Runs before the handler and
/// short-circuits to 429 when a client exceeds its tier. Replaces per-handler
/// checks so throttling lives in exactly one place (#345).
pub async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if let Some((tier, max, window)) = classify(req.method(), req.uri().path(), &state.ratelimit_config)
    {
        let key = state.ratelimit.client_key(tier, req.headers());
        if state
            .ratelimit
            .check(&key, max, window, crate::util::now_unix())
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
        // A WRITE whose input is guessable belongs in the probe tier too: the
        // invite resolver takes a username-or-email and answers "no such user"
        // distinguishably, and anybody can be an admin of a group they made, so
        // at the write rate it walked the identifier space (L3).
        assert_eq!(tier("/v1/invites/create"), Some("probe"));
    }

    /// GETs are no longer exempt wholesale.
    ///
    /// The exemption was written when the one substantial GET was an open,
    /// side-effect-free read of the commit log. That route is retired, and what
    /// is left over GET is operator surface behind a STATIC bearer token — the
    /// exact shape that needs an attempt bound. The two liveness probes stay
    /// exempt, deliberately: throttling a health check manufactures an outage.
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

    /// Limiting is still per IP after hashing: one IP over its budget does not
    /// throttle another, and the SAME IP looked up afresh from headers (as the
    /// middleware does on every request) lands in the same, exhausted bucket.
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

    /// No plain IP is ever readable from the limiter's memory. Drive hits through
    /// the same header path the middleware uses and inspect every stored key —
    /// the tier prefix survives, the IP (v4, v6 or the sentinel) does not, and
    /// neither does it in the key's `Debug` form.
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

    /// The hashing key is per instance (i.e. per process): the same IP hashes to
    /// unrelated pseudonyms in two limiters, so a key seen in one process's
    /// memory says nothing about any other, and none survive a restart.
    #[test]
    fn different_limiters_hash_the_same_ip_differently() {
        let a = RateLimiter::new();
        let b = RateLimiter::new();
        assert_ne!(a.key_for_ip("t", "203.0.113.7"), b.key_for_ip("t", "203.0.113.7"));
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

    /// Behind the Worker the container receives no IP, only the bucket — and
    /// the bucket wins over any IP header that does arrive, so a local-run
    /// fallback can never take precedence over the edge's answer.
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
