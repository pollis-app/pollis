//! Closed-overlay relay wiring (design §14): policy, the real circuit factory, the SOCKS5 shim and the shared reqwest client.
//!
//! **Off-by-default is sacred.** With the overlay off no shim starts and [`http_client`] is proxy-less.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use ml_dsa::Keypair;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

#[cfg(test)]
use std::net::SocketAddr;
#[cfg(test)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(test)]
use tokio::net::TcpStream;

use super::directory;
use super::path::{self, GuardBook, RelayEndpoint, RelayPath};
use super::revocation::RelayAdmission;

use pollis_relay::circuit::{Circuit, CircuitFactory};
use pollis_relay::client::ClientIdentity;
use pollis_relay::proto::DeviceCertMaterial;
use pollis_relay::stream::BoxedStream;
use pollis_relay::{
    Allowlist, CertificateDer, OverlayHandle, OverlayMode, OverlayShim, RoutingPolicy,
};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::state::AppState;

/// The one control-plane HTTP client: through the loopback SOCKS5 shim (`socks5h://`) when `overlay` is `Some`, plain otherwise.
pub(crate) fn http_client(overlay: Option<&OverlayHandle>) -> reqwest::Client {
    pollis_relay::http::http_client(overlay)
}

/// Bare host of a URL or `host:port`; the allowlist matches on host only.
fn host_of(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = after_scheme
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Per-host routing for a runtime `mode`: the first-party control plane goes through the overlay, everything else direct.
///
/// **LiveKit is deliberately absent (#813, §14.4)**: media goes direct to the same host, so hiding signalling gains nothing.
///
/// The shim is TCP-only anyway (see `a_socks_shim_cannot_carry_media_even_for_an_overlaid_host`).
pub(crate) fn overlay_policy(config: &Config, mode: OverlayMode) -> RoutingPolicy {
    let mut overlay_hosts: Vec<String> = Vec::new();
    let control_urls = [
        config.pollis_delivery_url.as_deref(),
        Some(config.r2_endpoint.as_str()),
        Some(config.r2_public_url.as_str()),
    ];
    for url in control_urls.into_iter().flatten() {
        if let Some(host) = host_of(url) {
            if !overlay_hosts.contains(&host) {
                overlay_hosts.push(host);
            }
        }
    }

    RoutingPolicy::new(
        mode,
        Allowlist::from_patterns(overlay_hosts),
        Allowlist::default(),
    )
}

/// `identity_version` for the locally minted relay cert; the relay only checks self-consistency, so a fixed value is correct.
pub(crate) const OVERLAY_CERT_IDENTITY_VERSION: u32 = 1;

/// How long a failed relay stays dead. Lazy recovery instead of a health-poll loop.
const RELAY_DEAD_COOLDOWN: Duration = Duration::from_secs(30);

/// Weight of the newest sample in the latency EWMA.
const RELAY_LATENCY_EWMA_ALPHA: f64 = 0.25;

/// Endpoints within this of the best rotate as equals, so a region doesn't pin to one relay (#812).
const RELAY_LATENCY_BAND: Duration = Duration::from_millis(20);

/// Every Nth dial ignores latency so demoted endpoints get re-sampled.
const RELAY_EXPLORE_EVERY: usize = 20;

/// Per-endpoint dial bound so an unreachable relay fails over fast.
const RELAY_DIAL_TIMEOUT: Duration = Duration::from_secs(8);

/// The production [`CircuitFactory`]: dial the pool with the device's [`ClientIdentity`].
///
/// **Identity (§9.4).** The cert is minted locally, never fetched: fetching would recurse through this shim. No account key ⇒ `connect` errors.
///
/// **Pool + failover (§14.1).** Try relays in health order, first success wins; failed dials cool down, and fail-open tries dead ones last.
///
/// **Path selection (#813 B).** Pick an exit as before, then [`crate::net::path`] adds guard and middles; the last hop is first-party.
struct RealRelayFactory {
    /// Weak, to avoid a cycle back into `AppState`.
    state: Weak<AppState>,
    endpoints: Vec<RelayEndpoint>,
    identity: AsyncMutex<Option<Arc<ClientIdentity>>>,
    /// `Some(dead_until)` while in cooldown, parallel to `endpoints`. Never held across an await.
    health: Mutex<Vec<Option<Instant>>>,
    /// Rotating start offset; doubles as the explore counter.
    next_start: AtomicUsize,
    /// Successful-dial latency EWMA; `None` (unmeasured) sorts with the fast band.
    latency: Mutex<Vec<Option<Duration>>>,
    /// Dead-endpoint cooldown; tests shorten it.
    cooldown: Duration,
    /// Per-dial bound; tests shorten it.
    dial_timeout: Duration,
    /// Shared across rebuilds so rollback protection survives a refresh.
    admission: Arc<RelayAdmission>,
    /// Pinned guards (§4.4a), shared so a refresh doesn't re-roll them.
    guards: Arc<GuardBook>,
    /// Target path length; tests raise it.
    target_hops: usize,
    /// Injectable dial step (#953).
    dialer: Arc<dyn PathDialer>,
}

/// The dial step of a `connect`, isolated behind a trait.
///
/// Lets the selection tests check ordering without real dials feeding back into it (#953).
#[async_trait::async_trait]
trait PathDialer: Send + Sync {
    async fn dial(
        &self,
        path: &RelayPath,
        identity: &Arc<ClientIdentity>,
        host: &str,
        port: u16,
    ) -> anyhow::Result<BoxedStream>;
}

/// Production dialer: resolve hops, build the circuit, CONNECT.
struct CircuitDialer;

#[async_trait::async_trait]
impl PathDialer for CircuitDialer {
    async fn dial(
        &self,
        path: &RelayPath,
        identity: &Arc<ClientIdentity>,
        host: &str,
        port: u16,
    ) -> anyhow::Result<BoxedStream> {
        let hops = path.resolve_hops().await?;
        let circuit = Circuit::build(hops, identity.clone())?;
        circuit.connect(host, port).await
    }
}

impl RealRelayFactory {
    /// Build a factory over `endpoints`, all initially healthy.
    fn new(
        state: Weak<AppState>,
        endpoints: Vec<RelayEndpoint>,
        cooldown: Duration,
        dial_timeout: Duration,
        admission: Arc<RelayAdmission>,
        guards: Arc<GuardBook>,
    ) -> Self {
        let n = endpoints.len();
        // A peer raises the target by one; no peers, no change.
        let target_hops = if endpoints.iter().any(|e| e.tier == path::RelayTier::Peer) {
            path::MAX_PATH_HOPS
        } else {
            path::TARGET_HOPS
        };
        RealRelayFactory {
            state,
            endpoints,
            identity: AsyncMutex::new(None),
            health: Mutex::new(vec![None; n]),
            next_start: AtomicUsize::new(0),
            latency: Mutex::new(vec![None; n]),
            cooldown,
            dial_timeout,
            admission,
            guards,
            target_hops,
            dialer: Arc::new(CircuitDialer),
        }
    }

    /// Test-only seam (#953); see [`PathDialer`].
    #[cfg(test)]
    fn with_dialer(mut self, dialer: Arc<dyn PathDialer>) -> Self {
        self.dialer = dialer;
        self
    }

    /// Try order: healthy first (fastest band rotating), then cooled-down ones (fail-open). No I/O under the locks.
    ///
    /// **Why latency ordering (#812)**: health-only rotation sent clients cross-country half the time, blowing §6.4.
    fn candidate_order(&self) -> Vec<usize> {
        let n = self.endpoints.len();
        let seq = self.next_start.fetch_add(1, Ordering::Relaxed);
        let start = seq % n;
        let now = Instant::now();
        let health = self.health.lock().unwrap();
        let mut healthy = Vec::with_capacity(n);
        let mut dead = Vec::new();
        for i in 0..n {
            let idx = (start + i) % n;
            let in_cooldown = health[idx].is_some_and(|until| until > now);
            if in_cooldown {
                dead.push(idx);
            } else {
                healthy.push(idx);
            }
        }
        drop(health);

        // `== N - 1` so a fresh pool's first dial is normal.
        let exploring = seq % RELAY_EXPLORE_EVERY == RELAY_EXPLORE_EVERY - 1;
        if !exploring {
            let latency = self.latency.lock().unwrap();
            // Cold pool: keep the rotation order.
            if let Some(best) = healthy.iter().filter_map(|&i| latency[i]).min() {
                let cutoff = best + RELAY_LATENCY_BAND;
                // Unmeasured counts as in-band; `partition` preserves rotation.
                let (fast, mut slow): (Vec<usize>, Vec<usize>) = healthy
                    .iter()
                    .copied()
                    .partition(|&i| latency[i].is_none_or(|d| d <= cutoff));
                slow.sort_by_key(|&i| latency[i]);
                healthy = fast;
                healthy.extend(slow);
            }
        }

        healthy.extend(dead);
        healthy
    }

    /// Fold a successful dial's duration into endpoint `idx`'s latency EWMA.
    fn record_latency(&self, idx: usize, sample: Duration) {
        let mut latency = self.latency.lock().unwrap();
        latency[idx] = Some(match latency[idx] {
            None => sample,
            Some(prev) => Duration::from_secs_f64(
                prev.as_secs_f64() * (1.0 - RELAY_LATENCY_EWMA_ALPHA)
                    + sample.as_secs_f64() * RELAY_LATENCY_EWMA_ALPHA,
            ),
        });
    }

    /// Mark endpoint `idx` dead until `now + cooldown` (failed dial).
    fn mark_dead(&self, idx: usize) {
        let until = Instant::now() + self.cooldown;
        self.health.lock().unwrap()[idx] = Some(until);
    }

    /// Clear endpoint `idx`'s dead mark (successful dial).
    fn mark_healthy(&self, idx: usize) {
        self.health.lock().unwrap()[idx] = None;
    }

    /// Dial along `path` via the [`PathDialer`]; one hop is the v0 exchange.
    async fn dial_path(
        &self,
        path: &RelayPath,
        identity: &Arc<ClientIdentity>,
        host: &str,
        port: u16,
    ) -> anyhow::Result<BoxedStream> {
        self.dialer.dial(path, identity, host, port).await
    }

    /// [`RealRelayFactory::candidate_order`] through the revocation gate; empty is the fail-closed state.
    fn admissible_order(&self, now_secs: i64) -> Vec<usize> {
        self.candidate_order()
            .into_iter()
            .filter(|&i| self.admission.admits(&self.endpoints[i], now_secs))
            .collect()
    }

    /// Load (or reuse) the device `ClientIdentity`; fail-closed when it can't authenticate.
    async fn identity(&self) -> anyhow::Result<Arc<ClientIdentity>> {
        {
            let cached = self.identity.lock().await;
            if let Some(id) = cached.as_ref() {
                return Ok(id.clone());
            }
        }
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("overlay: app state gone"))?;
        let id = Arc::new(build_client_identity(&state, "overlay").await?);
        *self.identity.lock().await = Some(id.clone());
        Ok(id)
    }
}

#[async_trait::async_trait]
impl CircuitFactory for RealRelayFactory {
    async fn connect(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream> {
        // Fail-closed when nothing is configured.
        if self.endpoints.is_empty() {
            anyhow::bail!("overlay: no relay endpoint / pinned cert configured");
        }
        let identity = self.identity().await?;

        // An inadmissible pool is EMPTY, never unfiltered.
        let now_secs = crate::util::now_unix() as i64;
        let admitted = self.admissible_order(now_secs);
        if admitted.is_empty() {
            anyhow::bail!(
                "overlay: no admissible relay (revocation fail-closed{})",
                if self.admission.enforcement_required() {
                    ", list required by the directory anchor"
                } else {
                    ""
                }
            );
        }
        // Guards only matter for multi-hop.
        let guards = if path::is_multi_hop(&self.endpoints, &admitted, self.target_hops) {
            self.guards.guards_for(&self.endpoints, &admitted, now_secs)
        } else {
            Vec::new()
        };

        // First success wins; only an all-down pool errors. Health tracks the EXIT endpoint.
        let mut last_err = None;
        for (attempt, idx) in admitted.iter().copied().enumerate() {
            let Some(selected) = path::plan_path(
                &self.endpoints,
                &admitted,
                &guards,
                idx,
                attempt,
                self.target_hops,
            ) else {
                // No valid path with this exit (e.g. a peer, §4.4).
                continue;
            };
            let started = Instant::now();
            let dial = self.dial_path(&selected, &identity, host, port);
            let outcome = match tokio::time::timeout(self.dial_timeout, dial).await {
                Ok(res) => res,
                Err(_) => Err(anyhow::anyhow!(
                    "overlay: relay {} dial timed out",
                    self.endpoints[idx].addr
                )),
            };
            match outcome {
                Ok(stream) => {
                    // Time the whole dial: that is the latency §6.4 budgets.
                    self.record_latency(idx, started.elapsed());
                    self.mark_healthy(idx);
                    return Ok(stream);
                }
                Err(e) => {
                    self.mark_dead(idx);
                    last_err = Some(e);
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| anyhow::anyhow!("overlay: relay pool exhausted with no endpoints")))
    }
}

/// Build the logged-in device's relay [`ClientIdentity`] from local state.
/// See [`RealRelayFactory`] for why the cert is minted locally.
/// Shared with `net::peer::context` for the park handshake; `who` only tags errors.
pub(crate) async fn build_client_identity(
    state: &Arc<AppState>,
    who: &str,
) -> anyhow::Result<ClientIdentity> {
    let user_id = overlay_signing_user(state, who).await?;

    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("{who}: device_id not set (not logged in)"))?;

    let account_key = crate::commands::account_identity::load_account_id_key(state, &user_id)
        .await
        .map_err(|e| anyhow::anyhow!("{who}: account identity unavailable ({e})"))?;
    let account_id_pub = account_key.verifying_key().encode().to_vec();

    // The ML-DSA-44 key signs; the Ed25519 one is only carried. Scoped so the !Send provider drops before any await.
    let (device_signing, ed_pub, pq_pub) = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("{who}: not signed in (local DB closed)"))?;
        let provider = crate::commands::mls::PollisProvider::new(db.conn());
        let (ed_pub, pq_pub) =
            crate::commands::mls::load_device_cert_pubs(&provider, &user_id, &device_id)
                .map_err(|e| anyhow::anyhow!("{who}: device signing keys unavailable ({e})"))?;
        let (device_signing, _) =
            crate::commands::mls::load_device_pq_signing_key(&provider, &user_id, &device_id)
                .map_err(|e| anyhow::anyhow!("{who}: device PQ signing key unavailable ({e})"))?;
        (device_signing, ed_pub, pq_pub)
    };
    let ed_pub: [u8; 32] = ed_pub
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("{who}: device Ed25519 signing pub is not 32 bytes"))?;

    let issued_at = crate::util::now_unix();
    let cert = DeviceCertMaterial::mint(
        &account_key,
        &device_id,
        &ed_pub,
        &pq_pub,
        OVERLAY_CERT_IDENTITY_VERSION,
        issued_at,
    );
    debug_assert_eq!(cert.account_id_pub, account_id_pub);

    Ok(ClientIdentity::new(
        user_id,
        device_id,
        ed_pub,
        device_signing,
        cert,
    ))
}

/// The user this device signs as, mirroring `ds_client::current_user_id`: prefer
/// the unlocked session, fall back to the accounts index before unlock.
async fn overlay_signing_user(state: &Arc<AppState>, who: &str) -> anyhow::Result<String> {
    if let Some(u) = state.unlock.lock().await.as_ref() {
        if !u.user_id.is_empty() {
            return Ok(u.user_id.clone());
        }
    }
    let index = crate::accounts::read_accounts_index()
        .map_err(|e| anyhow::anyhow!("{who}: read accounts index: {e}"))?;
    index
        .last_active_user
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{who}: no active user to sign relay handshake"))
}

/// Configured relay endpoints plus pinned leaf; empty (fail-closed) when either is missing.
fn load_relay_endpoints(config: &Config) -> Vec<RelayEndpoint> {
    let addrs = config.overlay_relay_endpoints();
    if addrs.is_empty() {
        return Vec::new();
    }
    let cert = match config.overlay_relay_cert.as_deref().and_then(load_pinned_cert) {
        Some(c) => c,
        None => {
            crate::diag!("[overlay] relay endpoint set but no valid pinned cert — staying fail-closed");
            return Vec::new();
        }
    };
    addrs
        .into_iter()
        // Static config is first-party with an unknown region.
        .map(|addr| RelayEndpoint::first_party(addr, String::new(), cert.clone()))
        .collect()
}

/// Resolve `POLLIS_OVERLAY_RELAY_CERT`: a filesystem path to a DER cert, else the
/// base64 (STANDARD) of the DER bytes.
fn load_pinned_cert(s: &str) -> Option<CertificateDer<'static>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if std::path::Path::new(s).is_file() {
        return std::fs::read(s).ok().map(CertificateDer::from);
    }
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .ok()
        .map(CertificateDer::from)
}

/// Refresh this long BEFORE expiry, so the pool never serves an expired directory.
const DIRECTORY_REFRESH_SKEW: Duration = Duration::from_secs(5 * 60);
/// Clamp so a pathological `expires_at` can neither tight-loop nor park forever.
const DIRECTORY_MIN_REFRESH: Duration = Duration::from_secs(30);
const DIRECTORY_MAX_REFRESH: Duration = Duration::from_secs(55 * 60);
/// Fixed retry backoff after a failed fetch; the previous pool is kept meanwhile.
const DIRECTORY_RETRY_BACKOFF: Duration = Duration::from_secs(60);
/// Floor between fetches, so exhaustion notifies can't hammer the host.
const DIRECTORY_MIN_REFETCH: Duration = Duration::from_secs(15);

/// Map a verified directory to endpoints, each with its own pinned cert; unpinnable entries are skipped.
pub(crate) fn directory_to_endpoints(dir: &directory::Directory) -> Vec<RelayEndpoint> {
    use base64::Engine as _;
    let mut endpoints: Vec<RelayEndpoint> = dir
        .relays
        .iter()
        .filter_map(|r| {
            let der = base64::engine::general_purpose::STANDARD
                .decode(r.cert_b64.trim())
                .ok()?;
            // `relays[]` is all ours; peers come from a separate array.
            Some(RelayEndpoint::first_party(
                r.addr.clone(),
                r.region.clone(),
                CertificateDer::from(der),
            ))
        })
        .collect();
    let peers = directory_to_peer_endpoints(dir, &endpoints);
    endpoints.extend(peers);
    endpoints
}

/// The peer-hosted middle hops a verified directory offers (#813 wave 3).
///
/// The only producer of [`RelayTier::Peer`] endpoints; everything fails toward fewer peers:
///
/// - a `cert_b64` that is not valid base64/DER is skipped;
/// - an entry parked at no relay in **this** directory is unreachable and skipped;
/// - the rendezvous is the first parked relay that IS advertised.
///
/// Revocation is applied per candidate at dial time by [`RelayAdmission`], not here.
fn directory_to_peer_endpoints(
    dir: &directory::Directory,
    first_party: &[RelayEndpoint],
) -> Vec<RelayEndpoint> {
    use base64::Engine as _;
    dir.peers
        .iter()
        .filter_map(|p| {
            let der = base64::engine::general_purpose::STANDARD
                .decode(p.cert_b64.trim())
                .ok()?;
            let parked_at: Vec<String> = p
                .parked_at
                .iter()
                .filter(|a| first_party.iter().any(|e| &&e.addr == a))
                .cloned()
                .collect();
            let rendezvous = parked_at.first()?.clone();
            Some(RelayEndpoint::peer_parked_at(
                rendezvous,
                // No region for a peer; unknown is never treated as diverse.
                String::new(),
                CertificateDer::from(der),
                parked_at,
            ))
        })
        .collect()
}

/// Sleep until the next refresh: `expires_at` minus skew, clamped, then jittered (#875).
///
/// `expires_at` is the same for the whole fleet, so unjittered it synchronises every client's wake-up.
fn until_refresh(expires_at: i64) -> Duration {
    let now = crate::util::now_unix() as i64;
    let secs = (expires_at - now).saturating_sub(DIRECTORY_REFRESH_SKEW.as_secs() as i64);
    let clamped =
        Duration::from_secs(secs.max(0) as u64).clamp(DIRECTORY_MIN_REFRESH, DIRECTORY_MAX_REFRESH);
    // Downward only, then re-floored, so it never overshoots expiry.
    pollis_relay::backoff::jittered_down(clamped).max(DIRECTORY_MIN_REFRESH)
}

/// Aborts the refresh task when the factory drops.
struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A [`CircuitFactory`] whose pool the refresh loop swaps live; an exhausted dial nudges a refetch.
struct SwappableFactory {
    inner: Mutex<Arc<dyn CircuitFactory>>,
    /// Pinged when a connect exhausts the pool, to refetch out of band.
    refresh_notify: tokio::sync::Notify,
    /// `Option` so it can be set after the `Arc` exists.
    refresh_task: Mutex<Option<AbortOnDrop>>,
}

impl SwappableFactory {
    fn new(initial: Arc<dyn CircuitFactory>) -> Self {
        SwappableFactory {
            inner: Mutex::new(initial),
            refresh_notify: tokio::sync::Notify::new(),
            refresh_task: Mutex::new(None),
        }
    }

    fn current(&self) -> Arc<dyn CircuitFactory> {
        self.inner.lock().unwrap().clone()
    }

    fn swap(&self, next: Arc<dyn CircuitFactory>) {
        *self.inner.lock().unwrap() = next;
    }
}

#[async_trait::async_trait]
impl CircuitFactory for SwappableFactory {
    async fn connect(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream> {
        let factory = self.current();
        match factory.connect(host, port).await {
            Ok(stream) => Ok(stream),
            Err(e) => {
                // Pool exhausted: nudge a (debounced) refetch.
                self.refresh_notify.notify_one();
                Err(e)
            }
        }
    }
}

/// Fetch, verify and build a fresh pool; returns it and `expires_at`. Fail-closed.
///
/// The revocation list rides this tick; its failure is not an error here, since [`RelayAdmission`] then refuses every member.
async fn fetch_and_build_pool(
    url: &str,
    key: &str,
    state: &Weak<AppState>,
    cooldown: Duration,
    dial_timeout: Duration,
    admission: &Arc<RelayAdmission>,
    guards: &Arc<GuardBook>,
) -> anyhow::Result<(Arc<dyn CircuitFactory>, i64)> {
    let bytes = directory::fetch_directory(url).await?;
    let now = crate::util::now_unix() as i64;
    let dir = directory::verify_directory(&bytes, key, now)?;
    // Adopt the anchor BEFORE fetching the list, so the install's seq floor matches.
    admission.observe_directory(&dir);
    if let Err(e) = admission.refresh(url, &dir, now).await {
        crate::diag!("[overlay] revocation list unusable ({e}) — relays fail closed while the directory anchors one");
    }
    let endpoints = directory_to_endpoints(&dir);
    if endpoints.is_empty() {
        anyhow::bail!("overlay: directory verified but no relay had a usable pinned cert");
    }
    let factory: Arc<dyn CircuitFactory> = Arc::new(RealRelayFactory::new(
        state.clone(),
        endpoints,
        cooldown,
        dial_timeout,
        admission.clone(),
        guards.clone(),
    ));
    Ok((factory, dir.expires_at))
}

/// Keep the pool current: wakes on the expiry schedule or a debounced exhaustion notify; exits when the factory drops.
#[allow(clippy::too_many_arguments)]
async fn directory_refresh_loop(
    factory: Weak<SwappableFactory>,
    state: Weak<AppState>,
    url: String,
    key: String,
    cooldown: Duration,
    dial_timeout: Duration,
    admission: Arc<RelayAdmission>,
    guards: Arc<GuardBook>,
    mut next_sleep: Duration,
) {
    let mut last_fetch = Instant::now();
    loop {
        let sf = match factory.upgrade() {
            Some(sf) => sf,
            None => return,
        };
        tokio::select! {
            _ = tokio::time::sleep(next_sleep) => {}
            _ = sf.refresh_notify.notified() => {}
        }
        drop(sf);

        let since = last_fetch.elapsed();
        if since < DIRECTORY_MIN_REFETCH {
            tokio::time::sleep(DIRECTORY_MIN_REFETCH - since).await;
        }
        last_fetch = Instant::now();

        match fetch_and_build_pool(
            &url,
            &key,
            &state,
            cooldown,
            dial_timeout,
            &admission,
            &guards,
        )
        .await
        {
            Ok((next_factory, expires_at)) => match factory.upgrade() {
                Some(sf) => {
                    sf.swap(next_factory);
                    next_sleep = until_refresh(expires_at);
                }
                None => return,
            },
            Err(e) => {
                crate::diag!("[overlay] directory refresh failed, keeping current pool: {e}");
                // Jittered so a directory outage doesn't return the fleet in lockstep (#875).
                next_sleep = pollis_relay::backoff::jittered(DIRECTORY_RETRY_BACKOFF);
            }
        }
    }
}

/// The directory-backed factory: one initial fetch, then the refresh loop. Empty (fail-closed) until it succeeds.
async fn build_directory_factory(state: &Arc<AppState>) -> Arc<dyn CircuitFactory> {
    let url = state.config.overlay_directory_url.clone().unwrap_or_default();
    let key = state.config.overlay_directory_key.clone().unwrap_or_default();
    let weak_state = Arc::downgrade(state);
    // Shared across rebuilds so rollback protection and guard pins survive a refresh.
    let admission = Arc::new(RelayAdmission::enforcing(&key));
    let guards = Arc::new(GuardBook::at(GuardBook::default_path()));

    let (initial, next_sleep) = match fetch_and_build_pool(
        &url,
        &key,
        &weak_state,
        RELAY_DEAD_COOLDOWN,
        RELAY_DIAL_TIMEOUT,
        &admission,
        &guards,
    )
    .await
    {
        Ok((factory, expires_at)) => (factory, until_refresh(expires_at)),
        Err(e) => {
            crate::diag!("[overlay] initial directory fetch failed, fail-closed until refresh: {e}");
            let empty: Arc<dyn CircuitFactory> = Arc::new(RealRelayFactory::new(
                weak_state.clone(),
                Vec::new(),
                RELAY_DEAD_COOLDOWN,
                RELAY_DIAL_TIMEOUT,
                admission.clone(),
                guards.clone(),
            ));
            (empty, DIRECTORY_RETRY_BACKOFF)
        }
    };

    let swappable = Arc::new(SwappableFactory::new(initial));
    let task = tokio::spawn(directory_refresh_loop(
        Arc::downgrade(&swappable),
        weak_state,
        url,
        key,
        RELAY_DEAD_COOLDOWN,
        RELAY_DIAL_TIMEOUT,
        admission,
        guards,
        next_sleep,
    ));
    *swappable.refresh_task.lock().unwrap() = Some(AbortOnDrop(task));
    swappable
}

/// Start the shim under a non-off `mode`. Succeeds before login; circuits fail closed until a device can sign.
pub(crate) async fn start_overlay_shim(
    state: &Arc<AppState>,
    mode: OverlayMode,
) -> Result<OverlayHandle> {
    let policy = overlay_policy(&state.config, mode);
    let factory: Arc<dyn CircuitFactory> = if state.config.overlay_directory_configured() {
        build_directory_factory(state).await
    } else {
        Arc::new(RealRelayFactory::new(
            Arc::downgrade(state),
            load_relay_endpoints(&state.config),
            RELAY_DEAD_COOLDOWN,
            RELAY_DIAL_TIMEOUT,
            // No directory ⇒ no anchor to enforce.
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
        ))
    };
    let handle = OverlayShim::start(policy, factory)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("overlay shim start: {e}")))?;
    let relay = if state.config.overlay_directory_configured() {
        state
            .config
            .overlay_directory_url
            .as_deref()
            .unwrap_or("<directory>")
    } else {
        state
            .config
            .overlay_relay_url
            .as_deref()
            .unwrap_or("<none configured>")
    };
    crate::diag!(
        "[overlay] shim on {} (mode={mode:?}, relay={relay})",
        handle.socks_addr()
    );
    Ok(handle)
}

// Test-only since #987: proves a TLS session survives the tunnel end to end (§14.1 T2).

#[cfg(test)]
fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::other(msg.to_string())
}

/// TCP to `host:port` via SOCKS5 CONNECT with proxy-side DNS.
#[cfg(test)]
pub(crate) async fn socks5_connect(
    shim: SocketAddr,
    host: &str,
    port: u16,
) -> std::io::Result<TcpStream> {
    let mut sock = TcpStream::connect(shim).await?;

    // Greeting: VER=5, one method, METHOD=0 (no auth — loopback only).
    sock.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut method = [0u8; 2];
    sock.read_exact(&mut method).await?;
    if method[0] != 0x05 || method[1] != 0x00 {
        return Err(io_err("overlay shim declined SOCKS5 no-auth"));
    }

    // Request: VER, CMD=CONNECT, RSV, ATYP=DOMAIN, LEN, host, port (big-endian).
    let host_bytes = host.as_bytes();
    if host_bytes.len() > 255 {
        return Err(io_err("overlay connector: host too long for SOCKS5"));
    }
    let mut req = Vec::with_capacity(7 + host_bytes.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8]);
    req.extend_from_slice(host_bytes);
    req.extend_from_slice(&port.to_be_bytes());
    sock.write_all(&req).await?;

    // Reply: VER, REP, RSV, ATYP, BND.ADDR, BND.PORT. REP=0 is success.
    let mut head = [0u8; 4];
    sock.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(io_err(&format!(
            "overlay shim CONNECT failed (SOCKS reply code {})",
            head[1]
        )));
    }
    // Drain the bound address the shim echoes (ignored for CONNECT).
    match head[3] {
        0x01 => {
            let mut addr = [0u8; 4];
            sock.read_exact(&mut addr).await?;
        }
        0x04 => {
            let mut addr = [0u8; 16];
            sock.read_exact(&mut addr).await?;
        }
        0x03 => {
            let len = sock.read_u8().await? as usize;
            let mut addr = vec![0u8; len];
            sock.read_exact(&mut addr).await?;
        }
        other => {
            return Err(io_err(&format!(
                "overlay shim replied with unknown ATYP {other}"
            )));
        }
    }
    let mut bnd_port = [0u8; 2];
    sock.read_exact(&mut bnd_port).await?;

    Ok(sock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pollis_relay::circuit::{Hop, SingleHopFactory};
    use pollis_relay::client::ClientIdentity;
    use pollis_relay::proto::DeviceCertMaterial;
    use pollis_relay::server::{Allowlist as RelayAllowlist, RelayConfig, RelayServer, RelayStats};
    use rustls::pki_types::{CertificateDer, ServerName};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use zeroize::Zeroizing;

    use crate::commands::pin::UnlockState;

    const USER: &str = "u_overlay_test";
    const DEVICE: &str = "d_overlay_test";
    const ORIGIN_NAME: &str = "origin.test";
    const ISSUED_AT: u64 = 1_700_000_000;

    /// Only carried by the handshake, so a fixed pattern is enough.
    const DEVICE_ED_PUB: [u8; 32] = [11u8; 32];

    /// The device ML-DSA-44 signing key (signs the relay handshake).
    fn signing_key() -> ml_dsa::SigningKey<ml_dsa::MlDsa44> {
        ml_dsa::SigningKey::<ml_dsa::MlDsa44>::from_seed(&[11u8; 32].into())
    }

    /// The account identity key that certifies the device into a cert chain.
    fn account_key() -> ml_dsa::SigningKey<ml_dsa::MlDsa44> {
        ml_dsa::SigningKey::<ml_dsa::MlDsa44>::from_seed(&[12u8; 32].into())
    }

    /// A device identity with a valid offline cert chain, as production presents.
    fn identity() -> Arc<ClientIdentity> {
        let device = signing_key();
        let cert = DeviceCertMaterial::mint(
            &account_key(),
            DEVICE,
            &DEVICE_ED_PUB,
            &device.verifying_key().encode(),
            1,
            ISSUED_AT,
        );
        Arc::new(ClientIdentity::new(
            USER,
            DEVICE,
            DEVICE_ED_PUB,
            device,
            cert,
        ))
    }

    fn cfg(mode: OverlayMode, relay: Option<&str>) -> Config {
        Config {
            r2_endpoint: "https://r2.example.com".into(),
            r2_public_url: "https://cdn.example.com".into(),
            livekit_url: "wss://livekit.example.com".into(),
            pollis_delivery_url: Some("https://api.example.com".into()),
            overlay_mode: mode,
            overlay_relay_url: relay.map(|s| s.to_string()),
            overlay_relay_cert: None,
            overlay_directory_url: None,
            overlay_directory_key: None,
            dev_access_key: None,
        }
    }

    /// Base64 DER of a pinned cert, for `POLLIS_OVERLAY_RELAY_CERT`.
    fn cert_b64(cert: &CertificateDer<'static>) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(cert.as_ref())
    }

    /// A never-connecting factory: the fail-closed shape.
    struct FailingFactory;

    #[async_trait::async_trait]
    impl CircuitFactory for FailingFactory {
        async fn connect(&self, _host: &str, _port: u16) -> anyhow::Result<BoxedStream> {
            anyhow::bail!("overlay circuit unavailable (test fail-closed factory)")
        }
    }

    struct TestRelay {
        addr: SocketAddr,
        cert: CertificateDer<'static>,
        stats: Arc<RelayStats>,
        _task: tokio::task::JoinHandle<()>,
    }

    fn spawn_relay(allow: &[&str], overrides: &[(&str, IpAddr)]) -> TestRelay {
        let mut config = RelayConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            // Test origins listen on random ports; a bare host would mean 443.
            RelayAllowlist::from_patterns(allow.iter().map(|h| format!("{h}:*"))),
        )
        .unwrap();
        for (host, ip) in overrides {
            config.resolve_overrides.insert((*host).to_string(), *ip);
        }
        // Without a current list this relay refuses every Extend (see net::testing).
        config.revocations = crate::net::testing::healthy_revocations();
        config.next_hops = crate::net::testing::test_directory();
        config.allow_private_next_hops = true;
        let cert = config.server_cert();
        let stats = config.stats.clone();
        let (task, addr) = RelayServer::spawn(config).unwrap();
        crate::net::testing::publish_hop(addr);
        TestRelay { addr, cert, stats, _task: task }
    }

    impl TestRelay {
        fn factory(&self) -> Arc<dyn CircuitFactory> {
            Arc::new(SingleHopFactory::new(
                Hop::new(self.addr, self.cert.clone()),
                identity(),
            ))
        }
    }

    async fn spawn_plain_http(body: &'static str) -> SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let body = body.to_string();
                tokio::spawn(async move {
                    let _ = read_http_head(&mut sock).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        addr
    }

    /// Loopback TLS origin for `origin.test`: addr, CA and an accept counter.
    async fn spawn_tls_origin(
        body: &'static str,
    ) -> (SocketAddr, CertificateDer<'static>, Arc<AtomicUsize>) {
        pollis_relay::tls::ensure_crypto_provider();
        let chain = pollis_relay::tls::generate_issued_chain(ORIGIN_NAME).unwrap();
        let ca = chain.ca_der.clone();

        let mut server_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![chain.leaf_cert_der.clone()], chain.leaf_key_der)
            .unwrap();
        server_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let c = c.clone();
                tokio::spawn(async move {
                    let mut tls = match acceptor.accept(tcp).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    c.fetch_add(1, Ordering::Relaxed);
                    let _ = read_http_head(&mut tls).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = tls.write_all(resp.as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        (addr, ca, count)
    }

    async fn read_http_head<S: AsyncReadExt + Unpin>(s: &mut S) -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = s.read(&mut byte).await?;
            if n == 0 {
                break;
            }
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        Ok(buf)
    }

    #[test]
    fn parse_overlay_mode_cases() {
        use crate::config::parse_overlay_mode;
        assert_eq!(parse_overlay_mode("off"), OverlayMode::Off);
        assert_eq!(parse_overlay_mode("prefer"), OverlayMode::Prefer);
        assert_eq!(parse_overlay_mode("STRICT"), OverlayMode::Strict);
        assert_eq!(parse_overlay_mode(" Prefer "), OverlayMode::Prefer);
        assert_eq!(parse_overlay_mode("bogus"), OverlayMode::Off);
        assert_eq!(parse_overlay_mode(""), OverlayMode::Off);
    }

    #[test]
    fn host_of_strips_scheme_port_path() {
        assert_eq!(host_of("https://api.example.com:8080/v1").as_deref(), Some("api.example.com"));
        assert_eq!(host_of("wss://LiveKit.Example.com").as_deref(), Some("livekit.example.com"));
        assert_eq!(host_of("bare-host:443").as_deref(), Some("bare-host"));
        assert_eq!(host_of(""), None);
    }

    /// The plane split: the control plane is overlaid; everything else, LiveKit included, is direct.
    #[test]
    fn policy_routes_control_plane_and_leaves_livekit_direct() {
        let policy = overlay_policy(&cfg(OverlayMode::Prefer, None), OverlayMode::Prefer);
        use pollis_relay::PlannedRoute;
        // Control-plane hosts (the DS and R2) route overlay.
        assert_eq!(
            policy.plan("api.example.com"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        assert_eq!(
            policy.plan("r2.example.com"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        assert_eq!(policy.plan("livekit.example.com"), PlannedRoute::Direct);
        assert_eq!(policy.plan("exp.host"), PlannedRoute::Direct);
    }

    /// `Off` routes every host direct.
    #[test]
    fn off_mode_policy_is_all_direct() {
        use pollis_relay::PlannedRoute;
        let policy = overlay_policy(&cfg(OverlayMode::Off, None), OverlayMode::Off);
        assert_eq!(policy.plan("api.example.com"), PlannedRoute::Direct);
        assert_eq!(policy.plan("r2.example.com"), PlannedRoute::Direct);
        assert_eq!(policy.plan("livekit.example.com"), PlannedRoute::Direct);
    }

    #[tokio::test]
    async fn off_mode_http_client_is_proxyless() {
        let addr = spawn_plain_http("direct-and-inert").await;
        let client = http_client(None);
        let resp = client
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("direct request with the overlay off");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "direct-and-inert");
    }

    /// With a Strict shim and no relay, a proxied client fails where `http_client(None)` succeeds.
    #[tokio::test]
    async fn off_client_bypasses_a_shim_that_would_degrade() {
        let addr = spawn_plain_http("only-direct-reaches-me").await;
        let host = addr.ip().to_string();
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([host.as_str()]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, Arc::new(FailingFactory))
            .await
            .unwrap();

        let proxied = http_client(Some(&shim));
        let via_overlay = proxied
            .get(format!("http://{host}:{}/", addr.port()))
            .send()
            .await;
        assert!(via_overlay.is_err(), "Strict with no relay must not silently go direct");

        let direct = http_client(None)
            .get(format!("http://{host}:{}/", addr.port()))
            .send()
            .await
            .expect("proxy-less client reaches the origin directly");
        assert_eq!(direct.status(), 200);
        drop(shim);
    }

    #[tokio::test]
    async fn reqwest_routes_through_shim_to_tls_origin() {
        let (origin_addr, ca, origin_conns) = spawn_tls_origin("hello-overlay").await;
        let relay = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([ORIGIN_NAME]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, relay.factory()).await.unwrap();

        let client = pollis_relay::http::http_client_builder(Some(&shim))
            .add_root_certificate(reqwest::Certificate::from_der(&ca).unwrap())
            .build()
            .unwrap();
        let url = format!("https://{ORIGIN_NAME}:{}/", origin_addr.port());
        let resp = client.get(&url).send().await.expect("request through overlay");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "hello-overlay");
        assert_eq!(relay.stats.dials(), 1, "relay must have dialed the origin");
        assert_eq!(origin_conns.load(Ordering::Relaxed), 1);
        drop(shim);
    }

    /// End-to-end TLS survives the tunnel and verifies for `origin.test` (§14.1, T2).
    #[tokio::test]
    async fn socks_shim_carries_verified_tls() {
        let (origin_addr, ca, _c) = spawn_tls_origin("socks-connector-shape").await;
        let relay = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([ORIGIN_NAME]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, relay.factory()).await.unwrap();

        let stream = socks5_connect(shim.socks_addr(), ORIGIN_NAME, origin_addr.port())
            .await
            .expect("SOCKS connect through shim");

        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca).unwrap();
        let mut client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tls_connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));
        let server_name = ServerName::try_from(ORIGIN_NAME).unwrap();
        let mut tls = tls_connector
            .connect(server_name, stream)
            .await
            .expect("inner TLS verified for origin.test through the shim");

        tls.write_all(b"GET / HTTP/1.1\r\nHost: origin.test\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        tls.read_to_end(&mut resp).await.unwrap();
        let resp = String::from_utf8_lossy(&resp);
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "unexpected response: {resp}");
        assert!(resp.contains("socks-connector-shape"));
        assert_eq!(relay.stats.dials(), 1);
        drop(shim);
    }

    /// Strict with no usable circuit surfaces an error rather than dialing direct.
    #[tokio::test]
    async fn strict_without_relay_degrades_not_silent_direct() {
        let addr = spawn_plain_http("must-not-be-reached").await;
        let host = addr.ip().to_string();
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([host.as_str()]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, Arc::new(FailingFactory))
            .await
            .expect("Strict starts the shim even with no relay reachable");

        let result = socks5_connect(shim.socks_addr(), &host, addr.port()).await;
        assert!(result.is_err(), "Strict + no relay must degrade, never silent-direct");
        drop(shim);
    }

    /// An `AppState` with device cert material, so `RealRelayFactory` can authenticate.
    async fn provisioned_state(config: Config) -> Arc<AppState> {
        let keystore: Arc<dyn crate::keystore::Keystore> =
            Arc::new(crate::keystore::InMemoryKeystore::new());
        let state = Arc::new(AppState::new_with_parts(config, keystore));
        *state.unlock.lock().await = Some(UnlockState {
            user_id: USER.to_string(),
            db_key: Zeroizing::new(vec![7u8; 32]),
            account_id_key: Zeroizing::new(account_key().to_seed().to_vec()),
        });
        *state.device_id.lock().await = Some(DEVICE.to_string());
        *state.local_db.lock().await = Some(crate::db::local::LocalDb::open_in_memory().unwrap());
        state
    }

    /// SOCKS-dial the origin and verify inner TLS terminates at `origin.test` (§8).
    async fn tls_probe_through_shim(shim: SocketAddr, port: u16, ca: &CertificateDer<'static>) {
        let stream = socks5_connect(shim, ORIGIN_NAME, port)
            .await
            .expect("SOCKS connect through shim");

        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.clone()).unwrap();
        let mut client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tls_connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));
        let server_name = ServerName::try_from(ORIGIN_NAME).unwrap();
        let mut tls = tls_connector
            .connect(server_name, stream)
            .await
            .expect("inner TLS verified for origin.test through the shim");
        tls.write_all(b"GET / HTTP/1.1\r\nHost: origin.test\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        tls.read_to_end(&mut resp).await.unwrap();
        assert!(
            String::from_utf8_lossy(&resp).starts_with("HTTP/1.1 200 OK"),
            "TLS probe did not reach origin through the relay"
        );
    }

    /// Live proof: `set_overlay_mode` routes reqwest and raw TLS through a relay, flips Prefer↔Strict live, and Off restores direct.
    #[tokio::test]
    async fn set_overlay_mode_routes_live_through_relay() {
        let (origin_addr, ca, origin_conns) = spawn_tls_origin("live-apply").await;
        let relay = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);

        let mut config = cfg(OverlayMode::Off, Some(&relay.addr.to_string()));
        config.pollis_delivery_url = Some(format!("https://{ORIGIN_NAME}"));
        config.overlay_relay_cert = Some(cert_b64(&relay.cert));
        let state = provisioned_state(config).await;

        use crate::commands::overlay::{apply_overlay_mode, get_overlay_mode};

        apply_overlay_mode(&state, OverlayMode::Prefer).await.unwrap();
        assert_eq!(get_overlay_mode(&state).await.unwrap(), "prefer");
        let handle = state.overlay_handle().expect("shim running after Prefer");
        assert_eq!(handle.mode(), OverlayMode::Prefer);
        let shim_addr = handle.socks_addr();

        let client = pollis_relay::http::http_client_builder(Some(handle.as_ref()))
            .add_root_certificate(reqwest::Certificate::from_der(&ca).unwrap())
            .build()
            .unwrap();
        let url = format!("https://{ORIGIN_NAME}:{}/", origin_addr.port());
        let resp = client.get(&url).send().await.expect("reqwest routes through relay");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "live-apply");
        assert_eq!(relay.stats.dials(), 1, "relay must have dialed the origin (reqwest)");

        tls_probe_through_shim(shim_addr, origin_addr.port(), &ca).await;
        assert_eq!(relay.stats.dials(), 2, "relay must have dialed the origin (SOCKS shape)");
        assert!(origin_conns.load(Ordering::Relaxed) >= 2);

        apply_overlay_mode(&state, OverlayMode::Strict).await.unwrap();
        assert_eq!(get_overlay_mode(&state).await.unwrap(), "strict");
        let handle2 = state.overlay_handle().expect("shim still running after Strict");
        assert_eq!(handle2.mode(), OverlayMode::Strict);
        assert_eq!(handle2.socks_addr(), shim_addr, "Prefer↔Strict must not restart the shim");

        apply_overlay_mode(&state, OverlayMode::Off).await.unwrap();
        assert_eq!(get_overlay_mode(&state).await.unwrap(), "off");
        assert!(state.overlay_handle().is_none(), "shim must stop after Off");

        let plain = spawn_plain_http("direct-after-off").await;
        let direct = http_client(None)
            .get(format!("http://{plain}/"))
            .send()
            .await
            .expect("direct request after Off");
        assert_eq!(direct.status(), 200);
        assert_eq!(relay.stats.dials(), 2, "Off routes direct — relay sees no new dials");
    }

    /// Strict with the relay DOWN, applied live, surfaces an error.
    #[tokio::test]
    async fn set_overlay_strict_relay_down_degrades_live() {
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        let mut config = cfg(OverlayMode::Off, Some("127.0.0.1:1"));
        config.pollis_delivery_url = Some(format!("https://{ORIGIN_NAME}"));
        config.overlay_relay_cert = Some(cert_b64(&cert));
        let state = provisioned_state(config).await;

        crate::commands::overlay::apply_overlay_mode(&state, OverlayMode::Strict)
            .await
            .unwrap();
        let handle = state.overlay_handle().unwrap();

        assert!(
            socks5_connect(handle.socks_addr(), ORIGIN_NAME, 443).await.is_err(),
            "Strict + relay down must degrade, never silent-direct"
        );
    }

    /// Prefer with the relay DOWN falls back to a direct dial.
    #[tokio::test]
    async fn set_overlay_prefer_relay_down_falls_back_direct_live() {
        let plain = spawn_plain_http("prefer-fallback").await;
        let host = plain.ip().to_string();
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        let mut config = cfg(OverlayMode::Off, Some("127.0.0.1:1"));
        config.pollis_delivery_url = Some(format!("https://{host}"));
        config.overlay_relay_cert = Some(cert_b64(&cert));
        let state = provisioned_state(config).await;

        crate::commands::overlay::apply_overlay_mode(&state, OverlayMode::Prefer)
            .await
            .unwrap();
        let handle = state.overlay_handle().unwrap();

        assert!(
            socks5_connect(handle.socks_addr(), &host, plain.port()).await.is_ok(),
            "Prefer + relay down must fall back to a direct dial"
        );
    }

    const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// An in-process pool relay that resolves and allows `origin.test`.
    fn spawn_pool_relay() -> TestRelay {
        spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, LOCALHOST)])
    }

    /// A `RealRelayFactory` over `endpoints`, with identity from a provisioned `AppState`.
    fn pool_factory(
        state: &Arc<AppState>,
        endpoints: Vec<RelayEndpoint>,
        cooldown: Duration,
    ) -> RealRelayFactory {
        RealRelayFactory::new(
            Arc::downgrade(state),
            endpoints,
            cooldown,
            Duration::from_secs(2),
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
        )
    }

    fn endpoint(addr: String, cert: CertificateDer<'static>) -> RelayEndpoint {
        RelayEndpoint::first_party(addr, String::new(), cert)
    }

    /// FAILOVER: an unreachable first endpoint is marked unhealthy and the live one serves.
    #[tokio::test]
    async fn pool_fails_over_to_healthy_relay() {
        let origin = spawn_plain_http("failover-target").await;
        let relay_b = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        // Index 0 is tried first on the first connect.
        let endpoints = vec![
            endpoint("127.0.0.1:1".into(), relay_b.cert.clone()),
            endpoint(relay_b.addr.to_string(), relay_b.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));

        let stream = factory.connect(ORIGIN_NAME, origin.port()).await;
        assert!(stream.is_ok(), "pool must fail over to the healthy relay B");
        assert_eq!(relay_b.stats.dials(), 1, "relay B dialed the origin");

        let health = factory.health.lock().unwrap();
        assert!(health[0].is_some(), "relay A must be marked unhealthy after its failed dial");
        assert!(health[1].is_none(), "relay B stays healthy after a successful dial");
    }

    /// ALL-DOWN: `connect` errors and every endpoint is marked dead.
    #[tokio::test]
    async fn pool_all_down_errors_so_policy_applies() {
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;
        let endpoints = vec![
            endpoint("127.0.0.1:1".into(), cert.clone()),
            endpoint("127.0.0.1:2".into(), cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));

        assert!(
            factory.connect(ORIGIN_NAME, 443).await.is_err(),
            "an all-down pool must error so Prefer→direct / Strict→degrade applies"
        );
        let health = factory.health.lock().unwrap();
        assert!(
            health.iter().all(|h| h.is_some()),
            "every endpoint marked dead after all fail"
        );
    }

    /// ALL-DOWN via a comma-separated config, applied live: Prefer still goes direct.
    #[tokio::test]
    async fn pool_multi_endpoint_prefer_falls_back_direct_live() {
        let plain = spawn_plain_http("pool-prefer-fallback").await;
        let host = plain.ip().to_string();
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        let mut config = cfg(OverlayMode::Off, Some("127.0.0.1:1,127.0.0.1:2"));
        config.pollis_delivery_url = Some(format!("https://{host}"));
        config.overlay_relay_cert = Some(cert_b64(&cert));
        let state = provisioned_state(config).await;

        crate::commands::overlay::apply_overlay_mode(&state, OverlayMode::Prefer)
            .await
            .unwrap();
        let handle = state.overlay_handle().unwrap();

        assert!(
            socks5_connect(handle.socks_addr(), &host, plain.port()).await.is_ok(),
            "Prefer + whole pool down must still fall back to a direct dial"
        );
    }

    /// HEALTH/COOLDOWN: a cooled-down endpoint is skipped, then retried after expiry.
    #[tokio::test]
    async fn pool_skips_dead_endpoint_during_cooldown_then_retries() {
        let origin = spawn_plain_http("cooldown-target").await;
        let relay0 = spawn_pool_relay();
        let relay1 = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let cooldown = Duration::from_millis(150);
        let endpoints = vec![
            endpoint(relay0.addr.to_string(), relay0.cert.clone()),
            endpoint(relay1.addr.to_string(), relay1.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, cooldown);

        factory.health.lock().unwrap()[0] = Some(Instant::now() + cooldown);

        factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        assert_eq!(relay0.stats.dials(), 0, "dead endpoint 0 skipped while in cooldown");
        assert_eq!(relay1.stats.dials(), 1, "healthy endpoint 1 served the connect");

        tokio::time::sleep(cooldown + Duration::from_millis(50)).await;
        for _ in 0..4 {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert!(
            relay0.stats.dials() >= 1,
            "endpoint 0 is retried once its cooldown has expired"
        );
    }

    /// FAIL-OPEN: an all-dead pool is still tried.
    #[tokio::test]
    async fn pool_tries_all_dead_endpoints_fail_open() {
        let origin = spawn_plain_http("fail-open-target").await;
        let relay = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![endpoint(relay.addr.to_string(), relay.cert.clone())];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));

        factory.health.lock().unwrap()[0] = Some(Instant::now() + Duration::from_secs(30));

        factory
            .connect(ORIGIN_NAME, origin.port())
            .await
            .expect("fail-open: a fully-dead pool is still tried");
        assert_eq!(relay.stats.dials(), 1, "the dead endpoint was tried anyway");
        assert!(factory.health.lock().unwrap()[0].is_none());
    }

    /// LOAD SPREAD: rotation deals connects across healthy endpoints.
    #[tokio::test]
    async fn pool_spreads_load_across_healthy_relays() {
        let origin = spawn_plain_http("spread-target").await;
        let relay0 = spawn_pool_relay();
        let relay1 = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![
            endpoint(relay0.addr.to_string(), relay0.cert.clone()),
            endpoint(relay1.addr.to_string(), relay1.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));

        const N: u64 = 6;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        // Rotation, not exact halves:
        // real loopback latencies can legitimately demote one endpoint via the #812 band (exact halves flaked).
        assert_eq!(relay0.stats.dials() + relay1.stats.dials(), N);
        assert!(
            relay0.stats.dials() >= 1 && relay1.stats.dials() >= 1,
            "load collapsed onto one endpoint ({} vs {})",
            relay0.stats.dials(),
            relay1.stats.dials()
        );
    }

    /// A [`PathDialer`] that records each path's EXIT index and returns an in-memory pipe.
    struct RecordingDialer {
        addrs: Vec<String>,
        exits: Mutex<Vec<usize>>,
    }

    impl RecordingDialer {
        fn new(endpoints: &[RelayEndpoint]) -> Arc<Self> {
            Arc::new(RecordingDialer {
                addrs: endpoints.iter().map(|e| e.addr.clone()).collect(),
                exits: Mutex::new(Vec::new()),
            })
        }

        /// The exit endpoint index each `connect` chose, in call order.
        fn exits(&self) -> Vec<usize> {
            self.exits.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl PathDialer for RecordingDialer {
        async fn dial(
            &self,
            path: &RelayPath,
            _identity: &Arc<ClientIdentity>,
            _host: &str,
            _port: u16,
        ) -> anyhow::Result<BoxedStream> {
            let addr = &path.exit().endpoint().addr;
            let idx = self
                .addrs
                .iter()
                .position(|a| a == addr)
                .expect("the exit must be one of the pool's endpoints");
            self.exits.lock().unwrap().push(idx);
            let (near, _far) = tokio::io::duplex(1);
            Ok(BoxedStream::new(near))
        }
    }

    /// A pool of `n` never-dialed endpoints; identity pre-cached so the dangling `Weak` is never used.
    fn ordering_factory(n: usize) -> (RealRelayFactory, Arc<RecordingDialer>) {
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        let endpoints: Vec<RelayEndpoint> = (0..n)
            .map(|i| endpoint(format!("relay-{i}.invalid:443"), cert.clone()))
            .collect();
        let dialer = RecordingDialer::new(&endpoints);
        let mut factory = RealRelayFactory::new(
            Weak::new(),
            endpoints,
            Duration::from_secs(30),
            Duration::from_secs(2),
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
        )
        .with_dialer(dialer.clone());
        *factory.identity.get_mut() = Some(identity());
        (factory, dialer)
    }

    /// Pin measured latencies, bypassing the EWMA, before every `connect`.
    fn seed_latency(factory: &RealRelayFactory, millis: &[u64]) {
        let mut latency = factory.latency.lock().unwrap();
        for (i, ms) in millis.iter().enumerate() {
            latency[i] = Some(Duration::from_millis(*ms));
        }
    }

    /// #812: a relay slower than the band is not dialed while a fast one is healthy.
    #[tokio::test]
    async fn pool_prefers_the_lower_latency_relay() {
        let (factory, dialer) = ordering_factory(2);

        const N: usize = 6;
        for _ in 0..N {
            seed_latency(&factory, &[15, 69]);
            factory.connect(ORIGIN_NAME, 443).await.unwrap();
        }
        assert_eq!(
            dialer.exits(),
            vec![0; N],
            "every dial must take the fast relay; the +54ms one is never preferred"
        );
    }

    /// #812: relays within the band keep rotating.
    #[tokio::test]
    async fn pool_rotates_within_the_latency_band() {
        let (factory, dialer) = ordering_factory(2);

        const N: usize = 6;
        for _ in 0..N {
            seed_latency(&factory, &[10, 15]);
            factory.connect(ORIGIN_NAME, 443).await.unwrap();
        }
        assert_eq!(
            dialer.exits(),
            vec![0, 1, 0, 1, 0, 1],
            "in-band endpoints keep alternating"
        );
    }

    /// #812: the explore dial lets a demoted endpoint earn its way back.
    #[tokio::test]
    async fn pool_explores_the_slow_relay_periodically() {
        let (factory, dialer) = ordering_factory(2);

        // One explore period; its last dial (n=2, seq odd) starts at endpoint 1.
        for _ in 0..RELAY_EXPLORE_EVERY {
            seed_latency(&factory, &[15, 500]);
            factory.connect(ORIGIN_NAME, 443).await.unwrap();
        }
        let mut expected = vec![0; RELAY_EXPLORE_EVERY - 1];
        expected.push(1);
        assert_eq!(
            dialer.exits(),
            expected,
            "the slow relay is re-sampled exactly once per explore period, and \
             every other dial takes the fast one"
        );
    }

    /// #812 reorders only HEALTHY endpoints; fail-open is intact.
    #[tokio::test]
    async fn pool_latency_order_never_overrides_fail_open() {
        let origin = spawn_plain_http("failopen-target").await;
        let slow_live = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![
            endpoint("127.0.0.1:1".to_string(), slow_live.cert.clone()),
            endpoint(slow_live.addr.to_string(), slow_live.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));
        factory.record_latency(0, Duration::from_millis(1));
        factory.record_latency(1, Duration::from_millis(400));

        factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        assert_eq!(slow_live.stats.dials(), 1, "the slow live relay carried it");
    }

    /// A pool factory with explicit path length, revocation gate and guard book.
    fn path_factory(
        state: &Arc<AppState>,
        endpoints: Vec<RelayEndpoint>,
        admission: Arc<RelayAdmission>,
        guards: Arc<GuardBook>,
        target_hops: usize,
    ) -> RealRelayFactory {
        let mut factory = RealRelayFactory::new(
            Arc::downgrade(state),
            endpoints,
            Duration::from_secs(30),
            Duration::from_secs(4),
            admission,
            guards,
        );
        factory.target_hops = target_hops;
        factory
    }

    fn peer_endpoint(addr: String, cert: CertificateDer<'static>) -> RelayEndpoint {
        RelayEndpoint::peer(addr, String::new(), cert)
    }

    fn endpoint_in(addr: String, region: &str, cert: CertificateDer<'static>) -> RelayEndpoint {
        RelayEndpoint::first_party(addr, region.to_string(), cert)
    }

    /// A pool with a spare builds a real MULTI-HOP circuit, read off the relays' own counters.
    #[tokio::test]
    async fn a_three_relay_pool_builds_a_two_hop_circuit() {
        let origin = spawn_plain_http("two-hop-target").await;
        let relays = [spawn_pool_relay(), spawn_pool_relay(), spawn_pool_relay()];
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints: Vec<RelayEndpoint> = relays
            .iter()
            .map(|r| endpoint(r.addr.to_string(), r.cert.clone()))
            .collect();
        let factory = path_factory(
            &state,
            endpoints,
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        const N: u64 = 6;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }

        let dials: u64 = relays.iter().map(|r| r.stats.dials()).sum();
        let extends: u64 = relays.iter().map(|r| r.stats.extends()).sum();
        assert_eq!(dials, N, "exactly one hop per circuit reaches the destination");
        assert_eq!(extends, N, "a 2-hop circuit extends exactly once");
        // Distinct entry and exit per circuit (`plan_path`); a guard may exit a *different* circuit.
        assert!(
            relays.iter().filter(|r| r.stats.extends() > 0).count() >= 1,
            "no relay ever acted as an entry hop"
        );
        for r in &relays {
            assert!(
                r.stats.dials() + r.stats.extends() <= N,
                "a relay served more positions than there were circuits"
            );
        }
    }

    /// A pool with no spare stays SINGLE-hop.
    #[tokio::test]
    async fn a_two_relay_pool_stays_single_hop() {
        let origin = spawn_plain_http("single-hop-target").await;
        let relays = [spawn_pool_relay(), spawn_pool_relay()];
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints: Vec<RelayEndpoint> = relays
            .iter()
            .map(|r| endpoint(r.addr.to_string(), r.cert.clone()))
            .collect();
        let factory = path_factory(
            &state,
            endpoints,
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        for _ in 0..4u32 {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        let extends: u64 = relays.iter().map(|r| r.stats.extends()).sum();
        assert_eq!(extends, 0, "a 2-node pool must not build multi-hop paths");
        assert_eq!(relays.iter().map(|r| r.stats.dials()).sum::<u64>(), 4);
    }

    /// On the wire: a peer is never the hop that reaches the destination (§4.4).
    #[tokio::test]
    async fn a_peer_relay_is_never_the_last_hop() {
        let origin = spawn_plain_http("peer-exit-target").await;
        let first_a = spawn_pool_relay();
        let first_b = spawn_pool_relay();
        let peer = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![
            endpoint(first_a.addr.to_string(), first_a.cert.clone()),
            peer_endpoint(peer.addr.to_string(), peer.cert.clone()),
            endpoint(first_b.addr.to_string(), first_b.cert.clone()),
        ];
        let factory = path_factory(
            &state,
            endpoints,
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        const N: u64 = 8;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert_eq!(
            peer.stats.dials(),
            0,
            "a peer relay reached the destination — the first-party exit invariant is broken"
        );
        assert_eq!(
            peer.stats.extends(),
            0,
            "a peer was used as an entry hop; peers are middle-only"
        );
        assert_eq!(
            first_a.stats.dials() + first_b.stats.dials(),
            N,
            "every circuit exited through a first-party relay"
        );
    }

    /// ...and a peer IS carried as a middle hop.
    #[tokio::test]
    async fn a_peer_relay_is_usable_as_a_middle_hop_on_the_wire() {
        let origin = spawn_plain_http("peer-middle-target").await;
        let a = spawn_pool_relay();
        let b = spawn_pool_relay();
        let c = spawn_pool_relay();
        let peer = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        // A distinct region, so middle selection prefers it.
        let endpoints = vec![
            endpoint_in(a.addr.to_string(), "us-west-2", a.cert.clone()),
            endpoint_in(b.addr.to_string(), "us-west-2", b.cert.clone()),
            RelayEndpoint::peer(peer.addr.to_string(), "eu-west-1".into(), peer.cert.clone()),
            endpoint_in(c.addr.to_string(), "us-west-2", c.cert.clone()),
        ];
        let factory = path_factory(
            &state,
            endpoints,
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
            3,
        );

        const N: u64 = 4;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert!(
            peer.stats.extends() > 0,
            "the peer was never used as a middle hop"
        );
        assert_eq!(peer.stats.dials(), 0, "the peer must never be the exit");
        let extends: u64 = [&a, &b, &c, &peer].iter().map(|r| r.stats.extends()).sum();
        assert_eq!(extends, N * 2, "a 3-hop circuit extends twice");
    }

    /// GUARDS: at most [`path::GUARD_SET_SIZE`] relays ever take the entry position.
    #[tokio::test]
    async fn guard_pinning_bounds_the_entry_set() {
        let origin = spawn_plain_http("guard-target").await;
        let relays: Vec<TestRelay> = (0..4).map(|_| spawn_pool_relay()).collect();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints: Vec<RelayEndpoint> = relays
            .iter()
            .map(|r| endpoint(r.addr.to_string(), r.cert.clone()))
            .collect();
        let book = Arc::new(GuardBook::ephemeral());
        let factory = path_factory(
            &state,
            endpoints,
            Arc::new(RelayAdmission::unenforced()),
            book,
            path::TARGET_HOPS,
        );

        for _ in 0..12u32 {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        let entries = relays.iter().filter(|r| r.stats.extends() > 0).count();
        assert!(entries >= 1, "somebody must have been the entry hop");
        assert!(
            entries <= path::GUARD_SET_SIZE,
            "entry hops were not pinned: {entries} distinct relays saw the client's address"
        );
        assert!(
            relays.iter().any(|r| r.stats.extends() == 0),
            "every relay served as an entry — that is a random first hop, not a guard"
        );
    }

    /// An enforcing gate holding a fresh list revoking `revoked_addrs`, with a matching anchor.
    fn enforcing_gate(revoked_addrs: &[&str], now: i64, expires_at: i64) -> Arc<RelayAdmission> {
        let revoked: Vec<serde_json::Value> = revoked_addrs
            .iter()
            .map(|a| serde_json::json!({ "addr": a }))
            .collect();
        enforcing_gate_for(revoked, now, expires_at)
    }

    /// The same with full entries; `cert_sha256_b64` is the only selector that can name a peer.
    fn enforcing_gate_for(
        revoked: Vec<serde_json::Value>,
        now: i64,
        expires_at: i64,
    ) -> Arc<RelayAdmission> {
        use base64::Engine as _;
        use ed25519_dalek::{Signer, SigningKey};

        let sk = SigningKey::from_bytes(&[21u8; 32]);
        let key_b64 =
            base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        let gate = Arc::new(RelayAdmission::enforcing(&key_b64));

        let revoked_count = revoked.len();
        let payload = serde_json::to_string(&serde_json::json!({
            "version": pollis_relay::REVOCATION_VERSION,
            "type": pollis_relay::REVOCATION_TYPE,
            "seq": 5,
            "issued_at": now - 10,
            "expires_at": expires_at,
            "revoked": revoked,
        }))
        .unwrap();
        let sig = sk.sign(payload.as_bytes());
        let envelope = serde_json::to_vec(&serde_json::json!({
            "payload_b64": base64::engine::general_purpose::STANDARD.encode(payload.as_bytes()),
            "signature_b64": base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()),
        }))
        .unwrap();

        let dir_json = format!(
            r#"{{"version":1,"issued_at":1,"expires_at":9999999999,"relays":[{{"addr":"x:1","cert_b64":"QUJD"}}],"revocation":{{"seq":5,"count":{revoked_count}}}}}"#
        );
        let dir: directory::Directory = serde_json::from_str(&dir_json).unwrap();
        gate.observe_directory(&dir);
        gate.install(&envelope, now).expect("list installs");
        gate
    }

    /// A revoked relay is NEVER dialed.
    #[tokio::test]
    async fn a_revoked_relay_is_never_dialed() {
        let origin = spawn_plain_http("revocation-target").await;
        let good = spawn_pool_relay();
        let seized = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let now = crate::util::now_unix() as i64;
        let gate = enforcing_gate(&[&seized.addr.to_string()], now, now + 3_600);
        let endpoints = vec![
            endpoint(seized.addr.to_string(), seized.cert.clone()),
            endpoint(good.addr.to_string(), good.cert.clone()),
        ];
        let factory = path_factory(
            &state,
            endpoints,
            gate,
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        const N: u64 = 6;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert_eq!(seized.stats.dials(), 0, "a revoked relay carried traffic");
        assert_eq!(seized.stats.extends(), 0, "a revoked relay was an entry hop");
        assert_eq!(good.stats.dials(), N, "the unrevoked relay carried everything");
    }

    /// FAIL-CLOSED: anchored list, none held ⇒ empty pool, `connect` errors.
    #[tokio::test]
    async fn an_unevaluable_revocation_list_empties_the_pool() {
        let origin = spawn_plain_http("failclosed-target").await;
        let relay = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let gate = Arc::new(RelayAdmission::enforcing("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="));
        let dir: directory::Directory = serde_json::from_str(
            r#"{"version":1,"issued_at":1,"expires_at":9999999999,"relays":[{"addr":"x:1","cert_b64":"QUJD"}],"revocation":{"seq":9,"count":2}}"#,
        )
        .unwrap();
        gate.observe_directory(&dir);

        let endpoints = vec![endpoint(relay.addr.to_string(), relay.cert.clone())];
        let factory = path_factory(
            &state,
            endpoints,
            gate,
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        assert!(
            factory.connect(ORIGIN_NAME, origin.port()).await.is_err(),
            "an unevaluable revocation list must empty the pool, not pass it through"
        );
        assert_eq!(
            relay.stats.dials(),
            0,
            "a relay was dialed while revocation could not be evaluated"
        );
    }

    /// A list expiring mid-session closes the pool at USE time.
    #[tokio::test]
    async fn an_expired_revocation_list_closes_the_pool_at_use_time() {
        let origin = spawn_plain_http("expiry-target").await;
        let relay = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let now = crate::util::now_unix() as i64;
        let gate = enforcing_gate(&["198.51.100.9:9444"], now - 100, now - 1);
        let endpoints = vec![endpoint(relay.addr.to_string(), relay.cert.clone())];
        let factory = path_factory(
            &state,
            endpoints,
            gate,
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        assert!(
            factory.connect(ORIGIN_NAME, origin.port()).await.is_err(),
            "an expired list is not evidence; the pool must close"
        );
        assert_eq!(relay.stats.dials(), 0);
    }

    fn cert_b64_of(cert: &CertificateDer<'static>) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(cert.as_ref())
    }

    fn cert_digest_b64_of(cert: &CertificateDer<'static>) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(pollis_relay::proto::cert_fingerprint(cert.as_ref()))
    }

    /// A directory JSON with `relays` and optional `peers`, parsed as `verify_directory` would.
    fn directory_with(relays: &[(&str, &CertificateDer<'static>)], peers: serde_json::Value) -> directory::Directory {
        let relays: Vec<serde_json::Value> = relays
            .iter()
            .map(|(addr, cert)| {
                serde_json::json!({ "addr": addr, "region": "us-west-2", "cert_b64": cert_b64_of(cert) })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "issued_at": 1,
            "expires_at": 9_999_999_999i64,
            "relays": relays,
            "peers": peers,
        }))
        .expect("directory parses")
    }

    /// Peer entries in the signed directory become `Peer`-tier endpoints.
    #[test]
    fn directory_peers_become_peer_tier_endpoints() {
        let a = CertificateDer::from(vec![1u8; 24]);
        let b = CertificateDer::from(vec![2u8; 24]);
        let p = CertificateDer::from(vec![9u8; 24]);
        let dir = directory_with(
            &[("203.0.113.7:9444", &a), ("203.0.113.8:9444", &b)],
            serde_json::json!([{ "cert_b64": cert_b64_of(&p), "parked_at": ["203.0.113.8:9444", "203.0.113.7:9444"] }]),
        );

        let endpoints = directory_to_endpoints(&dir);
        assert_eq!(endpoints.len(), 3);
        assert!(endpoints[..2]
            .iter()
            .all(|e| e.tier == path::RelayTier::FirstParty));
        let peer = &endpoints[2];
        assert_eq!(peer.tier, path::RelayTier::Peer);
        assert_eq!(peer.cert.as_ref(), p.as_ref(), "the client pins the peer's own leaf");
        // The rendezvous is the first ADVERTISED relay it is parked at.
        assert_eq!(peer.addr, "203.0.113.8:9444");
    }

    /// No `peers`: the same pool and path length as before wave 3.
    #[test]
    fn a_directory_without_peers_is_unchanged() {
        let a = CertificateDer::from(vec![1u8; 24]);
        let b = CertificateDer::from(vec![2u8; 24]);
        let dir: directory::Directory = serde_json::from_value(serde_json::json!({
            "version": 1, "issued_at": 1, "expires_at": 9_999_999_999i64,
            "relays": [
                { "addr": "203.0.113.7:9444", "region": "us-west-2", "cert_b64": cert_b64_of(&a) },
                { "addr": "203.0.113.8:9444", "region": "us-west-2", "cert_b64": cert_b64_of(&b) },
            ],
        }))
        .unwrap();
        let endpoints = directory_to_endpoints(&dir);
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.iter().all(|e| e.tier == path::RelayTier::FirstParty));
    }

    /// Unusable peer entries are dropped, never fatal.
    #[test]
    fn unusable_peer_entries_are_dropped() {
        let a = CertificateDer::from(vec![1u8; 24]);
        let p = CertificateDer::from(vec![9u8; 24]);
        let dir = directory_with(
            &[("203.0.113.7:9444", &a)],
            serde_json::json!([
                { "cert_b64": "!!!not base64!!!", "parked_at": ["203.0.113.7:9444"] },
                { "cert_b64": cert_b64_of(&p), "parked_at": ["198.51.100.9:9444"] },
                { "cert_b64": cert_b64_of(&p) },
            ]),
        );
        let endpoints = directory_to_endpoints(&dir);
        assert_eq!(endpoints.len(), 1, "an unusable peer entered the pool");
        assert_eq!(endpoints[0].tier, path::RelayTier::FirstParty);
    }

    /// A directory peer is planned into the MIDDLE of a 3-hop path, and the pool raises its own length.
    ///
    /// Plan only: a parked peer isn't dialable; the wire proof is `a_peer_relay_is_usable_as_a_middle_hop_on_the_wire`.
    #[test]
    fn a_directory_published_peer_is_planned_as_the_middle_hop() {
        let a = CertificateDer::from(vec![1u8; 24]);
        let b = CertificateDer::from(vec![2u8; 24]);
        let c = CertificateDer::from(vec![3u8; 24]);
        let p = CertificateDer::from(vec![9u8; 24]);
        let dir = directory_with(
            &[
                ("203.0.113.7:9444", &a),
                ("203.0.113.8:9444", &b),
                ("203.0.113.9:9444", &c),
            ],
            serde_json::json!([{
                "cert_b64": cert_b64_of(&p),
                "parked_at": ["203.0.113.7:9444", "203.0.113.8:9444", "203.0.113.9:9444"],
            }]),
        );
        let endpoints = directory_to_endpoints(&dir);
        assert_eq!(endpoints.len(), 4);

        // Checked through `RealRelayFactory::new`, which decides the length.
        let with_peer = pool_factory_for(&endpoints);
        assert_eq!(with_peer, path::MAX_PATH_HOPS);
        assert_eq!(
            pool_factory_for(&endpoints[..3]),
            path::TARGET_HOPS,
            "a peerless pool must not grow paths"
        );

        let order: Vec<usize> = (0..endpoints.len()).collect();
        let path = path::plan_path(&endpoints, &order, &[0], 1, 0, with_peer).expect("path");
        let hops = path.endpoints();
        assert_eq!(hops.len(), 3, "the peer did not earn its middle hop");
        assert_eq!(hops[0].tier, path::RelayTier::FirstParty, "the guard is ours");
        assert_eq!(hops[1].tier, path::RelayTier::Peer);
        assert_eq!(hops[1].cert.as_ref(), p.as_ref());
        assert_eq!(hops[2].tier, path::RelayTier::FirstParty, "the exit is ours");
    }

    /// The `target_hops` a production factory settles on for this pool.
    fn pool_factory_for(endpoints: &[RelayEndpoint]) -> usize {
        let state: Weak<AppState> = Weak::new();
        RealRelayFactory::new(
            state,
            endpoints.to_vec(),
            Duration::from_secs(30),
            Duration::from_secs(4),
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
        )
        .target_hops
    }

    /// A peer whose cert is revoked is refused client-side even if an older directory still lists it.
    #[tokio::test]
    async fn a_revoked_peer_is_never_used_as_a_middle() {
        let origin = spawn_plain_http("revoked-peer-target").await;
        let a = spawn_pool_relay();
        let b = spawn_pool_relay();
        let c = spawn_pool_relay();
        let peer = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let dir = directory_with(
            &[
                (&a.addr.to_string(), &a.cert),
                (&b.addr.to_string(), &b.cert),
                (&c.addr.to_string(), &c.cert),
            ],
            serde_json::json!([{
                "cert_b64": cert_b64_of(&peer.cert),
                "parked_at": [a.addr.to_string(), b.addr.to_string(), c.addr.to_string()],
            }]),
        );
        let now = crate::util::now_unix() as i64;
        let gate = enforcing_gate_for(
            vec![serde_json::json!({ "cert_sha256_b64": cert_digest_b64_of(&peer.cert) })],
            now,
            now + 3_600,
        );

        let factory = RealRelayFactory::new(
            Arc::downgrade(&state),
            directory_to_endpoints(&dir),
            Duration::from_secs(30),
            Duration::from_secs(4),
            gate,
            Arc::new(GuardBook::ephemeral()),
        );

        const N: u64 = 4;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert_eq!(peer.stats.extends(), 0, "a REVOKED peer carried traffic");
        assert_eq!(peer.stats.dials(), 0);
        // The pool routes around the revoked peer with a first-party-only path.
        assert_eq!(
            a.stats.dials() + b.stats.dials() + c.stats.dials(),
            N,
            "revoking a peer must not cost delivery"
        );
    }

    /// Counts circuit requests, to show which flows reached the overlay.
    struct CountingFactory {
        inner: Arc<dyn CircuitFactory>,
        count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl CircuitFactory for CountingFactory {
        async fn connect(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream> {
            self.count.fetch_add(1, Ordering::Relaxed);
            self.inner.connect(host, port).await
        }
    }

    /// For ONE host and port, TCP signaling rides a circuit while UDP media goes straight to the origin.
    ///
    /// The SOCKS5 CONNECT transport enforces the §6.4 media rule, which no host list could.
    #[tokio::test]
    async fn a_socks_shim_cannot_carry_media_even_for_an_overlaid_host() {
        use tokio::net::UdpSocket;

        let media = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let sfu_addr = media.local_addr().unwrap();
        let signaling = TcpListener::bind(sfu_addr).await.expect("same port, TCP side");
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = signaling.accept().await {
                tokio::spawn(async move {
                    let _ = read_http_head(&mut sock).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nsignaling",
                        )
                        .await;
                });
            }
        });
        tokio::spawn(async move {
            let mut buf = [0u8; 256];
            while let Ok((n, from)) = media.recv_from(&mut buf).await {
                let _ = media.send_to(&buf[..n], from).await;
            }
        });

        let host = sfu_addr.ip().to_string();
        let relay = spawn_relay(&[host.as_str()], &[]);
        let circuits = Arc::new(AtomicUsize::new(0));
        let factory: Arc<dyn CircuitFactory> = Arc::new(CountingFactory {
            inner: relay.factory(),
            count: circuits.clone(),
        });
        // Overlay-route the SFU deliberately (stronger than shipped), so media staying direct proves §6.4 by transport.
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([host.as_str()]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, factory).await.unwrap();

        let mut stream = socks5_connect(shim.socks_addr(), &host, sfu_addr.port())
            .await
            .expect("signaling routes through the overlay");
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: sfu\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp).contains("signaling"));
        assert_eq!(circuits.load(Ordering::Relaxed), 1, "signaling used a circuit");
        assert_eq!(relay.stats.dials(), 1, "the relay dialed the SFU for signaling");

        // (2) MEDIA: a datagram to the same host:port never touches the shim.
        let rtp = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        rtp.send_to(b"rtp-frame", sfu_addr).await.unwrap();
        let mut buf = [0u8; 64];
        let (n, from) = tokio::time::timeout(Duration::from_secs(2), rtp.recv_from(&mut buf))
            .await
            .expect("media must not be blocked by the overlay")
            .unwrap();
        assert_eq!(&buf[..n], b"rtp-frame");
        assert_eq!(from, sfu_addr, "media came straight back from the SFU");

        assert_eq!(
            circuits.load(Ordering::Relaxed),
            1,
            "MEDIA WAS OVERLAID — §6.4's plane split is broken"
        );
        assert_eq!(
            relay.stats.dials(),
            1,
            "the relay saw a second dial; media must never traverse it"
        );
        drop(shim);
    }
}
