//! The DS → LiveKit server-API path (#1751): every Twirp `RoomService` call,
//! its retry, its health record and the operator probe.
//!
//! **Self-healing.** A call that fails in transport (connect stalled or refused,
//! no answer within the deadline, connection broken) throws the LiveKit client
//! away, pool and all, and is tried once more on a fresh connection. Nothing
//! waits for a redeploy to get a new connection.
//!
//! **Visible.** Every failure logs one structured `warn!` naming the phase it
//! died in, which also lands in that request's `/v1/ops/errors` row. Five
//! failures in a row with no success between them is "degraded": one `error!`
//! on the way in, one `warn!` on the way out, `GET /health/livekit` answers 503
//! while it lasts, and `GET /v1/ops/livekit` has the counts. Nothing here names
//! a room, a user or an address.
//!
//! **Not blind when quiet.** The counts live in memory and a quiet DS (dev
//! sleeps after 10 idle minutes) has none, so on its own the signal read
//! `idle` straight through an outage. When no real call landed within
//! [`WINDOW_SECS`], reading either endpoint runs one [`check`]: a fresh
//! connection's `GET /`, at most once per [`CHECK_TTL_SECS`], one at a time.
//! It runs because someone asked, never on a timer.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::util::{redact_ip_literals, Upstream};
use crate::AppState;

/// This many transport or status failures in a row, with no success between, is an outage.
pub const DEGRADED_AFTER: u32 = 5;
/// The window `failures_last_10m` counts over, and how long a failure keeps "degraded" alive.
pub const WINDOW_SECS: u64 = 600;
/// Attempts per call: the first, plus one on a fresh connection.
pub const MAX_ATTEMPTS: u8 = 2;
/// How long one [`check`] answers for, so readers cost LiveKit one request a minute at most.
pub const CHECK_TTL_SECS: u64 = 60;
/// Largest LiveKit answer read; a roster is far smaller.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// Where a call died.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// DNS, TCP or TLS did not finish within [`crate::util::LIVEKIT_CONNECT_TIMEOUT`], or failed.
    Connect,
    /// Connected and sent, but no answer within the per-attempt deadline.
    Timeout,
    /// The connection broke mid-request, or the answer could not be read.
    Transport,
    /// LiveKit answered, with a status that is neither success nor "room empty".
    Status,
}

impl Phase {
    pub const fn name(self) -> &'static str {
        match self {
            Phase::Connect => "connect",
            Phase::Timeout => "timeout",
            Phase::Transport => "transport",
            Phase::Status => "status",
        }
    }
}

/// LiveKit's answer, read in full.
#[derive(Debug, Clone)]
pub struct TwirpReply {
    pub status: StatusCode,
    pub body: String,
}

/// A call that never got an answer.
#[derive(Debug, Clone)]
pub struct TwirpFailure {
    pub method: &'static str,
    pub phase: Phase,
    pub attempts: u8,
    pub elapsed_ms: u64,
    /// The last attempt's error chain, IP literals scrubbed.
    pub detail: String,
}

impl TwirpFailure {
    pub fn is_timeout(&self) -> bool {
        matches!(self.phase, Phase::Connect | Phase::Timeout)
    }
}

impl std::fmt::Display for TwirpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.phase {
            Phase::Connect => format!(
                "livekit connect failed or timed out ({}s)",
                crate::util::LIVEKIT_CONNECT_TIMEOUT.as_secs()
            ),
            Phase::Timeout => format!(
                "livekit timed out after {}s",
                Upstream::LiveKit.timeout().as_secs()
            ),
            Phase::Transport => "livekit connection failed".to_string(),
            Phase::Status => "livekit error status".to_string(),
        };
        write!(
            f,
            "{}: {what} [{} attempts, {}ms]",
            self.method, self.attempts, self.elapsed_ms
        )
    }
}

/// POST one Twirp `RoomService` method, retrying once on a fresh connection
/// after a transport failure. `body` must be safe to send twice (SendData
/// carries a nonce for that). Records the outcome in [`AppState::livekit_health`].
pub async fn twirp(
    state: &AppState,
    method: &'static str,
    token: &str,
    body: &serde_json::Value,
) -> Result<TwirpReply, TwirpFailure> {
    let Some((base, _)) = state.broker.livekit_api_base() else {
        return Err(TwirpFailure {
            method,
            phase: Phase::Status,
            attempts: 0,
            elapsed_ms: 0,
            detail: "livekit not configured".into(),
        });
    };
    let endpoint = format!("{base}/twirp/livekit.RoomService/{method}");
    let started = Instant::now();
    let mut first_phase = None;
    let mut attempt: u8 = 0;
    loop {
        attempt += 1;
        let (generation, request) = crate::util::livekit_post(&endpoint);
        let outcome = send_once(request.bearer_auth(token).json(body)).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(reply) => {
                let healthy = reply_is_healthy(&reply);
                if healthy {
                    state
                        .livekit_health
                        .record_ok(crate::util::now_unix(), elapsed_ms, attempt);
                    if let Some(phase) = first_phase {
                        tracing::warn!(
                            upstream = "livekit",
                            method,
                            first_phase = Phase::name(phase),
                            elapsed_ms,
                            "livekit: first attempt failed, a fresh connection succeeded"
                        );
                    }
                } else {
                    let failure = TwirpFailure {
                        method,
                        phase: Phase::Status,
                        attempts: attempt,
                        elapsed_ms,
                        detail: format!("{}", reply.status),
                    };
                    note_failure(state, &failure);
                }
                return Ok(reply);
            }
            Err((phase, detail)) => {
                let failure = TwirpFailure { method, phase, attempts: attempt, elapsed_ms, detail };
                if attempt >= MAX_ATTEMPTS {
                    note_failure(state, &failure);
                    return Err(failure);
                }
                first_phase = Some(phase);
                // A request on a dead pooled connection hangs or breaks the
                // same way the next one would; start from an empty pool.
                if crate::util::reset_livekit_client(generation) {
                    state.livekit_health.record_reset();
                }
                state.livekit_health.record_retry();
            }
        }
    }
}

/// One attempt with no retry and no health record (the probe's view).
async fn send_once(request: reqwest::RequestBuilder) -> Result<TwirpReply, (Phase, String)> {
    let resp = request.send().await.map_err(classify)?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(classify)?;
    let body = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_BODY_BYTES)]).into_owned();
    Ok(TwirpReply { status, body })
}

fn classify(e: reqwest::Error) -> (Phase, String) {
    let phase = if e.is_connect() {
        Phase::Connect
    } else if e.is_timeout() {
        Phase::Timeout
    } else {
        Phase::Transport
    };
    (phase, error_chain(&e))
}

/// The error and its sources, `": "`-joined, IP literals scrubbed.
fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        out.push_str(": ");
        out.push_str(&s.to_string());
        cur = s.source();
    }
    redact_ip_literals(&out).chars().take(300).collect()
}

/// Success, 404 (no such room / identity) and the empty-room 503 all mean LiveKit is up.
fn reply_is_healthy(reply: &TwirpReply) -> bool {
    reply.status.is_success()
        || reply.status == StatusCode::NOT_FOUND
        || crate::broker::send_data_room_is_empty(reply.status, &reply.body)
}

fn note_failure(state: &AppState, failure: &TwirpFailure) {
    let consecutive = state
        .livekit_health
        .record_failure(crate::util::now_unix(), failure);
    tracing::warn!(
        upstream = "livekit",
        method = failure.method,
        phase = failure.phase.name(),
        attempts = failure.attempts,
        elapsed_ms = failure.elapsed_ms,
        consecutive,
        detail = %failure.detail,
        "livekit call failed"
    );
    // A DS-originated nudge (enrollment prompt, inbox refresh, history
    // transfer) runs after its request already answered 2xx, so no request
    // row would ever show it: #1751's approval prompts failed that way,
    // unseen. Log it as its own row, named by the upstream call.
    if !crate::error_log::in_request() {
        let status = if failure.is_timeout() { 504 } else { 502 };
        crate::error_log::record_background(
            state,
            &format!("/twirp/livekit.RoomService/{}", failure.method),
            status,
            &failure.to_string(),
            &format!(
                "background nudge, no request waiting: phase={} consecutive={consecutive} {}",
                failure.phase.name(),
                failure.detail
            ),
        );
    }
}

/// Outcome counts for the DS → LiveKit path since the process started.
#[derive(Clone, Default)]
pub struct LiveKitHealth {
    inner: Arc<Mutex<Inner>>,
    /// One [`check`] at a time; the others wait for its answer.
    checking: Arc<tokio::sync::Mutex<()>>,
}

/// The result of one [`check`].
#[derive(Debug, Clone)]
pub struct Check {
    pub at: u64,
    pub ok: bool,
    pub ms: u64,
    /// Where it died, when it did.
    pub phase: Option<&'static str>,
    /// LiveKit's status, when it answered.
    pub status: Option<u16>,
}

impl Check {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "at": self.at,
            "ok": self.ok,
            "ms": self.ms,
            "phase": self.phase,
            "status": self.status,
        })
    }
}

#[derive(Default)]
struct Inner {
    since: u64,
    ok: u64,
    ok_on_retry: u64,
    failures: BTreeMap<&'static str, u64>,
    retries: u64,
    client_resets: u64,
    consecutive_failures: u32,
    recent_failures: VecDeque<u64>,
    last_ok_at: Option<u64>,
    last_ok_ms: Option<u64>,
    max_ok_ms: u64,
    last_failure_at: Option<u64>,
    last_failure: Option<String>,
    degraded_since: Option<u64>,
    last_check: Option<Check>,
}

/// What `GET /health/livekit` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The last call succeeded, or failures are below [`DEGRADED_AFTER`].
    Ok,
    /// [`DEGRADED_AFTER`] failures in a row, the newest within [`WINDOW_SECS`].
    Degraded,
    /// No call in [`WINDOW_SECS`]: nothing to judge by.
    Idle,
}

impl LiveKitHealth {
    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    pub fn record_ok(&self, now: u64, elapsed_ms: u64, attempts: u8) {
        self.with(|h| {
            h.since = if h.since == 0 { now } else { h.since };
            h.ok += 1;
            if attempts > 1 {
                h.ok_on_retry += 1;
            }
            h.last_ok_at = Some(now);
            h.last_ok_ms = Some(elapsed_ms);
            h.max_ok_ms = h.max_ok_ms.max(elapsed_ms);
            if let Some(started) = h.degraded_since.take() {
                tracing::warn!(
                    upstream = "livekit",
                    after_failures = h.consecutive_failures,
                    degraded_secs = now.saturating_sub(started),
                    "livekit recovered"
                );
            }
            h.consecutive_failures = 0;
        });
    }

    /// Returns the consecutive-failure count, this one included.
    pub fn record_failure(&self, now: u64, failure: &TwirpFailure) -> u32 {
        self.with(|h| {
            h.since = if h.since == 0 { now } else { h.since };
            *h.failures.entry(failure.phase.name()).or_default() += 1;
            h.consecutive_failures = h.consecutive_failures.saturating_add(1);
            h.last_failure_at = Some(now);
            h.last_failure = Some(format!("{} {}", failure.method, failure.phase.name()));
            h.recent_failures.push_back(now);
            while h
                .recent_failures
                .front()
                .is_some_and(|t| now.saturating_sub(*t) > WINDOW_SECS)
            {
                h.recent_failures.pop_front();
            }
            if h.consecutive_failures >= DEGRADED_AFTER && h.degraded_since.is_none() {
                h.degraded_since = Some(now);
                // The alert line: once per outage, not once per failure.
                tracing::error!(
                    upstream = "livekit",
                    consecutive = h.consecutive_failures,
                    newest = %format!("{} {}", failure.method, failure.phase.name()),
                    "livekit degraded: every recent DS -> LiveKit call failed"
                );
            }
            h.consecutive_failures
        })
    }

    /// The newest [`check`], if it is younger than [`CHECK_TTL_SECS`].
    fn fresh_check(&self, now: u64) -> Option<Check> {
        self.with(|h| {
            h.last_check
                .clone()
                .filter(|c| now.saturating_sub(c.at) < CHECK_TTL_SECS)
        })
    }

    pub fn record_retry(&self) {
        self.with(|h| h.retries += 1);
    }

    pub fn record_reset(&self) {
        self.with(|h| h.client_resets += 1);
    }

    pub fn verdict(&self, now: u64) -> Verdict {
        self.with(|h| {
            let last_call = h.last_ok_at.max(h.last_failure_at);
            match last_call {
                None => Verdict::Idle,
                Some(t) if now.saturating_sub(t) > WINDOW_SECS => Verdict::Idle,
                Some(_) if h.consecutive_failures >= DEGRADED_AFTER => Verdict::Degraded,
                Some(_) => Verdict::Ok,
            }
        })
    }

    /// The counts, for `GET /v1/ops/livekit`.
    pub fn snapshot(&self, now: u64) -> serde_json::Value {
        let verdict = self.verdict(now);
        self.with(|h| {
            let failures_last_10m = h
                .recent_failures
                .iter()
                .filter(|t| now.saturating_sub(**t) <= WINDOW_SECS)
                .count();
            serde_json::json!({
                "verdict": verdict_name(verdict),
                "since": h.since,
                "ok": h.ok,
                "ok_on_retry": h.ok_on_retry,
                "failures": h.failures,
                "failures_last_10m": failures_last_10m,
                "consecutive_failures": h.consecutive_failures,
                "degraded_since": h.degraded_since,
                "retries": h.retries,
                "client_resets": h.client_resets,
                "last_ok_at": h.last_ok_at,
                "last_ok_ms": h.last_ok_ms,
                "max_ok_ms": h.max_ok_ms,
                "last_failure_at": h.last_failure_at,
                "last_failure": h.last_failure,
                "check": h.last_check.as_ref().map(Check::json),
            })
        })
    }
}

fn verdict_name(v: Verdict) -> &'static str {
    match v {
        Verdict::Ok => "ok",
        Verdict::Degraded => "degraded",
        Verdict::Idle => "idle",
    }
}

/// What the verdict was judged by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Real calls within [`WINDOW_SECS`].
    Calls,
    /// No real call in the window, so a [`check`] decided.
    Check,
    /// Neither: LiveKit is not configured.
    None,
}

impl Source {
    pub const fn name(self) -> &'static str {
        match self {
            Source::Calls => "calls",
            Source::Check => "check",
            Source::None => "none",
        }
    }
}

/// The verdict the endpoints report: real calls when there are any in the
/// window, else a [`check`] (cached [`CHECK_TTL_SECS`]). `Idle` only when
/// LiveKit is not configured at all.
pub async fn effective_verdict(state: &AppState) -> (Verdict, Source) {
    let passive = state.livekit_health.verdict(crate::util::now_unix());
    if passive != Verdict::Idle {
        return (passive, Source::Calls);
    }
    match check(state).await {
        Some(c) if c.ok => (Verdict::Ok, Source::Check),
        Some(_) => (Verdict::Degraded, Source::Check),
        None => (Verdict::Idle, Source::None),
    }
}

/// One fresh-connection `GET /` (LiveKit answers "OK"), unless a check younger
/// than [`CHECK_TTL_SECS`] already answered. Single-flight: concurrent readers
/// share one request. Not counted as a call; kept as `check` in the counts. A
/// failure logs one `warn!`, which lands in the reading request's
/// `/v1/ops/errors` row when the endpoint answers 503.
pub async fn check(state: &AppState) -> Option<Check> {
    let health = &state.livekit_health;
    if let Some(c) = health.fresh_check(crate::util::now_unix()) {
        return Some(c);
    }
    let _one_at_a_time = health.checking.lock().await;
    if let Some(c) = health.fresh_check(crate::util::now_unix()) {
        return Some(c);
    }
    let (base, _) = state.broker.livekit_api_base()?;
    let started = Instant::now();
    let outcome = send_once(crate::util::livekit_probe_get(&format!("{base}/"))).await;
    let ms = started.elapsed().as_millis() as u64;
    let at = crate::util::now_unix();
    let result = match outcome {
        Ok(reply) => Check {
            at,
            ok: reply.status.is_success(),
            ms,
            phase: (!reply.status.is_success()).then_some(Phase::Status.name()),
            status: Some(reply.status.as_u16()),
        },
        Err((phase, detail)) => {
            tracing::warn!(
                upstream = "livekit",
                phase = phase.name(),
                ms,
                detail = %detail,
                "livekit check failed: no DS -> LiveKit call in the window, and a fresh connection got no answer"
            );
            Check { at, ok: false, ms, phase: Some(phase.name()), status: None }
        }
    };
    health.with(|h| h.last_check = Some(result.clone()));
    Some(result)
}

/// `GET /health/livekit` — open, like `/health`: `ok` / `idle` (200) or `degraded` (503).
///
/// Judged by real calls when there were any in the last [`WINDOW_SECS`];
/// otherwise by a [`check`], so it costs LiveKit at most one request per
/// [`CHECK_TTL_SECS`] however often it is read. `idle` now means only "LiveKit
/// is not configured". One word, no counts.
pub async fn health(State(state): State<AppState>) -> Response {
    let (verdict, _) = effective_verdict(&state).await;
    let status = if verdict == Verdict::Degraded {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (status, Json(serde_json::json!({ "livekit": verdict_name(verdict) }))).into_response()
}

#[derive(Deserialize)]
pub struct OpsQuery {
    #[serde(default)]
    probe: Option<String>,
}

/// `GET /v1/ops/livekit` — the counts; `?probe=1` also runs [`probe`] from
/// inside this process. Operator token: 404 unset, 401 wrong.
pub async fn ops(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<OpsQuery>,
) -> Response {
    let Some(expected) = state.metrics_token.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !crate::bearer_token_matches(&headers, expected) {
        return crate::error::AuthRejection::Unauthorized.into_response();
    }
    let (verdict, source) = effective_verdict(&state).await;
    let now = crate::util::now_unix();
    let mut counts = state.livekit_health.snapshot(now);
    // The calls-only verdict would say `idle` through a quiet outage.
    counts["verdict"] = verdict_name(verdict).into();
    counts["verdict_source"] = source.name().into();
    let mut out = serde_json::json!({
        "now": now,
        "counts": counts,
    });
    // Which env var the server-side base came from, and its hostname: never an
    // address literal (#1751).
    if let Some((base, source)) = state.broker.livekit_api_base() {
        out["api_base_source"] = source.name().into();
        out["api_host"] = api_hostname(&base).into();
    }
    if q.probe.as_deref().is_some_and(|p| p == "1" || p == "true") {
        out["probe"] = probe(&state).await;
    }
    (StatusCode::OK, Json(out)).into_response()
}

/// Time each leg of the DS → LiveKit path from where the DS actually runs:
/// DNS, a bare TCP connect, a fresh HTTPS round trip, then `ListRooms` and a
/// `SendData` to a room nobody is in over the shared client. Says where an
/// outage lives. Reports address COUNTS only, never an address, and never a
/// room name; nothing it does is recorded in [`LiveKitHealth`].
pub async fn probe(state: &AppState) -> serde_json::Value {
    let (Some((api_key, api_secret, _)), Some((base, _))) =
        (state.broker.livekit_ready(), state.broker.livekit_api_base())
    else {
        return serde_json::json!({ "error": "livekit not configured" });
    };
    let mut out = serde_json::Map::new();

    let parsed = reqwest::Url::parse(&base).ok();
    let host = parsed.as_ref().and_then(|u| u.host_str().map(str::to_string));
    let port = parsed.as_ref().and_then(|u| u.port_or_known_default());
    let mut first_addr = None;
    if let (Some(host), Some(port)) = (host, port) {
        let started = Instant::now();
        let dns = tokio::time::timeout(
            crate::util::LIVEKIT_CONNECT_TIMEOUT,
            tokio::net::lookup_host((host.as_str(), port)),
        )
        .await;
        let ms = started.elapsed().as_millis() as u64;
        out.insert(
            "dns".into(),
            match dns {
                Ok(Ok(addrs)) => {
                    let addrs: Vec<std::net::SocketAddr> = addrs.collect();
                    first_addr = addrs.first().copied();
                    serde_json::json!({
                        "ok": !addrs.is_empty(),
                        "ms": ms,
                        "v4": addrs.iter().filter(|a| a.is_ipv4()).count(),
                        "v6": addrs.iter().filter(|a| a.is_ipv6()).count(),
                    })
                }
                Ok(Err(e)) => step_err(ms, &e),
                Err(_) => serde_json::json!({ "ok": false, "ms": ms, "error": "timed out" }),
            },
        );
    }
    if let Some(addr) = first_addr {
        let started = Instant::now();
        let tcp = tokio::time::timeout(
            crate::util::LIVEKIT_CONNECT_TIMEOUT,
            tokio::net::TcpStream::connect(addr),
        )
        .await;
        let ms = started.elapsed().as_millis() as u64;
        out.insert(
            "tcp_connect".into(),
            match tcp {
                Ok(Ok(_)) => serde_json::json!({ "ok": true, "ms": ms }),
                Ok(Err(e)) => step_err(ms, &e),
                Err(_) => serde_json::json!({ "ok": false, "ms": ms, "error": "timed out" }),
            },
        );
        // The same host's port 80 (nginx's plain vhost). Port 443 failing while
        // 80 connects is a port-level block; both failing is the whole address
        // dropping this container's traffic (#1751).
        let started = Instant::now();
        let tcp80 = tokio::time::timeout(
            crate::util::LIVEKIT_CONNECT_TIMEOUT,
            tokio::net::TcpStream::connect(std::net::SocketAddr::new(addr.ip(), 80)),
        )
        .await;
        let ms = started.elapsed().as_millis() as u64;
        out.insert(
            "tcp_connect_port_80".into(),
            match tcp80 {
                Ok(Ok(_)) => serde_json::json!({ "ok": true, "ms": ms }),
                Ok(Err(e)) => step_err(ms, &e),
                Err(_) => serde_json::json!({ "ok": false, "ms": ms, "error": "timed out" }),
            },
        );
    }

    // Fresh handshake: LiveKit answers `GET /` with "OK".
    let started = Instant::now();
    let fresh = send_once(crate::util::livekit_probe_get(&format!("{base}/"))).await;
    out.insert("https_fresh".into(), step(started, fresh));

    let now = crate::util::now_unix();
    let list_token = crate::broker::sign_livekit_admin_token(api_key, api_secret, "", now);
    if let Ok(token) = list_token {
        let started = Instant::now();
        let (_, request) =
            crate::util::livekit_post(&format!("{base}/twirp/livekit.RoomService/ListRooms"));
        let listed = send_once(
            request
                .bearer_auth(&token)
                .json(&serde_json::json!({})),
        )
        .await;
        out.insert("list_rooms".into(), step(started, listed));
    }

    // The exact call that failed in #1751, aimed at a room nobody is in, so it
    // reaches no one: LiveKit answers its empty-room 503 after ~3s.
    let room = format!("pollis-ds-probe-{}", hex::encode(rand::random::<[u8; 8]>()));
    if let Ok(token) = crate::broker::sign_livekit_admin_token(api_key, api_secret, &room, now) {
        let started = Instant::now();
        let (_, request) =
            crate::util::livekit_post(&format!("{base}/twirp/livekit.RoomService/SendData"));
        let sent = send_once(request.bearer_auth(&token).json(&serde_json::json!({
            "room": room,
            "data": crate::util::b64(b"{}"),
            "kind": "RELIABLE",
        })))
        .await;
        out.insert("send_data_empty_room".into(), step(started, sent));
    }
    serde_json::Value::Object(out)
}

/// The base URL's DNS hostname; `None` for an IP literal (never reported) or
/// an unparsable URL.
fn api_hostname(base: &str) -> Option<String> {
    let url = reqwest::Url::parse(base).ok()?;
    let host = url.host_str()?;
    // `host_str` brackets an IPv6 literal, so it never parses as an address.
    if host.starts_with('[') || host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    Some(host.to_string())
}

fn step(started: Instant, r: Result<TwirpReply, (Phase, String)>) -> serde_json::Value {
    let ms = started.elapsed().as_millis() as u64;
    match r {
        Ok(reply) => serde_json::json!({
            "ok": reply_is_healthy(&reply),
            "ms": ms,
            "status": reply.status.as_u16(),
        }),
        Err((phase, detail)) => serde_json::json!({
            "ok": false,
            "ms": ms,
            "phase": phase.name(),
            "error": detail,
        }),
    }
}

fn step_err(ms: u64, e: &std::io::Error) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "ms": ms,
        "error": redact_ip_literals(&e.to_string()),
    })
}

/// The deadline a caller should budget for one [`twirp`] call, retry included.
pub fn worst_case() -> Duration {
    Upstream::LiveKit.timeout() * u32::from(MAX_ATTEMPTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_host_is_reported_only_when_it_is_a_name() {
        assert_eq!(api_hostname("https://lk-api.example.com").as_deref(), Some("lk-api.example.com"));
        assert_eq!(api_hostname("http://localhost:7880").as_deref(), Some("localhost"));
        assert_eq!(api_hostname("http://192.0.2.1:7880"), None);
        assert_eq!(api_hostname("http://[2001:db8::1]:7880"), None);
    }

    fn failure(phase: Phase) -> TwirpFailure {
        TwirpFailure { method: "SendData", phase, attempts: 2, elapsed_ms: 12_000, detail: String::new() }
    }

    #[test]
    fn degraded_needs_consecutive_failures_and_a_success_clears_it() {
        let h = LiveKitHealth::default();
        assert_eq!(h.verdict(1_000), Verdict::Idle);
        for i in 0..DEGRADED_AFTER - 1 {
            h.record_failure(1_000 + u64::from(i), &failure(Phase::Timeout));
        }
        assert_eq!(h.verdict(1_010), Verdict::Ok, "below the threshold is not an outage");
        h.record_failure(1_010, &failure(Phase::Connect));
        assert_eq!(h.verdict(1_010), Verdict::Degraded);
        assert_eq!(h.snapshot(1_010)["degraded_since"], 1_010);

        h.record_ok(1_020, 80, 1);
        assert_eq!(h.verdict(1_020), Verdict::Ok);
        assert_eq!(h.snapshot(1_020)["consecutive_failures"], 0);
        assert!(h.snapshot(1_020)["degraded_since"].is_null());
    }

    #[test]
    fn interleaved_successes_never_read_as_degraded() {
        let h = LiveKitHealth::default();
        for i in 0..50 {
            h.record_failure(2_000 + i, &failure(Phase::Timeout));
            h.record_ok(2_000 + i, 3_100, 1);
        }
        assert_eq!(h.verdict(2_050), Verdict::Ok);
        assert_eq!(h.snapshot(2_050)["failures_last_10m"], 50);
    }

    #[test]
    fn an_old_outage_goes_idle_and_the_window_forgets_it() {
        let h = LiveKitHealth::default();
        for i in 0..DEGRADED_AFTER {
            h.record_failure(3_000 + u64::from(i), &failure(Phase::Timeout));
        }
        assert_eq!(h.verdict(3_010), Verdict::Degraded);
        let later = 3_010 + WINDOW_SECS + 1;
        assert_eq!(h.verdict(later), Verdict::Idle, "no call to judge by");
        assert_eq!(h.snapshot(later)["failures_last_10m"], 0);
    }

    #[test]
    fn counts_are_kept_per_phase() {
        let h = LiveKitHealth::default();
        h.record_failure(1, &failure(Phase::Connect));
        h.record_failure(2, &failure(Phase::Timeout));
        h.record_failure(3, &failure(Phase::Timeout));
        let snap = h.snapshot(3);
        assert_eq!(snap["failures"]["connect"], 1);
        assert_eq!(snap["failures"]["timeout"], 2);
        assert_eq!(snap["last_failure"], "SendData timeout");
    }

    #[test]
    fn the_failure_text_names_the_phase_and_the_attempts() {
        let text = failure(Phase::Timeout).to_string();
        assert!(text.starts_with("SendData: livekit timed out after"), "{text}");
        assert!(text.contains("2 attempts"), "{text}");
        let text = failure(Phase::Connect).to_string();
        assert!(text.contains("connect"), "{text}");
    }

    #[test]
    fn the_whole_budget_stays_under_the_clients_read_timeout() {
        // pollis-relay's client gives up on a silent read after 30s.
        assert!(worst_case() < Duration::from_secs(30), "{:?}", worst_case());
    }
}
