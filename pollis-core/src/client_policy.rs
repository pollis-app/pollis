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

/// The published policy for this build's DS: the dev file for a non-prod DS, the prod file otherwise.
pub fn published_url(config: &Config) -> String {
    if config.ds_is_prod() {
        PROD_POLICY_URL.to_string()
    } else {
        DEV_POLICY_URL.to_string()
    }
}

/// Whether a test override may be honoured: never by a release build talking to the prod DS.
pub fn overrides_allowed(ds_is_prod: bool, debug_build: bool) -> bool {
    !ds_is_prod || debug_build
}

/// The policy URL for this build: [`Config::client_policy_url`]
/// (`POLLIS_CLIENT_POLICY_URL_OVERRIDE`) where overrides are allowed, else [`published_url`].
pub fn policy_url(config: &Config) -> String {
    if overrides_allowed(config.ds_is_prod(), cfg!(debug_assertions)) {
        if let Some(url) = config.client_policy_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            return url.to_string();
        }
    }
    published_url(config)
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

/// Why a fetch gave no policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// Transport, timeout, a non-2xx other than 404, or a malformed body: fall back to the cache.
    Failed(String),
    /// A format this build does not understand (a future `v`): its rule is unknowable here, so
    /// the cache is dropped too, and the build is allowed.
    Unsupported(u32),
}

fn parse_body(json: &str) -> Result<ClientPolicy, FetchError> {
    ClientPolicy::parse(json).map_err(|e| match e {
        pollis_api::client_policy::PolicyError::UnsupportedFormat(v) => FetchError::Unsupported(v),
        other => FetchError::Failed(other.to_string()),
    })
}

/// Fetch and parse the policy once. A 404 is "nothing published": no minimums, as on the DS.
pub async fn fetch(url: &str, plan: &FetchPlan, timeout: Duration) -> Result<ClientPolicy, FetchError> {
    let failed = FetchError::Failed;
    if let Some(json) = url.strip_prefix("inline:") {
        return parse_body(json);
    }
    let mut builder = reqwest::Client::builder().timeout(timeout).connect_timeout(timeout);
    match plan {
        FetchPlan::Direct => {}
        FetchPlan::Proxy(proxy) => {
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|e| failed(e.to_string()))?);
        }
        FetchPlan::Skip => return Err(failed("skipped: strict overlay with no circuit".into())),
    }
    let client = builder.build().map_err(|e| failed(e.to_string()))?;
    let resp = client
        .get(url)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send()
        .await
        .map_err(|e| failed(crate::commands::mls::ds_client::error_chain(&e)))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(ClientPolicy::empty());
    }
    if !resp.status().is_success() {
        return Err(failed(format!("HTTP {}", resp.status())));
    }
    let body = resp.text().await.map_err(|e| failed(e.to_string()))?;
    parse_body(&body)
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    policy: ClientPolicy,
    fetched_at: u64,
    /// Where the policy came from; the cache answers only for that same source, so a test
    /// override's verdict never outlives it. Files without it read as `None`: ignored.
    #[serde(default)]
    source: Option<String>,
}

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CACHE_FILE)
}

/// The policy cached for `source`, or `None` if there is none, it is for another source, or it
/// does not parse.
pub fn read_cache(data_dir: &Path, source: &str) -> Option<ClientPolicy> {
    let raw = std::fs::read(cache_path(data_dir)).ok()?;
    let file: CacheFile = serde_json::from_slice(&raw).ok()?;
    if file.source.as_deref() != Some(source) {
        return None;
    }
    // Re-validate: a hand-edited or truncated cache must not block anything.
    let json = serde_json::to_string(&file.policy).ok()?;
    ClientPolicy::parse(&json).ok()
}

/// Replace the cached policy for `source`. Best-effort: a failure only loses offline blocking.
pub fn write_cache(data_dir: &Path, source: &str, policy: &ClientPolicy) {
    let file = CacheFile {
        policy: policy.clone(),
        fetched_at: crate::util::now_unix(),
        source: Some(source.to_string()),
    };
    let Ok(json) = serde_json::to_vec(&file) else {
        return;
    };
    if let Err(e) = crate::private_fs::create_dir_all(data_dir)
        .and_then(|_| crate::private_fs::write(&cache_path(data_dir), json))
    {
        crate::diag!("[client-policy] cache not written: {e}");
    }
}

/// Drop the cache (a policy format this build cannot read replaced it).
pub fn clear_cache(data_dir: &Path) {
    let _ = std::fs::remove_file(cache_path(data_dir));
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
    if !overrides_allowed(config.ds_is_prod(), cfg!(debug_assertions)) {
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
    if !overrides_allowed(config.ds_is_prod(), cfg!(debug_assertions)) {
        return None;
    }
    let raw = std::fs::read(data_dir.join(OVERRIDE_FILE)).ok()?;
    serde_json::from_slice::<PolicyOverride>(&raw).ok()?.source()
}

/// Where this build reads its policy, and whether that answer may be cached.
fn source_for(config: &Config, data_dir: &Path) -> (String, bool) {
    // The env override beats the dev file override; both are gated by [`overrides_allowed`].
    let env = policy_url(config);
    if env != published_url(config) {
        // An env URL is cached under its own name; `inline:` never is.
        let cacheable = !env.starts_with("inline:");
        return (env, cacheable);
    }
    match read_override(config, data_dir) {
        // A file override's verdict is never cached: it must not outlive the override.
        Some(source) => (source, false),
        None => (env, true),
    }
}

/// The verdict from the cache alone, no network: launch uses it to decide whether it can go on
/// while a fresh check runs in the background.
pub fn peek(config: &Config, data_dir: &Path) -> ClientPolicyStatus {
    let identity = config.client.as_ref();
    if identity.is_none() {
        return resolve(None, None, None);
    }
    let (source, cacheable) = source_for(config, data_dir);
    let cached = if cacheable { read_cache(data_dir, &source) } else { None };
    resolve(identity, None, cached.as_ref())
}

/// Run the check without an [`AppState`] (the CLI, before it opens anything): fetch, fall back to
/// the cache, decide. A fresh policy replaces the cache.
pub async fn check(config: &Config, data_dir: &Path, plan: &FetchPlan, timeout: Duration) -> ClientPolicyStatus {
    let identity = config.client.as_ref();
    if identity.is_none() {
        return resolve(None, None, None);
    }
    let (source, cacheable) = source_for(config, data_dir);
    let fresh = match fetch(&source, plan, timeout).await {
        Ok(policy) => {
            if cacheable {
                write_cache(data_dir, &source, &policy);
            }
            Some(policy)
        }
        Err(FetchError::Unsupported(v)) => {
            crate::diag!("[client-policy] format v{v} is not supported by this build; allowed, cache dropped");
            clear_cache(data_dir);
            return resolve(identity, Some(&ClientPolicy::empty()), None);
        }
        Err(FetchError::Failed(e)) => {
            crate::diag!("[client-policy] fetch failed ({e}); using the cached policy if any");
            None
        }
    };
    let cached = if fresh.is_none() && cacheable { read_cache(data_dir, &source) } else { None };
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

/// [`peek`] for a running app.
pub fn peek_state(state: &AppState) -> ClientPolicyStatus {
    peek(&state.config, &data_dir())
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
/// holds offline and across launches. Only the DS's typed body counts (`error ==
/// "upgrade_required"`); any other 426 (a proxy, a misconfigured edge) is an ordinary error, and
/// `None` is returned.
pub fn record_upgrade_required(state: &AppState, body: &str) -> Option<ClientPolicyStatus> {
    let parsed = serde_json::from_str::<UpgradeRequiredBody>(body).ok()?;
    if parsed.error != pollis_api::client_policy::UPGRADE_REQUIRED_ERROR {
        return None;
    }
    // The DS enforces the published policy for its own host, so cache it under that source.
    write_cache(&data_dir(), &published_url(&state.config), &parsed.policy);
    let status = ClientPolicyStatus {
        blocked: true,
        platform: Some(parsed.platform),
        version: Some(parsed.version),
        minimum: Some(parsed.minimum),
        message: parsed.message.filter(|m| !m.trim().is_empty()),
        source: PolicySource::Ds,
    };
    if let Ok(mut rt) = state.client_policy.lock() {
        rt.status = Some(status.clone());
    }
    Some(status)
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
        let mut c = config(Some("https://api-dev.pollis.com"));
        c.client_policy_url = Some("http://127.0.0.1:9/p.json".into());
        assert_eq!(policy_url(&c), "http://127.0.0.1:9/p.json");
        assert_eq!(published_url(&c), DEV_POLICY_URL);
    }

    /// Overrides (the env URL and the dev file) only for a debug build or a non-prod DS.
    #[test]
    fn overrides_are_refused_by_a_release_build_on_the_prod_ds() {
        assert!(!overrides_allowed(true, false));
        assert!(overrides_allowed(true, true));
        assert!(overrides_allowed(false, false));
        assert!(overrides_allowed(false, true));
    }

    #[test]
    fn the_cache_round_trips_and_refuses_garbage() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_cache(dir.path(), PROD_POLICY_URL), None);
        let p = policy(&[("linux", "1.17.0")], Some("hi"));
        write_cache(dir.path(), PROD_POLICY_URL, &p);
        assert_eq!(read_cache(dir.path(), PROD_POLICY_URL), Some(p));
        std::fs::write(
            dir.path().join(CACHE_FILE),
            format!(r#"{{"policy":{{"v":1,"minimum":{{"linux":"x"}}}},"fetched_at":0,"source":"{PROD_POLICY_URL}"}}"#),
        )
        .unwrap();
        assert_eq!(read_cache(dir.path(), PROD_POLICY_URL), None);
        std::fs::write(dir.path().join(CACHE_FILE), b"garbage").unwrap();
        assert_eq!(read_cache(dir.path(), PROD_POLICY_URL), None);
    }

    /// The cache answers only for the source it was written from; an unlabelled one never.
    #[test]
    fn the_cache_is_ignored_for_another_source() {
        let dir = tempfile::tempdir().unwrap();
        let floor = policy(&[("macos", "9.0.0")], None);
        write_cache(dir.path(), "http://127.0.0.1:9/override.json", &floor);
        assert_eq!(read_cache(dir.path(), PROD_POLICY_URL), None);
        assert_eq!(read_cache(dir.path(), "http://127.0.0.1:9/override.json"), Some(floor.clone()));
        std::fs::write(dir.path().join(CACHE_FILE), br#"{"policy":{"v":1,"minimum":{"macos":"9.0.0"}},"fetched_at":0}"#).unwrap();
        assert_eq!(read_cache(dir.path(), PROD_POLICY_URL), None);
        // So an env override's block never blocks the published source.
        let mut c = config(Some("https://api-dev.pollis.com"));
        c.client_policy_url = Some("http://127.0.0.1:9/override.json".into());
        write_cache(dir.path(), "http://127.0.0.1:9/override.json", &floor);
        assert!(peek(&c, dir.path()).blocked);
        c.client_policy_url = None;
        assert!(!peek(&c, dir.path()).blocked);
    }

    /// What one switchable route answers: status, body, and whether it hangs.
    type Reply = Arc<std::sync::Mutex<(u16, String, bool)>>;

    /// A one-route server answering whatever `reply` holds at each request, counting hits.
    async fn serve(status: u16, body: &str) -> (String, Reply, Arc<AtomicUsize>) {
        use axum::{http::StatusCode, routing::get, Router};
        let hits = Arc::new(AtomicUsize::new(0));
        let reply: Reply = Arc::new(std::sync::Mutex::new((status, body.to_string(), false)));
        let (h, r) = (hits.clone(), reply.clone());
        let app = Router::new().route(
            "/p.json",
            get(move || {
                let (h, r) = (h.clone(), r.clone());
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    let (status, body, hang) = r.lock().unwrap().clone();
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
        (format!("http://127.0.0.1:{port}/p.json"), reply, hits)
    }

    fn set(reply: &Reply, status: u16, body: &str, hang: bool) {
        *reply.lock().unwrap() = (status, body.to_string(), hang);
    }

    async fn run(url: &str, dir: &Path, version: &str) -> ClientPolicyStatus {
        let mut c = config(Some("https://api-dev.pollis.com"));
        c.client = Some(mac(version));
        c.client_policy_url = Some(url.to_string());
        check(&c, dir, &FetchPlan::Direct, Duration::from_millis(500)).await
    }

    const FLOOR: &str = r#"{"v":1,"minimum":{"macos":"1.17.0"},"message":null}"#;
    const OPEN: &str = r#"{"v":1,"minimum":{},"message":null}"#;
    const MALFORMED: &str = r#"{"v":1,"minimum":{"macos":"soon"}}"#;
    const FUTURE: &str = r#"{"v":2,"minimum":{"macos":"99.0.0"}}"#;

    /// The matrix end to end, against one URL whose answer changes: fetch, cache, fall back.
    #[tokio::test]
    async fn the_matrix_over_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let (url, reply, hits) = serve(500, "").await;

        // No cache yet: a server error, a timeout and a malformed manifest all fail open.
        for (status, body) in [(500, ""), (200, MALFORMED)] {
            set(&reply, status, body, false);
            let s = run(&url, dir.path(), "1.16.0").await;
            assert!(!s.blocked, "{status} {body}");
            assert_eq!(s.source, PolicySource::None, "{status} {body}");
        }
        set(&reply, 200, FLOOR, true);
        let before = hits.load(Ordering::SeqCst);
        let started = Instant::now();
        assert!(!run(&url, dir.path(), "1.16.0").await.blocked);
        assert!(started.elapsed() < Duration::from_secs(3), "the timeout bounds the fetch");
        assert_eq!(hits.load(Ordering::SeqCst), before + 1, "a timed-out fetch is never retried");

        // At and above the minimum; below it.
        set(&reply, 200, FLOOR, false);
        assert!(!run(&url, dir.path(), "1.17.0").await.blocked);
        assert!(!run(&url, dir.path(), "1.18.1").await.blocked);
        let s = run(&url, dir.path(), "1.16.0").await;
        assert!(s.blocked);
        assert_eq!(s.source, PolicySource::Network);

        // The block is cached: a server error, a timeout or a malformed manifest keep blocking.
        for (status, body, hang) in [(500, "", false), (200, MALFORMED, false), (200, OPEN, true)] {
            set(&reply, status, body, hang);
            let s = run(&url, dir.path(), "1.16.0").await;
            assert!(s.blocked, "{status} {body}");
            assert_eq!(s.source, PolicySource::Cache, "{status} {body}");
        }
        // A fresh fetch that allows it unblocks, and replaces the cache.
        set(&reply, 200, OPEN, false);
        assert!(!run(&url, dir.path(), "1.16.0").await.blocked);
        set(&reply, 500, "", false);
        assert!(!run(&url, dir.path(), "1.16.0").await.blocked);
        // An upgraded build is allowed even by a cached block.
        write_cache(dir.path(), &url, &ClientPolicy::parse(FLOOR).unwrap());
        assert!(!run(&url, dir.path(), "1.17.0").await.blocked);
    }

    /// A 404 is "nothing published": no minimums, and it replaces a cached block (as on the DS).
    #[tokio::test]
    async fn a_404_means_no_minimums_and_lifts_a_cached_block() {
        let dir = tempfile::tempdir().unwrap();
        let (url, reply, _) = serve(200, FLOOR).await;
        assert!(run(&url, dir.path(), "1.16.0").await.blocked);
        set(&reply, 404, "", false);
        let s = run(&url, dir.path(), "1.16.0").await;
        assert!(!s.blocked);
        assert_eq!(s.source, PolicySource::Network);
        set(&reply, 500, "", false);
        assert!(!run(&url, dir.path(), "1.16.0").await.blocked, "the 404 replaced the cache");
    }

    /// A format this build cannot read is allowed and drops the cache, so a block cannot stick.
    #[tokio::test]
    async fn a_future_format_allows_and_clears_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let (url, reply, _) = serve(200, FLOOR).await;
        assert!(run(&url, dir.path(), "1.16.0").await.blocked);
        set(&reply, 200, FUTURE, false);
        assert!(!run(&url, dir.path(), "1.16.0").await.blocked);
        assert!(!dir.path().join(CACHE_FILE).exists());
        set(&reply, 500, "", false);
        assert!(!run(&url, dir.path(), "1.16.0").await.blocked);
    }

    /// Launch's cache-only answer: blocked or allowed by what was cached, `none` with no cache.
    #[test]
    fn peek_reads_only_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = config(Some("https://api-dev.pollis.com"));
        let s = peek(&c, dir.path());
        assert!(!s.blocked);
        assert_eq!(s.source, PolicySource::None);
        write_cache(dir.path(), DEV_POLICY_URL, &ClientPolicy::parse(FLOOR).unwrap());
        let s = peek(&c, dir.path());
        assert!(s.blocked);
        assert_eq!(s.source, PolicySource::Cache);
        c.client = Some(mac("1.17.0"));
        let s = peek(&c, dir.path());
        assert!(!s.blocked);
        assert_eq!(s.source, PolicySource::Cache);
    }

    #[tokio::test]
    async fn the_dev_override_is_used_and_never_cached() {
        let dir = tempfile::tempdir().unwrap();
        let dev = config(Some("http://127.0.0.1:9"));
        let ov = PolicyOverride { url: None, policy: Some(FLOOR.into()) };
        set_override(&dev, dir.path(), Some(&ov)).unwrap();
        let s = check(&dev, dir.path(), &FetchPlan::Direct, Duration::from_millis(200)).await;
        assert!(s.blocked);
        assert_eq!(s.source, PolicySource::Network);
        assert!(!dir.path().join(CACHE_FILE).exists(), "an override's verdict is not cached");

        // Cleared: nothing of it remains; the published file (unreachable here) decides.
        set_override(&dev, dir.path(), None).unwrap();
        set_override(&dev, dir.path(), None).unwrap();
        let s = check(&dev, dir.path(), &FetchPlan::Direct, Duration::from_millis(200)).await;
        assert!(!s.blocked);
        assert!(!peek(&dev, dir.path()).blocked);
    }

    /// An `inline:` env policy decides but is never cached either.
    #[tokio::test]
    async fn an_inline_env_policy_is_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let s = run(&format!("inline:{FLOOR}"), dir.path(), "1.16.0").await;
        assert!(s.blocked);
        assert!(!dir.path().join(CACHE_FILE).exists());
    }

    #[tokio::test]
    async fn a_strict_overlay_without_a_circuit_never_fetches() {
        let (url, _, hits) = serve(200, FLOOR).await;
        assert!(fetch(&url, &FetchPlan::Skip, FETCH_TIMEOUT).await.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }
}
