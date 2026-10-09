//! Dev-only observability for `request-otp` and for every rate-limited request.
//!
//! **Why:** on 2026-10-08 the dev DS's raised `request-otp` budget (then 200
//! per 10 minutes per client; the OTP tiers are now `off` on dev,
//! `wrangler.dev.jsonc`) still ran out during a mobile e2e run. The limiter could say THAT a client was over budget
//! but nothing could say WHO had spent it: the counters are anonymous by design,
//! Workers Logs are off, and container stdout is not retained. This module
//! records enough to tell the callers apart — never enough to identify one.
//!
//! **Gate:** [`crate::AppState::ops_log`] is `Some` only when the DS starts with
//! `POLLIS_DS_OPS_LOG` set to an on value ([`enabled_from_value`]). Only
//! `wrangler.dev.jsonc` sets it; production sets none, so in production there is
//! no [`OpsLog`] object, the middleware branch that would call [`observe`] is
//! never taken, and `GET /v1/ops/otp-requests` answers 404.
//! `tests/otp_ops_log.rs` fails the build if `wrangler.prod.jsonc` ever sets it.
//!
//! **What an event holds** (and nothing else):
//! - time, endpoint path, rate-limit tier, outcome and HTTP status;
//! - the client's PSEUDONYM — the same per-process keyed HMAC the limiter's map
//!   already holds ([`crate::ratelimit::ClientKey`]), never an IP;
//! - the `User-Agent`, truncated, and an `X-Pollis-Client-Version` if one is
//!   sent (no client sends one today), both with any IP-shaped token redacted;
//! - for `request-otp` only, the email's DOMAIN and a short keyed TAG of the
//!   normalized address ([`crate::ratelimit::RateLimiter::email_tag`]) — the tag
//!   tells "the same address again" from "a new address each time" and cannot be
//!   reversed or compared across a restart. Never the address itself.
//!
//! **Where it goes:** one `tracing` line per event (target
//! `pollis_delivery::ops`, readable on a local run), plus a bounded in-memory
//! ring of the last [`MAX_EVENTS`] events and a per-client summary, served on
//! `GET /v1/ops/otp-requests` behind the operator bearer token
//! (`POLLIS_DS_METRICS_TOKEN`). Memory only: it is gone at the next restart or
//! scale-to-zero sleep.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header::USER_AGENT, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::ratelimit::{too_many_requests, ClientKey, RateLimiter, OTP_REQUEST_TIER};
use crate::AppState;

/// The gate. Read once at startup in [`crate::build_app_state`].
pub const ENV_VAR: &str = "POLLIS_DS_OPS_LOG";

/// Events kept in the ring; the oldest is dropped first.
pub const MAX_EVENTS: usize = 1000;
/// Clients kept in the summary; the least recently seen is dropped first.
pub const MAX_CLIENTS: usize = 1000;
/// Distinct user agents / client versions kept per client summary.
const MAX_LABELS_PER_CLIENT: usize = 5;
/// Characters kept from a `User-Agent`.
pub const USER_AGENT_MAX_CHARS: usize = 120;
/// Characters kept from a client version.
const CLIENT_VERSION_MAX_CHARS: usize = 40;
/// Characters kept from an email domain.
const EMAIL_DOMAIN_MAX_CHARS: usize = 64;
/// The same ceiling axum's `Bytes` extractor applies to the handler's body by
/// default, so buffering here rejects exactly what the handler would.
const BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// An optional client-version header. Recorded when present; no Pollis client
/// sends one yet (the app's HTTP client sends no `User-Agent` either, which is
/// itself a useful signal: an absent agent is the app, `curl/…` is a script).
pub const CLIENT_VERSION_HEADER: &str = "x-pollis-client-version";

/// Parse the gate's value. Only an explicit on value enables it; anything else,
/// including a typo, leaves it off.
pub fn enabled_from_value(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// [`enabled_from_value`] over the process environment.
pub fn enabled_from_env() -> bool {
    enabled_from_value(std::env::var("POLLIS_DS_OPS_LOG").ok().as_deref())
}

/// How the DS answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The handler answered 2xx. For `request-otp` that is the code issued and
    /// mailed — unless the per-email resend throttle or `DEV_OTP` held the mail
    /// back, which the anti-enumeration 200 deliberately does not reveal.
    Accepted,
    /// The per-client limiter answered 429.
    RateLimited,
    /// The handler answered anything else (a malformed body, etc.).
    Rejected,
}

/// One observed request. Every field is listed in the module docs; nothing here
/// can hold an IP or an email address.
#[derive(Clone, Debug, Serialize)]
pub struct OpsEvent {
    pub ts: u64,
    pub endpoint: String,
    pub tier: &'static str,
    pub client: String,
    pub user_agent: Option<String>,
    pub client_version: Option<String>,
    pub email_domain: Option<String>,
    pub email_tag: Option<String>,
    pub outcome: Outcome,
    pub status: u16,
}

#[derive(Default)]
struct ClientAgg {
    requests: u64,
    accepted: u64,
    rate_limited: u64,
    rejected: u64,
    first_ts: u64,
    last_ts: u64,
    user_agents: BTreeSet<String>,
    client_versions: BTreeSet<String>,
    endpoints: BTreeMap<String, u64>,
    email_tags: BTreeSet<String>,
}

#[derive(Serialize)]
struct ClientSummary<'a> {
    client: &'a str,
    requests: u64,
    accepted: u64,
    rate_limited: u64,
    rejected: u64,
    first_ts: u64,
    last_ts: u64,
    /// Distinct email tags seen from this client (`request-otp` only), capped at
    /// [`MAX_EVENTS`]. Many requests over few tags is a retry loop; one tag per
    /// request is a signup suite.
    distinct_emails: usize,
    user_agents: &'a BTreeSet<String>,
    client_versions: &'a BTreeSet<String>,
    endpoints: &'a BTreeMap<String, u64>,
}

#[derive(Default)]
struct Inner {
    events: VecDeque<OpsEvent>,
    clients: HashMap<String, ClientAgg>,
    total_recorded: u64,
    since: u64,
}

/// The in-memory ring + per-client summary. Shallow-`Clone` (shared `Arc`) so it
/// rides on the `Clone` `AppState`.
#[derive(Clone, Default)]
pub struct OpsLog {
    inner: Arc<Mutex<Inner>>,
}

impl OpsLog {
    /// Record one event: a log line, the ring, and the client's summary.
    pub fn record(&self, ev: OpsEvent) {
        tracing::info!(
            target: "pollis_delivery::ops",
            ts = ev.ts,
            endpoint = %ev.endpoint,
            tier = ev.tier,
            client = %ev.client,
            user_agent = ev.user_agent.as_deref().unwrap_or("-"),
            client_version = ev.client_version.as_deref().unwrap_or("-"),
            email_domain = ev.email_domain.as_deref().unwrap_or("-"),
            email_tag = ev.email_tag.as_deref().unwrap_or("-"),
            outcome = ?ev.outcome,
            status = ev.status,
            "ops: otp/rate-limit event"
        );
        let mut g = self.inner.lock().expect("ops log mutex poisoned");
        if g.since == 0 {
            g.since = ev.ts;
        }
        g.total_recorded += 1;

        if !g.clients.contains_key(&ev.client) && g.clients.len() >= MAX_CLIENTS {
            let stalest = g
                .clients
                .iter()
                .min_by_key(|(_, a)| a.last_ts)
                .map(|(k, _)| k.clone());
            if let Some(k) = stalest {
                g.clients.remove(&k);
            }
        }
        let agg = g.clients.entry(ev.client.clone()).or_default();
        if agg.requests == 0 {
            agg.first_ts = ev.ts;
        }
        agg.requests += 1;
        agg.last_ts = ev.ts;
        match ev.outcome {
            Outcome::Accepted => agg.accepted += 1,
            Outcome::RateLimited => agg.rate_limited += 1,
            Outcome::Rejected => agg.rejected += 1,
        }
        *agg.endpoints.entry(ev.endpoint.clone()).or_default() += 1;
        let ua = ev.user_agent.clone().unwrap_or_else(|| "(none)".to_string());
        if agg.user_agents.len() < MAX_LABELS_PER_CLIENT || agg.user_agents.contains(&ua) {
            agg.user_agents.insert(ua);
        }
        if let Some(v) = &ev.client_version {
            if agg.client_versions.len() < MAX_LABELS_PER_CLIENT {
                agg.client_versions.insert(v.clone());
            }
        }
        if let Some(t) = &ev.email_tag {
            if agg.email_tags.len() < MAX_EVENTS {
                agg.email_tags.insert(t.clone());
            }
        }

        if g.events.len() >= MAX_EVENTS {
            g.events.pop_front();
        }
        g.events.push_back(ev);
    }

    /// The operator view: per-client summary (busiest first) and the retained
    /// events (newest first).
    pub fn snapshot(&self, now: u64) -> serde_json::Value {
        let g = self.inner.lock().expect("ops log mutex poisoned");
        let mut clients: Vec<ClientSummary<'_>> = g
            .clients
            .iter()
            .map(|(client, a)| ClientSummary {
                client,
                requests: a.requests,
                accepted: a.accepted,
                rate_limited: a.rate_limited,
                rejected: a.rejected,
                first_ts: a.first_ts,
                last_ts: a.last_ts,
                distinct_emails: a.email_tags.len(),
                user_agents: &a.user_agents,
                client_versions: &a.client_versions,
                endpoints: &a.endpoints,
            })
            .collect();
        clients.sort_by(|a, b| b.requests.cmp(&a.requests).then(b.last_ts.cmp(&a.last_ts)));
        let events: Vec<&OpsEvent> = g.events.iter().rev().collect();
        serde_json::json!({
            "now": now,
            "since": g.since,
            "total_recorded": g.total_recorded,
            "retained_events": g.events.len(),
            "clients": clients,
            "events": events,
        })
    }
}

/// A header as display text: trimmed, control characters dropped, any IP-shaped
/// token redacted, and cut to `max` characters. `None` when absent or empty.
fn label(headers: &HeaderMap, name: impl axum::http::header::AsHeaderName, max: usize) -> Option<String> {
    let raw = headers.get(name).map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())?;
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return None;
    }
    Some(truncate(&crate::util::redact_ip_literals(cleaned), max))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// `(domain, keyed tag)` of the `request-otp` body's email, or `(None, None)`
/// when the body is not a request-otp body or the email is empty.
fn email_fields(limiter: &RateLimiter, body: &[u8]) -> (Option<String>, Option<String>) {
    let Ok(parsed) = serde_json::from_slice::<pollis_api::otp::RequestOtpBody>(body) else {
        return (None, None);
    };
    let normalized = crate::otp::normalize_email(&parsed.email);
    if normalized.is_empty() {
        return (None, None);
    }
    let domain = normalized
        .rsplit_once('@')
        .map(|(_, d)| d.trim())
        .filter(|d| !d.is_empty())
        .map(|d| truncate(&crate::util::redact_ip_literals(d), EMAIL_DOMAIN_MAX_CHARS));
    (domain, Some(limiter.email_tag(&normalized)))
}

/// Answer the request exactly as the limiter decided, and record it.
///
/// Called by [`crate::ratelimit::rate_limit`] only when the ops log is on and the
/// request is a `request-otp` or was just rate-limited. `limited` is the
/// limiter's verdict, already counted; this function never changes it. A limited
/// request gets the same [`too_many_requests`] the limiter returns; anything else
/// goes to the handler. For `request-otp` the body is buffered (to read the
/// email's domain) and handed on unchanged.
pub async fn observe(
    ops: &OpsLog,
    limiter: &RateLimiter,
    tier: &'static str,
    key: &ClientKey,
    limited: bool,
    req: Request,
    next: Next,
) -> Response {
    let user_agent = label(req.headers(), USER_AGENT, USER_AGENT_MAX_CHARS);
    let client_version = label(req.headers(), CLIENT_VERSION_HEADER, CLIENT_VERSION_MAX_CHARS);
    let endpoint = req.uri().path().to_string();

    let mut email_domain = None;
    let mut email_tag = None;
    let response = if tier == OTP_REQUEST_TIER {
        let (parts, body) = req.into_parts();
        match axum::body::to_bytes(body, BODY_LIMIT_BYTES).await {
            Ok(bytes) => {
                (email_domain, email_tag) = email_fields(limiter, &bytes);
                if limited {
                    too_many_requests()
                } else {
                    next.run(Request::from_parts(parts, Body::from(bytes))).await
                }
            }
            // Over the size the handler's extractor accepts (or unreadable): the
            // handler would have refused it the same way, unless the limiter
            // already had.
            Err(_) if limited => too_many_requests(),
            Err(_) => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        }
    } else if limited {
        too_many_requests()
    } else {
        next.run(req).await
    };

    let status = response.status();
    let outcome = if limited {
        Outcome::RateLimited
    } else if status.is_success() {
        Outcome::Accepted
    } else {
        Outcome::Rejected
    };
    ops.record(OpsEvent {
        ts: crate::util::now_unix(),
        endpoint,
        tier,
        client: key.pseudonym().to_string(),
        user_agent,
        client_version,
        email_domain,
        email_tag,
        outcome,
        status: status.as_u16(),
    });
    response
}

/// `GET /v1/ops/otp-requests` — the ops log's operator view. 404 unless the ops
/// log is on AND an operator token is configured (fail-closed, like
/// `/v1/retention/metrics`); 401 without the right bearer token.
pub async fn otp_requests(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let (Some(ops), Some(expected)) = (state.ops_log.as_ref(), state.metrics_token.as_deref()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !crate::bearer_token_matches(&headers, expected) {
        return crate::error::AuthRejection::Unauthorized.into_response();
    }
    (StatusCode::OK, Json(ops.snapshot(crate::util::now_unix()))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(client: &str, ts: u64, outcome: Outcome, tag: Option<&str>) -> OpsEvent {
        OpsEvent {
            ts,
            endpoint: "/v1/auth/request-otp".into(),
            tier: OTP_REQUEST_TIER,
            client: client.into(),
            user_agent: None,
            client_version: None,
            email_domain: Some("example.com".into()),
            email_tag: tag.map(str::to_string),
            outcome,
            status: if outcome == Outcome::RateLimited { 429 } else { 200 },
        }
    }

    /// Only an explicit on value turns it on; a typo or an off value does not.
    #[test]
    fn the_gate_is_off_unless_explicitly_on() {
        for on in ["1", "true", "TRUE", " yes ", "on"] {
            assert!(enabled_from_value(Some(on)), "{on:?} should enable");
        }
        for off in ["", "0", "false", "off", "no", "ture", "enabled"] {
            assert!(!enabled_from_value(Some(off)), "{off:?} must not enable");
        }
        assert!(!enabled_from_value(None));
    }

    /// The ring and the client map are both bounded, and evict oldest first.
    #[test]
    fn the_ring_and_client_map_are_bounded() {
        let log = OpsLog::default();
        for i in 0..(MAX_EVENTS as u64 + 10) {
            log.record(ev(&format!("c{i}"), 1000 + i, Outcome::Accepted, None));
        }
        let snap = log.snapshot(9999);
        assert_eq!(snap["retained_events"], MAX_EVENTS);
        assert_eq!(snap["total_recorded"], MAX_EVENTS as u64 + 10);
        assert_eq!(snap["clients"].as_array().unwrap().len(), MAX_CLIENTS);
        // Newest first; the first ten were dropped from both.
        assert_eq!(snap["events"][0]["client"], format!("c{}", MAX_EVENTS + 9));
        let clients: Vec<&str> = snap["clients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["client"].as_str().unwrap())
            .collect();
        assert!(!clients.contains(&"c0"), "the least recently seen client is evicted");
    }

    /// The summary separates a retry loop (many requests, one address) from a
    /// signup suite (one address per request) and counts outcomes.
    #[test]
    fn the_summary_counts_outcomes_and_distinct_emails() {
        let log = OpsLog::default();
        for t in 0..5 {
            log.record(ev("loop", 100 + t, Outcome::Accepted, Some("same")));
        }
        log.record(ev("loop", 106, Outcome::RateLimited, Some("same")));
        for t in 0..3 {
            log.record(ev("suite", 100 + t, Outcome::Accepted, Some(&format!("t{t}"))));
        }
        let snap = log.snapshot(200);
        let top = &snap["clients"][0];
        assert_eq!(top["client"], "loop", "busiest client first");
        assert_eq!(top["requests"], 6);
        assert_eq!(top["accepted"], 5);
        assert_eq!(top["rate_limited"], 1);
        assert_eq!(top["distinct_emails"], 1);
        assert_eq!(top["first_ts"], 100);
        assert_eq!(top["last_ts"], 106);
        assert_eq!(top["user_agents"], serde_json::json!(["(none)"]));
        assert_eq!(snap["clients"][1]["distinct_emails"], 3);
    }

    /// A user agent is cut to length, stripped of control characters, and loses
    /// any IP-shaped token — a client must not be able to plant an address in
    /// the operator view through its own header.
    #[test]
    fn user_agents_are_truncated_and_ip_redacted() {
        let mut h = HeaderMap::new();
        h.insert(USER_AGENT, "probe/1.0 (from 203.0.113.9; 2001:db8::7)".parse().unwrap());
        let ua = label(&h, USER_AGENT, USER_AGENT_MAX_CHARS).unwrap();
        assert!(ua.starts_with("probe/1.0"), "{ua}");
        assert!(!ua.contains("203.0.113.9") && !ua.contains("2001:db8::7"), "{ua}");

        h.insert(USER_AGENT, "x".repeat(500).parse().unwrap());
        let ua = label(&h, USER_AGENT, USER_AGENT_MAX_CHARS).unwrap();
        assert_eq!(ua.chars().count(), USER_AGENT_MAX_CHARS + 1, "cut + ellipsis");

        h.insert(USER_AGENT, "   ".parse().unwrap());
        assert_eq!(label(&h, USER_AGENT, USER_AGENT_MAX_CHARS), None);
    }

    /// Only the domain and a keyed tag leave the body; an IP-literal domain is
    /// redacted too.
    #[test]
    fn email_fields_keep_only_domain_and_tag() {
        let rl = RateLimiter::default();
        let (d, t) = email_fields(&rl, br#"{"email":"  Alice@Example.COM "}"#);
        assert_eq!(d.as_deref(), Some("example.com"));
        let t = t.unwrap();
        assert!(!t.contains("alice"));
        assert_eq!(Some(t), email_fields(&rl, br#"{"email":"alice@example.com"}"#).1, "normalized first");

        let (d, _) = email_fields(&rl, br#"{"email":"x@[192.0.2.1]"}"#);
        assert!(!d.unwrap().contains("192.0.2.1"));

        assert_eq!(email_fields(&rl, b"not json"), (None, None));
        assert_eq!(email_fields(&rl, br#"{"email":"  "}"#), (None, None));
    }
}
