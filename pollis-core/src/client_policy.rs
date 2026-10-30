//! The minimum-version check every client runs at launch (#1611).
//!
//! The rule and the manifest format live in [`pollis_api::client_policy`]; this module adds what
//! a client needs around them, once for desktop, mobile and the CLI:
//!
//! - **which build this is** ([`ClientIdentity`]), set by the host in [`Config::client`];
//! - **where the policy is** ([`policy_url`]): the prod or dev file on the CDN, by DS host;
//! - **the fetch**, bounded by [`FETCH_TIMEOUT`] and never retried in a loop;
//! - **the cache**, `client-policy.json` in the data dir: plain public data, readable before
//!   unlock (it is not in the SQLCipher DB), so a cached block survives going offline;
//! - **the decision** ([`resolve`]): a fresh fetch always wins, in either direction; a failed or
//!   malformed fetch falls back to the cache; with neither, the build is allowed (fail open).
//!
//! Nothing here touches local data beyond its own two files.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pollis_api::client_policy::{
    ClientPlatform, ClientPolicy, UpgradeRequiredBody, Verdict, DEV_POLICY_URL, PROD_POLICY_URL,
};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::state::AppState;

/// Upper bound on one manifest fetch. A few seconds: it holds the launch screen.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(4);
/// A non-forced re-check (window focus, app resume) within this long of the last fetch reuses it.
pub const RECHECK_MIN_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// The cached policy, in the data dir.
pub const CACHE_FILE: &str = "client-policy.json";
/// A dev build's persisted test override (see [`set_override`]).
pub const OVERRIDE_FILE: &str = "client-policy-override.json";

/// Which build is running: its policy key and its version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientIdentity {
    pub platform: ClientPlatform,
    pub version: String,
}

impl ClientIdentity {
    /// The desktop app: the OS it was built for and the release version stamped into the workspace.
    pub fn desktop() -> ClientIdentity {
        let platform = if cfg!(target_os = "macos") {
            ClientPlatform::Macos
        } else if cfg!(target_os = "windows") {
            ClientPlatform::Windows
        } else {
            ClientPlatform::Linux
        };
        ClientIdentity { platform, version: env!("CARGO_PKG_VERSION").to_string() }
    }

    /// The terminal client (`pollis`), on every OS.
    pub fn cli() -> ClientIdentity {
        ClientIdentity { platform: ClientPlatform::Cli, version: env!("CARGO_PKG_VERSION").to_string() }
    }

    /// From the mobile host's init config; `None` unless both parts are present and the platform is known.
    pub fn from_parts(platform: Option<&str>, version: Option<&str>) -> Option<ClientIdentity> {
        let platform = ClientPlatform::parse(platform?.trim())?;
        let version = version.map(str::trim).filter(|v| !v.is_empty())?;
        Some(ClientIdentity { platform, version: version.to_string() })
    }
}

/// Where a [`ClientPolicyStatus`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicySource {
    /// Fetched just now.
    Network,
    /// The fetch failed or was malformed; this is the last policy fetched.
    Cache,
    /// The DS answered 426 to one of this build's requests.
    Ds,
    /// Nothing to go on (no identity, or no fetch and no cache): allowed.
    None,
}

/// The verdict the UI renders. Mirrored in `frontend/src/types` and `mobile/lib/clientPolicy.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPolicyStatus {
    pub blocked: bool,
    pub platform: Option<ClientPlatform>,
    /// This build's version.
    pub version: Option<String>,
    /// The platform's minimum, when one is set.
    pub minimum: Option<String>,
    /// The operator's message from the manifest, if any; otherwise the UI's built-in text.
    pub message: Option<String>,
    pub source: PolicySource,
}

impl ClientPolicyStatus {
    fn allowed(identity: Option<&ClientIdentity>, source: PolicySource) -> ClientPolicyStatus {
        ClientPolicyStatus {
            blocked: false,
            platform: identity.map(|i| i.platform),
            version: identity.map(|i| i.version.clone()),
            minimum: None,
            message: None,
            source,
        }
    }

    fn from_policy(identity: &ClientIdentity, policy: &ClientPolicy, source: PolicySource) -> ClientPolicyStatus {
        let verdict = policy.evaluate(identity.platform, &identity.version);
        ClientPolicyStatus {
            blocked: verdict.is_blocked(),
            platform: Some(identity.platform),
            version: Some(identity.version.clone()),
            minimum: policy.minimum_for(identity.platform).map(str::to_string),
            message: match verdict {
                Verdict::Blocked { .. } => policy.message().map(str::to_string),
                Verdict::Allowed => None,
            },
            source,
        }
    }
}

/// The per-process state behind [`AppState::client_policy`].
#[derive(Debug, Default)]
pub struct PolicyRuntime {
    /// The last verdict; `None` before the first check.
    pub status: Option<ClientPolicyStatus>,
    /// When the last fetch was attempted, for [`RECHECK_MIN_INTERVAL`].
    pub last_fetch: Option<Instant>,
}

/// THE decision, pure: a fresh policy wins in either direction; else the cached one; else allowed.
pub fn resolve(
    identity: Option<&ClientIdentity>,
    fresh: Option<&ClientPolicy>,
    cached: Option<&ClientPolicy>,
) -> ClientPolicyStatus {
    let Some(identity) = identity else {
        return ClientPolicyStatus::allowed(None, PolicySource::None);
    };
    match (fresh, cached) {
        (Some(policy), _) => ClientPolicyStatus::from_policy(identity, policy, PolicySource::Network),
        (None, Some(policy)) => ClientPolicyStatus::from_policy(identity, policy, PolicySource::Cache),
        (None, None) => ClientPolicyStatus::allowed(Some(identity), PolicySource::None),
    }
}

/// The policy URL for this build: [`Config::client_policy_url`] if set, else the dev file for a
/// non-prod DS and the prod file otherwise.
pub fn policy_url(config: &Config) -> String {
    if let Some(url) = config.client_policy_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        return url.to_string();
    }
    if config.ds_is_prod() {
        PROD_POLICY_URL.to_string()
    } else {
        DEV_POLICY_URL.to_string()
    }
}

/// How a fetch may leave the device; mirrors the updater's overlay plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchPlan {
    Direct,
    Proxy(String),
    /// Strict overlay with no circuit: going direct would leak the address, so do not fetch.
    Skip,
}

impl FetchPlan {
    /// The live plan for `state`, the same decision the desktop updater uses.
    pub fn for_state(state: &AppState) -> FetchPlan {
        let handle = state.overlay_handle();
        let mode = handle.as_ref().map(|h| h.mode()).unwrap_or(pollis_relay::OverlayMode::Off);
        let addr = handle.as_ref().map(|h| h.socks_addr());
        match crate::commands::update::plan_update_check(mode, addr) {
            crate::commands::update::UpdateCheckPlan::Direct => FetchPlan::Direct,
            crate::commands::update::UpdateCheckPlan::Proxy { url } => FetchPlan::Proxy(url),
            crate::commands::update::UpdateCheckPlan::Blocked { .. } => FetchPlan::Skip,
        }
    }
}

/// Fetch and parse the policy once. Any failure (transport, timeout, non-2xx, malformed) is `Err`.
pub async fn fetch(url: &str, plan: &FetchPlan, timeout: Duration) -> Result<ClientPolicy, String> {
    if let Some(json) = url.strip_prefix("inline:") {
        return ClientPolicy::parse(json).map_err(|e| e.to_string());
    }
    let mut builder = reqwest::Client::builder().timeout(timeout).connect_timeout(timeout);
    match plan {
        FetchPlan::Direct => {}
        FetchPlan::Proxy(proxy) => {
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|e| e.to_string())?);
        }
        FetchPlan::Skip => return Err("skipped: strict overlay with no circuit".into()),
    }
    let client = builder.build().map_err(|e| e.to_string())?;
    let resp = client
        .get(url)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send()
        .await
        .map_err(|e| crate::commands::mls::ds_client::error_chain(&e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    ClientPolicy::parse(&body).map_err(|e| e.to_string())
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    policy: ClientPolicy,
    fetched_at: u64,
}

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILE)
}

/// The cached policy, or `None` if there is none or it does not parse.
pub fn read_cache(data_dir: &Path) -> Option<ClientPolicy> {
    let raw = std::fs::read(cache_path(data_dir)).ok()?;
    let file: CacheFile = serde_json::from_slice(&raw).ok()?;
    // Re-validate: a hand-edited or truncated cache must not block anything.
    let json = serde_json::to_string(&file.policy).ok()?;
    ClientPolicy::parse(&json).ok()
}

/// Replace the cached policy. Best-effort: a failure only loses offline blocking.
pub fn write_cache(data_dir: &Path, policy: &ClientPolicy) {
    let file = CacheFile { policy: policy.clone(), fetched_at: crate::util::now_unix() };
    let Ok(json) = serde_json::to_vec(&file) else {
        return;
    };
    if let Err(e) = crate::private_fs::create_dir_all(data_dir)
        .and_then(|_| crate::private_fs::write(&cache_path(data_dir), json))
    {
        crate::diag!("[client-policy] cache not written: {e}");
    }
}

/// A dev build's test override: a URL to fetch instead, or a policy to use as if fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyOverride {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub policy: Option<String>,
}

impl PolicyOverride {
    fn source(&self) -> Option<String> {
        if let Some(policy) = self.policy.as_deref().filter(|p| !p.trim().is_empty()) {
            return Some(format!("inline:{policy}"));
        }
        self.url.as_deref().map(str::trim).filter(|u| !u.is_empty()).map(str::to_string)
    }
}

/// Persist (or with `None`, clear) the test override. Refused for a build that talks to the prod DS.
pub fn set_override(config: &Config, data_dir: &Path, value: Option<&PolicyOverride>) -> crate::error::Result<()> {
    if config.ds_is_prod() {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "the client policy override is for dev builds only"
        )));
    }
    let path = data_dir.join(OVERRIDE_FILE);
    match value {
        Some(value) => {
            let json = serde_json::to_vec(value).map_err(|e| crate::error::Error::Other(e.into()))?;
            crate::private_fs::create_dir_all(data_dir)
                .and_then(|_| crate::private_fs::write(&path, json))
                .map_err(|e| crate::error::Error::Other(e.into()))
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(crate::error::Error::Other(e.into())),
        },
    }
}

/// The override in effect, if any; never for a build that talks to the prod DS.
fn read_override(config: &Config, data_dir: &Path) -> Option<String> {
    if config.ds_is_prod() {
        return None;
    }
    let raw = std::fs::read(data_dir.join(OVERRIDE_FILE)).ok()?;
    serde_json::from_slice::<PolicyOverride>(&raw).ok()?.source()
}

/// Run the check without an [`AppState`] (the CLI, before it opens anything): fetch, fall back to
/// the cache, decide. A fresh policy replaces the cache.
pub async fn check(config: &Config, data_dir: &Path, plan: &FetchPlan, timeout: Duration) -> ClientPolicyStatus {
    let identity = config.client.as_ref();
    if identity.is_none() {
        return resolve(None, None, None);
    }
    // The env override (`POLLIS_CLIENT_POLICY_URL`) beats the dev file override.
    let source = match config.client_policy_url.as_deref().filter(|u| !u.trim().is_empty()) {
        Some(_) => policy_url(config),
        None => read_override(config, data_dir).unwrap_or_else(|| policy_url(config)),
    };
    let fresh = match fetch(&source, plan, timeout).await {
        Ok(policy) => {
            write_cache(data_dir, &policy);
            Some(policy)
        }
        Err(e) => {
            crate::diag!("[client-policy] fetch failed ({e}); using the cached policy if any");
            None
        }
    };
    let cached = if fresh.is_none() { read_cache(data_dir) } else { None };
    resolve(identity, fresh.as_ref(), cached.as_ref())
}

/// Where the cache and the override live: the data dir. Unit tests use a temp dir instead, so they
/// never write into a developer's real Pollis data.
pub(crate) fn data_dir() -> PathBuf {
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("pollis-client-policy-test-{}", std::process::id()))
    }
    #[cfg(not(test))]
    {
        crate::db::local::dirs_path()
    }
}

/// [`check`] for a running app: throttled unless `force`, and the verdict is recorded on `state`
/// so DS requests from a blocked build fail locally.
pub async fn check_state(state: &Arc<AppState>, force: bool) -> ClientPolicyStatus {
    {
        let rt = state.client_policy.lock().expect("client policy lock");
        if !force {
            if let (Some(status), Some(at)) = (&rt.status, rt.last_fetch) {
                if at.elapsed() < RECHECK_MIN_INTERVAL {
                    return status.clone();
                }
            }
        }
    }
    let data_dir = data_dir();
    let plan = FetchPlan::for_state(state);
    let status = check(&state.config, &data_dir, &plan, FETCH_TIMEOUT).await;
    let mut rt = state.client_policy.lock().expect("client policy lock");
    rt.last_fetch = Some(Instant::now());
    rt.status = Some(status.clone());
    status
}

/// The last verdict without any network; allowed before the first check.
pub fn current(state: &AppState) -> ClientPolicyStatus {
    state
        .client_policy
        .lock()
        .expect("client policy lock")
        .status
        .clone()
        .unwrap_or_else(|| ClientPolicyStatus::allowed(state.config.client.as_ref(), PolicySource::None))
}

/// `Err(UpgradeRequired)` once this build is known to be blocked; every DS sender calls it first.
pub fn refuse_if_blocked(state: &AppState) -> crate::error::Result<()> {
    let blocked = state
        .client_policy
        .lock()
        .map(|rt| rt.status.as_ref().is_some_and(|s| s.blocked))
        .unwrap_or(false);
    if blocked {
        return Err(crate::error::Error::UpgradeRequired);
    }
    Ok(())
}

/// Record a DS `426 Upgrade Required`: block this process and cache the DS's policy so the block
/// holds offline and across launches. An unreadable body still blocks: the DS refused us.
pub fn record_upgrade_required(state: &AppState, body: &str) -> ClientPolicyStatus {
    let identity = state.config.client.as_ref();
    let status = match serde_json::from_str::<UpgradeRequiredBody>(body) {
        Ok(parsed) => {
            write_cache(&data_dir(), &parsed.policy);
            ClientPolicyStatus {
                blocked: true,
                platform: Some(parsed.platform),
                version: Some(parsed.version),
                minimum: Some(parsed.minimum),
                message: parsed.message.filter(|m| !m.trim().is_empty()),
                source: PolicySource::Ds,
            }
        }
        Err(_) => ClientPolicyStatus {
            blocked: true,
            platform: identity.map(|i| i.platform),
            version: identity.map(|i| i.version.clone()),
            minimum: None,
            message: None,
            source: PolicySource::Ds,
        },
    };
    if let Ok(mut rt) = state.client_policy.lock() {
        rt.status = Some(status.clone());
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn id(platform: ClientPlatform, version: &str) -> ClientIdentity {
        ClientIdentity { platform, version: version.into() }
    }

    fn policy(minimum: &[(&str, &str)], message: Option<&str>) -> ClientPolicy {
        ClientPolicy {
            v: 1,
            minimum: minimum.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            message: message.map(str::to_string),
        }
    }

    fn mac(version: &str) -> ClientIdentity {
        id(ClientPlatform::Macos, version)
    }

    #[test]
    fn identities() {
        assert_eq!(ClientIdentity::cli().platform, ClientPlatform::Cli);
        assert_eq!(ClientIdentity::cli().version, env!("CARGO_PKG_VERSION"));
        assert_ne!(ClientIdentity::desktop().platform, ClientPlatform::Cli);
        assert_eq!(
            ClientIdentity::from_parts(Some("android-play"), Some(" 1.0.21 ")),
            Some(id(ClientPlatform::AndroidPlay, "1.0.21"))
        );
        assert_eq!(ClientIdentity::from_parts(Some("android"), Some("1.0.21")), None);
        assert_eq!(ClientIdentity::from_parts(Some("ios"), Some("")), None);
        assert_eq!(ClientIdentity::from_parts(None, Some("1.0.0")), None);
    }

    /// The issue's state matrix, at the decision.
    #[test]
    fn the_matrix() {
        let floor = policy(&[("macos", "1.17.0")], None);
        let open = policy(&[], None);

        // Up to date.
        let s = resolve(Some(&mac("1.18.0")), Some(&floor), None);
        assert!(!s.blocked);
        assert_eq!(s.source, PolicySource::Network);
        assert_eq!(s.minimum.as_deref(), Some("1.17.0"));
        // Exactly at the minimum: allowed.
        assert!(!resolve(Some(&mac("1.17.0")), Some(&floor), None).blocked);
        // Below: blocked.
        let s = resolve(Some(&mac("1.16.0")), Some(&floor), None);
        assert!(s.blocked);
        assert_eq!(s.version.as_deref(), Some("1.16.0"));
        // Unreachable, no cache: allowed.
        let s = resolve(Some(&mac("1.16.0")), None, None);
        assert!(!s.blocked);
        assert_eq!(s.source, PolicySource::None);
        // Unreachable, cached block: blocked.
        let s = resolve(Some(&mac("1.16.0")), None, Some(&floor));
        assert!(s.blocked);
        assert_eq!(s.source, PolicySource::Cache);
        // The cache says blocked, a fresh fetch says allowed: allowed.
        assert!(!resolve(Some(&mac("1.16.0")), Some(&open), Some(&floor)).blocked);
        // ...and the other direction: a fresh block beats a permissive cache.
        assert!(resolve(Some(&mac("1.16.0")), Some(&floor), Some(&open)).blocked);
        // No identity (a build that does not say what it is): never blocked.
        assert!(!resolve(None, Some(&floor), Some(&floor)).blocked);
    }

    #[test]
    fn the_message_shows_only_when_blocked() {
        let p = policy(&[("ios", "1.0.21")], Some("Version 1.0.20 loses drafts."));
        let blocked = resolve(Some(&id(ClientPlatform::Ios, "1.0.20")), Some(&p), None);
        assert_eq!(blocked.message.as_deref(), Some("Version 1.0.20 loses drafts."));
        let allowed = resolve(Some(&id(ClientPlatform::Ios, "1.0.21")), Some(&p), None);
        assert_eq!(allowed.message, None);
    }

    fn config(ds: Option<&str>) -> Config {
        let mut c = Config::for_test().unwrap();
        c.pollis_delivery_url = ds.map(str::to_string);
        c.client = Some(mac("1.16.0"));
        c
    }

    #[test]
    fn the_policy_url_follows_the_ds() {
        assert_eq!(policy_url(&config(Some("https://api.pollis.com"))), PROD_POLICY_URL);
        assert_eq!(policy_url(&config(Some("https://api-dev.pollis.com"))), DEV_POLICY_URL);
        assert_eq!(policy_url(&config(None)), PROD_POLICY_URL);
        let mut c = config(Some("https://api.pollis.com"));
        c.client_policy_url = Some("http://127.0.0.1:9/p.json".into());
        assert_eq!(policy_url(&c), "http://127.0.0.1:9/p.json");
    }

    #[test]
    fn the_cache_round_trips_and_refuses_garbage() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_cache(dir.path()), None);
        let p = policy(&[("linux", "1.17.0")], Some("hi"));
        write_cache(dir.path(), &p);
        assert_eq!(read_cache(dir.path()), Some(p));
        std::fs::write(dir.path().join(CACHE_FILE), b"{\"policy\":{\"v\":1,\"minimum\":{\"linux\":\"x\"}},\"fetched_at\":0}").unwrap();
        assert_eq!(read_cache(dir.path()), None);
        std::fs::write(dir.path().join(CACHE_FILE), b"garbage").unwrap();
        assert_eq!(read_cache(dir.path()), None);
    }

    /// A one-route server answering `status` + `body`, counting hits; `hang` never answers.
    async fn serve(status: u16, body: &'static str, hang: bool) -> (String, Arc<AtomicUsize>) {
        use axum::{http::StatusCode, routing::get, Router};
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let app = Router::new().route(
            "/p.json",
            get(move || {
                let h = h.clone();
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    if hang {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    }
                    (StatusCode::from_u16(status).unwrap(), body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://127.0.0.1:{port}/p.json"), hits)
    }

    async fn run(url: &str, dir: &Path, version: &str) -> ClientPolicyStatus {
        let mut c = config(Some("https://api-dev.pollis.com"));
        c.client = Some(mac(version));
        c.client_policy_url = Some(url.to_string());
        check(&c, dir, &FetchPlan::Direct, Duration::from_millis(500)).await
    }

    const FLOOR: &str = r#"{"v":1,"minimum":{"macos":"1.17.0"},"message":null}"#;
    const OPEN: &str = r#"{"v":1,"minimum":{},"message":null}"#;

    /// The matrix end to end: fetch, cache, fall back, override.
    #[tokio::test]
    async fn the_matrix_over_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let (floor, _) = serve(200, FLOOR, false).await;
        let (open, _) = serve(200, OPEN, false).await;
        let (malformed, _) = serve(200, r#"{"v":1,"minimum":{"macos":"soon"}}"#, false).await;
        let (missing, _) = serve(404, "", false).await;
        let (hanging, hits) = serve(200, FLOOR, true).await;
        let unreachable = "http://127.0.0.1:9/p.json";

        // No cache yet: unreachable, 404, timeout and malformed all fail open.
        for url in [unreachable, missing.as_str(), malformed.as_str()] {
            let s = run(url, dir.path(), "1.16.0").await;
            assert!(!s.blocked, "{url}");
            assert_eq!(s.source, PolicySource::None, "{url}");
        }
        let started = Instant::now();
        assert!(!run(&hanging, dir.path(), "1.16.0").await.blocked);
        assert!(started.elapsed() < Duration::from_secs(3), "the timeout bounds the fetch");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "a timed-out fetch is never retried");

        // At and above the minimum; below it.
        assert!(!run(&floor, dir.path(), "1.17.0").await.blocked);
        assert!(!run(&floor, dir.path(), "1.18.1").await.blocked);
        let s = run(&floor, dir.path(), "1.16.0").await;
        assert!(s.blocked);
        assert_eq!(s.source, PolicySource::Network);

        // The block is cached: offline, a timeout or a malformed manifest keep blocking.
        for url in [unreachable, malformed.as_str(), missing.as_str()] {
            let s = run(url, dir.path(), "1.16.0").await;
            assert!(s.blocked, "{url}");
            assert_eq!(s.source, PolicySource::Cache, "{url}");
        }
        // A fresh fetch that allows it unblocks, and replaces the cache.
        assert!(!run(&open, dir.path(), "1.16.0").await.blocked);
        assert!(!run(unreachable, dir.path(), "1.16.0").await.blocked);
        // An upgraded build is allowed even by a cached block.
        write_cache(dir.path(), &ClientPolicy::parse(FLOOR).unwrap());
        assert!(!run(unreachable, dir.path(), "1.17.0").await.blocked);
    }

    #[tokio::test]
    async fn the_dev_override_is_used_and_refused_for_prod() {
        let dir = tempfile::tempdir().unwrap();
        let dev = config(Some("http://127.0.0.1:9"));
        let ov = PolicyOverride { url: None, policy: Some(FLOOR.into()) };
        set_override(&dev, dir.path(), Some(&ov)).unwrap();
        let s = check(&dev, dir.path(), &FetchPlan::Direct, Duration::from_millis(200)).await;
        assert!(s.blocked);
        assert_eq!(s.source, PolicySource::Network);

        // A prod build neither writes nor reads one.
        let prod = config(Some("https://api.pollis.com"));
        assert!(set_override(&prod, dir.path(), Some(&ov)).is_err());
        assert_eq!(read_override(&prod, dir.path()), None);

        // Cleared: the cached block from the override holds until a real fetch says otherwise.
        set_override(&dev, dir.path(), None).unwrap();
        set_override(&dev, dir.path(), None).unwrap();
        let mut offline = dev.clone();
        offline.client_policy_url = Some("http://127.0.0.1:9/p.json".into());
        let s = check(&offline, dir.path(), &FetchPlan::Direct, Duration::from_millis(200)).await;
        assert!(s.blocked);
        assert_eq!(s.source, PolicySource::Cache);
    }

    #[tokio::test]
    async fn a_strict_overlay_without_a_circuit_never_fetches() {
        let (url, hits) = serve(200, FLOOR, false).await;
        assert!(fetch(&url, &FetchPlan::Skip, FETCH_TIMEOUT).await.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }
}
