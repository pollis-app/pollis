//! The minimum-client-version policy, server side (#1611).
//!
//! **Source of truth: the published file**, not DS config. The DS mirrors
//! `POLLIS_CLIENT_POLICY_URL` (the same `cdn.pollis.com/…client-policy.json` the clients fetch,
//! written only by `scripts/client-policy.mjs`), so there is one place to change a minimum and no
//! deploy is needed to do it. A config-backed copy would be a second list that drifts.
//!
//! **Refresh is lazy and traffic-driven**, never a timer: a request that finds the mirror older than
//! [`REFRESH_AFTER`] spawns one background re-fetch (single-flight) and is judged by the copy in
//! hand. A failed or malformed fetch keeps the last good copy; a 404 means nothing is published,
//! i.e. no minimums. Until a first fetch succeeds nothing is enforced (fail open).
//!
//! **Enforcement** ([`enforce`]): a `POST` whose `X-Pollis-Client-Platform` / `-Version` headers
//! name a build below its platform's minimum gets **426 Upgrade Required** with a typed
//! [`UpgradeRequiredBody`]. Everything else passes: requests without the headers (every build
//! older than #1611, operator tools), unknown platforms, and every `GET` (`/health`, `/version`,
//! the operator `/v1/ops/*`, `/v1/config`, `/v1/retention/metrics` and this mirror). The verifier
//! never talks to the DS.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use pollis_api::client_policy::{
    ClientPlatform, ClientPolicy, UpgradeRequiredBody, Verdict, CLIENT_PLATFORM_HEADER,
    CLIENT_VERSION_HEADER, UPGRADE_REQUIRED_ERROR,
};

use crate::AppState;

/// A request that finds the mirror older than this triggers one background re-fetch.
pub const REFRESH_AFTER: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Mirror {
    policy: Option<ClientPolicy>,
    /// When a fetch was last attempted (success or not), for [`REFRESH_AFTER`].
    attempted_at: Option<Instant>,
}

/// The DS's copy of the published policy. Cheap to clone (shared state).
#[derive(Clone, Default)]
pub struct PolicyMirror {
    url: Option<String>,
    mirror: Arc<RwLock<Mirror>>,
    refreshing: Arc<AtomicBool>,
}

impl PolicyMirror {
    /// Mirror `url`; `None` disables the check entirely (local dev, most tests).
    pub fn new(url: Option<String>) -> PolicyMirror {
        PolicyMirror { url: url.filter(|u| !u.trim().is_empty()), ..PolicyMirror::default() }
    }

    /// `POLLIS_CLIENT_POLICY_URL`; unset or empty disables the check.
    pub fn from_env() -> PolicyMirror {
        PolicyMirror::new(std::env::var("POLLIS_CLIENT_POLICY_URL").ok())
    }

    /// A fixed policy that never refreshes (tests).
    pub fn fixed(policy: ClientPolicy) -> PolicyMirror {
        let m = PolicyMirror::default();
        m.mirror.write().expect("policy lock").policy = Some(policy);
        m
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// The copy in hand, starting a background refresh if it is stale.
    pub fn current(&self) -> Option<ClientPolicy> {
        let (policy, stale) = {
            let m = self.mirror.read().expect("policy lock");
            let stale = m.attempted_at.is_none_or(|at| at.elapsed() >= REFRESH_AFTER);
            (m.policy.clone(), stale)
        };
        if stale && self.url.is_some() {
            self.spawn_refresh();
        }
        policy
    }

    fn spawn_refresh(&self) {
        if self.refreshing.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            this.refresh().await;
            this.refreshing.store(false, Ordering::Release);
        });
    }

    /// Fetch the published file once. Keeps the last good copy on any failure.
    pub async fn refresh(&self) {
        let Some(url) = self.url.clone() else {
            return;
        };
        let outcome = fetch(&url).await;
        let mut m = self.mirror.write().expect("policy lock");
        m.attempted_at = Some(Instant::now());
        match outcome {
            Ok(policy) => m.policy = Some(policy),
            Err(e) => tracing::warn!(error = %e, "client policy: refresh failed; keeping the last good copy"),
        }
    }
}

async fn fetch(url: &str) -> Result<ClientPolicy, String> {
    let resp = crate::util::http_get(crate::util::Upstream::ClientPolicy, url)
        .header(header::CACHE_CONTROL, "no-cache")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    // Nothing published yet (or it was removed): no minimums.
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(ClientPolicy::empty());
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    ClientPolicy::parse(&body).map_err(|e| e.to_string())
}

/// The platform and version a request declares, if it declares both and the platform is known.
fn declared(headers: &HeaderMap) -> Option<(ClientPlatform, String)> {
    let platform = headers.get(CLIENT_PLATFORM_HEADER)?.to_str().ok()?;
    let version = headers.get(CLIENT_VERSION_HEADER)?.to_str().ok()?;
    Some((ClientPlatform::parse(platform.trim())?, version.trim().to_string()))
}

/// Middleware: 426 a `POST` from a build below its platform's minimum; pass everything else.
pub async fn enforce(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if req.method() != Method::POST {
        return next.run(req).await;
    }
    let Some((platform, version)) = declared(req.headers()) else {
        return next.run(req).await;
    };
    let Some(policy) = state.client_policy.current() else {
        return next.run(req).await;
    };
    match policy.evaluate(platform, &version) {
        Verdict::Allowed => next.run(req).await,
        Verdict::Blocked { minimum } => {
            let body = UpgradeRequiredBody {
                error: UPGRADE_REQUIRED_ERROR.to_string(),
                platform,
                version,
                minimum,
                message: policy.message().map(str::to_string),
                policy,
            };
            (StatusCode::UPGRADE_REQUIRED, Json(body)).into_response()
        }
    }
}

/// `GET /v1/client-policy` — the policy this DS is enforcing. 404 when the check is not
/// configured, 503 until a first fetch has succeeded. Clients read the CDN file; this is for
/// confirming that a publish reached the DS.
pub async fn get_policy(State(state): State<AppState>) -> Response {
    if state.client_policy.url().is_none() && state.client_policy.mirror.read().expect("policy lock").policy.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match state.client_policy.current() {
        Some(policy) => ([(header::CACHE_CONTROL, "no-cache")], Json(policy)).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "client policy not loaded yet" })),
        )
            .into_response(),
    }
}
