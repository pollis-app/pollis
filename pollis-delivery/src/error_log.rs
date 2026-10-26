//! The DS's failed-request log (#1305).
//!
//! **Why:** a tester's first-device signup failed after `verify-otp` succeeded,
//! and nothing could say which request failed or why: Workers Logs and Logpush
//! are off (they record client IPs — `tests/no_client_ip_exposure.rs`), and
//! container stdout, where `tracing` writes, is not retained. A 5xx body says
//! only "internal error"; the cause went to a log nobody can read.
//!
//! **What:** every routed request that ends in a 4xx/5xx (except the limiter's
//! own 429s) is recorded in `ds_error_log`: when, the method, the router's
//! matched route pattern, the status, the response's `error` text, and any
//! `warn!`/`error!` the request emitted while it ran — the cause behind an
//! "internal error". Plus the client's random `X-Pollis-Request-Id`, which is
//! how a line in a user's diagnostics log finds its row here.
//!
//! **Not who:** no user, device, email or IP. The table has no column for one
//! (migration `000035_ds_error_log`), the route is the pattern rather than the
//! raw path, and every text field passes [`pollis_api::redact::scrub`] first.
//!
//! **Tight by construction:** the table's own trigger keeps 72 hours and 500
//! rows at most, on every insert. On top of that the DS writes at most
//! [`MAX_WRITES_PER_MINUTE`] rows a minute, so a burst of junk requests costs a
//! bounded number of database writes and is counted rather than stored.
//!
//! **Read it:** `GET /v1/ops/errors` with the operator bearer token
//! (`POLLIS_DS_METRICS_TOKEN`); 404 when no token is configured.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use crate::db::Db;
use crate::AppState;

/// Rows written per minute, at most. Beyond it a failure is counted in
/// [`ErrorLog::dropped`] and not stored.
pub const MAX_WRITES_PER_MINUTE: u32 = 30;
/// Longest `error` kept (matches the column's CHECK).
pub const ERROR_MAX_CHARS: usize = 200;
/// Longest `detail` kept (matches the column's CHECK).
pub const DETAIL_MAX_CHARS: usize = 1000;
/// `warn!`/`error!` events kept per request.
const MAX_EVENTS_PER_REQUEST: usize = 4;
/// Error bodies larger than this are not read for their `error` text.
const MAX_ERROR_BODY_BYTES: u64 = 8 * 1024;
/// Rows `GET /v1/ops/errors` returns — the table's own cap.
const READ_LIMIT: i64 = 500;

tokio::task_local! {
    /// The `warn!`/`error!` messages emitted while the current request ran.
    /// Scoped per request by [`record`]; [`CaptureLayer`] appends to it.
    static CAPTURED: RefCell<Vec<String>>;
}

/// Shared write budget. Shallow-`Clone`, like the other `AppState` stores.
#[derive(Clone, Default)]
pub struct ErrorLog {
    budget: Arc<Mutex<Budget>>,
}

#[derive(Default)]
struct Budget {
    window_start: u64,
    used: u32,
    dropped: u64,
}

impl ErrorLog {
    /// Take one write from this minute's budget.
    fn take(&self, now: u64) -> bool {
        let mut b = self.budget.lock().unwrap_or_else(|e| e.into_inner());
        if now.saturating_sub(b.window_start) >= 60 {
            b.window_start = now;
            b.used = 0;
        }
        if b.used >= MAX_WRITES_PER_MINUTE {
            b.dropped += 1;
            return false;
        }
        b.used += 1;
        true
    }

    /// Failures not stored because the per-minute budget was spent, since start.
    pub fn dropped(&self) -> u64 {
        self.budget.lock().unwrap_or_else(|e| e.into_inner()).dropped
    }
}

/// One failed request, already scrubbed and bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedRequest {
    pub at: u64,
    pub request_id: Option<String>,
    pub method: String,
    pub route: String,
    pub status: u16,
    pub error: Option<String>,
    pub detail: Option<String>,
}

impl FailedRequest {
    fn new(
        at: u64,
        request_id: Option<&str>,
        method: &str,
        route: &str,
        status: u16,
        error: Option<&str>,
        events: &[String],
    ) -> Self {
        let detail = if events.is_empty() { None } else { Some(events.join(" | ")) };
        Self {
            at,
            request_id: request_id
                .filter(|id| pollis_api::redact::is_valid_request_id(id))
                .map(str::to_string),
            method: method.to_string(),
            route: route.chars().take(128).collect(),
            status,
            error: error.map(|e| bounded(e, ERROR_MAX_CHARS)),
            detail: detail.map(|d| bounded(&d, DETAIL_MAX_CHARS)),
        }
    }
}

fn bounded(text: &str, max: usize) -> String {
    pollis_api::redact::scrub(text).chars().take(max).collect()
}

/// Route-level middleware (installed with `route_layer`, so it sees only
/// requests the router matched — a scanner's 404s never reach it).
pub async fn record(
    State(state): State<AppState>,
    matched: MatchedPath,
    req: Request,
    next: Next,
) -> Response {
    let request_id = req
        .headers()
        .get(pollis_api::redact::REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let method = req.method().as_str().to_string();
    let route = matched.as_str().to_string();

    let (response, events) = CAPTURED
        .scope(RefCell::new(Vec::new()), async move {
            let response = next.run(req).await;
            let events = CAPTURED.with(|c| c.take());
            (response, events)
        })
        .await;

    let status = response.status();
    // The limiter's 429s are counted by the limiter; logging them would let one
    // noisy client fill the table with its own throttling.
    if !(status.is_client_error() || status.is_server_error())
        || status == StatusCode::TOO_MANY_REQUESTS
    {
        return response;
    }
    let (response, error) = take_error_text(response).await;
    let now = crate::util::now_unix();
    if !state.error_log.take(now) {
        return response;
    }
    let row = FailedRequest::new(
        now,
        request_id.as_deref(),
        &method,
        &route,
        status.as_u16(),
        error.as_deref(),
        &events,
    );
    let db = Arc::clone(&state.db);
    // Off the response path, and best-effort: a DS running before migration
    // 000035 is applied simply fails these inserts.
    tokio::spawn(async move {
        if let Err(e) = insert(&db, &row).await {
            tracing::debug!("ds_error_log insert skipped: {e:#}");
        }
    });
    response
}

/// Read the JSON `error` field out of a small error body, handing back an
/// identical response. Bodies of unknown or large size are left untouched.
async fn take_error_text(response: Response) -> (Response, Option<String>) {
    use axum::body::HttpBody as _;
    let small = response
        .body()
        .size_hint()
        .exact()
        .is_some_and(|n| n <= MAX_ERROR_BODY_BYTES);
    if !small {
        return (response, None);
    }
    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_ERROR_BODY_BYTES as usize).await else {
        // Unreachable for a body whose exact size is under the limit; answer
        // with an empty body of the same status rather than panic.
        return (Response::from_parts(parts, Body::empty()), None);
    };
    let error = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string));
    (Response::from_parts(parts, Body::from(bytes)), error)
}

/// Store one row. The table's trigger trims it to 72 hours / 500 rows.
pub async fn insert(db: &Db, row: &FailedRequest) -> anyhow::Result<()> {
    let conn = db.conn().await?;
    conn.execute(
        "INSERT INTO ds_error_log (at, request_id, method, route, status, error, detail) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        libsql::params![
            row.at as i64,
            row.request_id.clone(),
            row.method.clone(),
            row.route.clone(),
            row.status as i64,
            row.error.clone(),
            row.detail.clone()
        ],
    )
    .await
    .map_err(|e| {
        // It runs right after a failure, so it is likely to meet the same
        // outage; tell the pool before this connection is released (#1500).
        let e = anyhow::Error::from(e);
        crate::error::note_transient(&e);
        e
    })?;
    Ok(())
}

/// `GET /v1/ops/errors` — the stored failures, newest first, behind the
/// operator token. 404 when no token is configured (fail-closed, like
/// `/v1/retention/metrics`); 401 without the right bearer token.
pub async fn list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(expected) = state.metrics_token.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !crate::bearer_token_matches(&headers, expected) {
        return crate::error::AuthRejection::Unauthorized.into_response();
    }
    match read_all(&state.db).await {
        Ok(rows) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "dropped_since_start": state.error_log.dropped(),
                "errors": rows,
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("ds_error_log read failed: {e:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "internal error" })),
            )
                .into_response()
        }
    }
}

async fn read_all(db: &Db) -> anyhow::Result<Vec<serde_json::Value>> {
    let conn = db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT at, request_id, method, route, status, error, detail \
             FROM ds_error_log ORDER BY id DESC LIMIT ?1",
            libsql::params![READ_LIMIT],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(serde_json::json!({
            "at": row.get::<i64>(0)?,
            "request_id": row.get::<Option<String>>(1)?,
            "method": row.get::<String>(2)?,
            "route": row.get::<String>(3)?,
            "status": row.get::<i64>(4)?,
            "error": row.get::<Option<String>>(5)?,
            "detail": row.get::<Option<String>>(6)?,
        }));
    }
    Ok(out)
}

/// A `tracing` layer that copies each `warn!`/`error!` raised inside a request
/// into that request's [`CAPTURED`] buffer. Outside a request it does nothing.
/// Installed next to the stdout formatter in `main`.
pub struct CaptureLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // `Level` orders by verbosity: ERROR < WARN < INFO.
        if *event.metadata().level() > tracing::Level::WARN {
            return;
        }
        let _ = CAPTURED.try_with(|captured| {
            let mut captured = captured.borrow_mut();
            if captured.len() >= MAX_EVENTS_PER_REQUEST {
                return;
            }
            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            captured.push(visitor.0);
        });
    }
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, "{}={value:?}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_is_scrubbed_and_bounded() {
        let events = vec!["establish-identity failed for alice@example.com from 203.0.113.7".to_string()];
        let long = "x".repeat(ERROR_MAX_CHARS * 2);
        let row = FailedRequest::new(1, Some("0123abcd"), "POST", "/v1/a", 500, Some(&long), &events);
        assert_eq!(row.error.as_ref().unwrap().chars().count(), ERROR_MAX_CHARS);
        assert_eq!(
            row.detail.as_deref(),
            Some("establish-identity failed for ***@example.com from [redacted]")
        );
    }

    #[test]
    fn a_request_id_that_could_carry_data_is_dropped() {
        let row = FailedRequest::new(1, Some("alice@example.com"), "POST", "/v1/a", 400, None, &[]);
        assert_eq!(row.request_id, None);
    }

    #[test]
    fn the_write_budget_resets_each_minute() {
        let log = ErrorLog::default();
        for _ in 0..MAX_WRITES_PER_MINUTE {
            assert!(log.take(100));
        }
        assert!(!log.take(100));
        assert_eq!(log.dropped(), 1);
        assert!(log.take(161));
    }
}
