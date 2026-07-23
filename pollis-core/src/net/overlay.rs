//! Closed-overlay relay wiring for `pollis-core` (design
//! `docs/relay-overlay-design.md` §14). This is the CONSUMER side of the
//! `pollis-relay` transport crate: it derives the routing policy from `Config`,
//! builds the real circuit factory + starts the loopback SOCKS5 shim
//! ([`start_overlay_shim`]), hands out the shared reqwest client, and builds the
//! libsql SOCKS connector. The runtime on/off/switch engine that DRIVES this lives
//! in [`crate::commands::overlay`] (`set_overlay_mode` / `apply_overlay_mode`).
//!
//! **Off-by-default is sacred.** With `POLLIS_OVERLAY` unset (`OverlayMode::Off`)
//! no shim is ever started, `AppState.overlay` stays `None`, and every network
//! path is byte-for-byte identical to a build without the overlay: [`http_client`]
//! returns a proxy-less `reqwest::Client`, and `RemoteDb::connect` takes libsql's
//! unchanged `.build()` path (no `.connector()`).

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use hyper::client::connect::{Connected, Connection};
use hyper::Uri;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::Mutex as AsyncMutex;
use tower_service::Service;

use pollis_relay::circuit::{Circuit, CircuitFactory, Hop};
use pollis_relay::client::ClientIdentity;
use pollis_relay::proto::DeviceCertMaterial;
use pollis_relay::stream::BoxedStream;
use pollis_relay::{
    Allowlist, CertificateDer, OverlayHandle, OverlayMode, OverlayShim, RoutingPolicy,
};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::state::AppState;

// ── The shared reqwest seam (design §14.2) ─────────────────────────────────

/// The one HTTP client every control-plane caller uses instead of
/// `reqwest::Client::new()`. When `overlay` is `Some`, the client is pointed at
/// the loopback SOCKS5 shim (`socks5h://`, proxy-side DNS) so the real hostname
/// reaches the relay and the inner TLS still terminates at the real service; when
/// `None`, it is a plain client — identical to `reqwest::Client::new()`.
///
/// Concentrating every call site here also retires the per-call
/// `reqwest::Client::new()` anti-pattern (design §14.2).
pub(crate) fn http_client(overlay: Option<&OverlayHandle>) -> reqwest::Client {
    pollis_relay::http::http_client(overlay)
}

// ── Policy derivation (the plane split, design §6.4) ───────────────────────

/// Extract the bare host from a URL (`libsql://`, `https://`, `wss://`, or a
/// bare `host:port`). Ports, schemes, paths, and queries are stripped — the
/// allowlist matches on host only, and that is what the shim sees per request.
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

/// Build the per-host routing policy from `Config` for a given runtime `mode`:
/// the first-party control plane (Turso + optional commit-log DB, the DS, R2)
/// routes through the overlay; LiveKit stays DIRECT in every mode (the media
/// plane, §6.4). Any host not on either list is dialed direct (e.g. non-first-
/// party Expo push, §14.4). The mode is passed explicitly (not read from
/// `config.overlay_mode`) because it is now a RUNTIME value the shim can flip.
pub(crate) fn overlay_policy(config: &Config, mode: OverlayMode) -> RoutingPolicy {
    let mut overlay_hosts: Vec<String> = Vec::new();
    let control_urls = [
        Some(config.turso_url.as_str()),
        config.log_db_url.as_deref(),
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

    // Media plane: LiveKit is always direct (§6.4), even in Strict mode.
    let direct_hosts: Vec<String> = host_of(&config.livekit_url).into_iter().collect();

    RoutingPolicy::new(
        mode,
        Allowlist::from_patterns(overlay_hosts),
        Allowlist::from_patterns(direct_hosts),
    )
}

// ── The real circuit factory (design §9.2, §9.4, §14.1) ────────────────────

/// The `identity_version` stamped into the relay handshake cert this client
/// mints locally (see [`RealRelayFactory`]). The relay verifies a device cert for
/// **self-consistency only** — that the presented account key signed *this*
/// device key at *this* `(version, issued_at)` — and never cross-checks the value
/// against `users.identity_version`. So a fixed version is sufficient and correct
/// for the handshake; the rate limiter keys on `account_id_pub` (the real one),
/// not the version.
const OVERLAY_CERT_IDENTITY_VERSION: u32 = 1;

/// How long a relay endpoint stays marked dead after a failed dial before it is
/// eligible again. Mark-dead-on-failure + cooldown is the *event-driven*
/// alternative to a background health-poll loop (CLAUDE.md forbids periodic
/// keepalives): a dead relay is simply skipped until this window elapses, then
/// retried on the next connect that reaches it. Mirrors `RemoteDb::with_retry`'s
/// reconnect-on-demand posture — recover lazily, never poll.
const RELAY_DEAD_COOLDOWN: Duration = Duration::from_secs(30);

/// Upper bound on a single endpoint's dial (QUIC handshake + CONNECT). Without
/// it, a relay that is *unreachable* (packets dropped, no ICMP) stalls on the
/// QUIC handshake timeout — which would defeat the pool's purpose: a dead relay
/// must fail over FAST, not hang delivery. On timeout the endpoint is treated as
/// failed (marked dead) and the next candidate is tried. Generous enough for a
/// real first-party relay handshake over the internet.
const RELAY_DIAL_TIMEOUT: Duration = Duration::from_secs(8);

/// A resolved relay endpoint: the `host:port` to dial and the pinned QUIC leaf
/// the client verifies it against (the relay's identity *is* its cert, §7).
#[derive(Clone)]
struct RelayEndpoint {
    /// As configured; resolved to a `SocketAddr` per dial (v0 relays are a small
    /// known set, so a fresh lookup per circuit is fine).
    addr: String,
    cert: CertificateDer<'static>,
}

/// The production [`CircuitFactory`]: on each `connect`, present the logged-in
/// device's [`ClientIdentity`] and dial the configured relay, returning the
/// resulting byte pipe (over which the caller runs its own TLS to the real host).
///
/// **Identity (design §9.4).** The `ClientIdentity` carries the device Ed25519
/// signing key — the SAME key `ds_client` signs DS writes with and that
/// `user_device.mls_signature_pub` records — plus the offline cert chain
/// (`account_id_pub` + `device_cert` + `version`/`issued_at`) the relay verifies
/// with zero I/O. Both halves are loaded from LOCAL state: the device signing key
/// from the open local DB (openmls storage), and the cert is minted on the spot
/// from the locally-held account identity key (`load_account_id_key`). Minting
/// locally — rather than reading the published `user_device.device_cert` through
/// `remote_db` — is deliberate: once the mode is applied, `remote_db` itself
/// routes through THIS shim, so reading the cert from it to *build* a circuit
/// would recurse into the very circuit being built. The minted cert is
/// cryptographically identical in what the relay checks (the current account key
/// certifying the real device key), and a device with NO account key yet
/// (pre-enrollment / locked / no user) simply can't mint one → `connect` errors →
/// `Prefer` falls back to direct and `Strict` degrades, never a silent send.
///
/// **Caching.** The built identity is cached (`identity`), but re-derived if the
/// cache is empty, so a device re-enroll is tolerated: `set_overlay_mode` rebuilds
/// the factory on the next apply, and each fresh factory reloads.
///
/// **Pool + failover (design §14.1, the "messages must work" slice).** The
/// factory holds a POOL of first-party relays and, per `connect`, tries them in
/// health order, returning the FIRST success; only when EVERY candidate fails
/// does it error — so `Prefer` still falls back to direct and `Strict` still
/// degrades, but only once the whole pool is exhausted, never on one dead relay.
/// Health is tracked inline (`health[i]` = `Some(dead_until)` while endpoint `i`
/// is in its cooldown after a failed dial, `None` when healthy): a failed dial
/// marks the endpoint dead for [`RELAY_DEAD_COOLDOWN`], a success clears it. There
/// is **no background poll** — recovery is lazy (the cooldown expires and the next
/// connect retries it), matching `RemoteDb::with_retry`. Selection is *fail-open*:
/// healthy endpoints are tried first, but if all are marked dead they are still
/// tried (a transient outage that marked the whole pool dead must never wedge it
/// permanently). A rotating start index (`next_start`) spreads load across healthy
/// endpoints instead of always hammering endpoint 0.
struct RealRelayFactory {
    /// Weak so the factory (owned by the shim task, owned by `AppState.overlay`)
    /// does not form a reference cycle back into `AppState`.
    state: Weak<AppState>,
    endpoints: Vec<RelayEndpoint>,
    identity: AsyncMutex<Option<Arc<ClientIdentity>>>,
    /// Per-endpoint health, indexed parallel to `endpoints`: `Some(dead_until)`
    /// while in cooldown after a failed dial, `None` when healthy. A plain
    /// `std::sync::Mutex` (never held across an await) — the guard is dropped
    /// before any I/O.
    health: Mutex<Vec<Option<Instant>>>,
    /// Rotating start offset for load spread: each `connect` bumps this so the
    /// pool doesn't always begin at endpoint 0.
    next_start: AtomicUsize,
    /// How long a failed endpoint stays dead. `RELAY_DEAD_COOLDOWN` in production;
    /// tests inject a short value to exercise recovery.
    cooldown: Duration,
    /// Upper bound on a single dial. `RELAY_DIAL_TIMEOUT` in production; tests
    /// inject a short value so an unreachable endpoint fails over fast.
    dial_timeout: Duration,
}

impl RealRelayFactory {
    /// Build a factory over `endpoints`, all initially healthy.
    fn new(
        state: Weak<AppState>,
        endpoints: Vec<RelayEndpoint>,
        cooldown: Duration,
        dial_timeout: Duration,
    ) -> Self {
        let n = endpoints.len();
        RealRelayFactory {
            state,
            endpoints,
            identity: AsyncMutex::new(None),
            health: Mutex::new(vec![None; n]),
            next_start: AtomicUsize::new(0),
            cooldown,
            dial_timeout,
        }
    }

    /// The order to try endpoints in for the next dial: healthy endpoints first
    /// (rotated by `next_start` so load spreads), then any still in cooldown
    /// (fail-open — always tried, so a fully-dead pool is never permanently
    /// wedged). Returns endpoint indices. The health lock is taken and dropped
    /// here; no I/O happens under it.
    fn candidate_order(&self) -> Vec<usize> {
        let n = self.endpoints.len();
        let start = self.next_start.fetch_add(1, Ordering::Relaxed) % n;
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
        healthy.extend(dead);
        healthy
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

    /// Dial a single endpoint: resolve → single-hop circuit → connect to the
    /// target. Kept single-hop (v0 is n=1 first-party); the pool decides *which*
    /// relay, not *how many hops*.
    async fn dial_endpoint(
        &self,
        endpoint: &RelayEndpoint,
        identity: &Arc<ClientIdentity>,
        host: &str,
        port: u16,
    ) -> anyhow::Result<BoxedStream> {
        let addr = tokio::net::lookup_host(&endpoint.addr)
            .await
            .map_err(|e| anyhow::anyhow!("overlay: resolve relay {}: {e}", endpoint.addr))?
            .next()
            .ok_or_else(|| anyhow::anyhow!("overlay: relay {} did not resolve", endpoint.addr))?;
        let circuit = Circuit::build_single_hop(Hop::new(addr, endpoint.cert.clone()), identity.clone());
        circuit.connect(host, port).await
    }

    /// Load (or reuse the cached) device `ClientIdentity`. Errors — fail-closed —
    /// when the device isn't in a state to authenticate to a relay (no user, no
    /// local DB, locked/absent account key).
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
        let id = Arc::new(build_client_identity(&state).await?);
        *self.identity.lock().await = Some(id.clone());
        Ok(id)
    }
}

#[async_trait::async_trait]
impl CircuitFactory for RealRelayFactory {
    async fn connect(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream> {
        // Fail-closed when nothing is configured (no endpoint / no pinned cert):
        // `Prefer` then dials direct and `Strict` degrades, same as today.
        if self.endpoints.is_empty() {
            anyhow::bail!("overlay: no relay endpoint / pinned cert configured");
        }
        let identity = self.identity().await?;

        // Try the pool in health order and return the FIRST success. Only when
        // EVERY candidate fails do we error — so a single dead relay never wedges
        // delivery, but an all-down pool still surfaces (Prefer→direct,
        // Strict→degrade). Each failed dial marks that endpoint dead; each success
        // clears it.
        let mut last_err = None;
        for idx in self.candidate_order() {
            let dial = self.dial_endpoint(&self.endpoints[idx], &identity, host, port);
            let outcome = match tokio::time::timeout(self.dial_timeout, dial).await {
                Ok(res) => res,
                Err(_) => Err(anyhow::anyhow!(
                    "overlay: relay {} dial timed out",
                    self.endpoints[idx].addr
                )),
            };
            match outcome {
                Ok(stream) => {
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
async fn build_client_identity(state: &Arc<AppState>) -> anyhow::Result<ClientIdentity> {
    let user_id = overlay_signing_user(state).await?;

    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("overlay: device_id not set (not logged in)"))?;

    // Account identity key (local): absent/locked ⇒ fail-closed (pre-enrollment).
    let account_key = crate::commands::account_identity::load_account_id_key(state, &user_id)
        .await
        .map_err(|e| anyhow::anyhow!("overlay: account identity unavailable ({e})"))?;
    let account_id_pub = account_key.verifying_key().to_bytes();

    // Device signing key (local DB / openmls storage) — the key the cert chain
    // certifies and `ds_client` signs with. Scoped so the !Send provider drops
    // before any await.
    let (device_signing, device_pub) = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("overlay: not signed in (local DB closed)"))?;
        let provider = crate::commands::mls::PollisProvider::new(db.conn());
        crate::commands::mls::load_device_signing_key(&provider, &user_id, &device_id)
            .map_err(|e| anyhow::anyhow!("overlay: device signing key unavailable ({e})"))?
    };
    let device_pub: [u8; 32] = device_pub
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("overlay: device signing pub is not 32 bytes"))?;

    let issued_at = now_unix_secs();
    let cert = DeviceCertMaterial::mint(
        &account_key,
        &device_id,
        &device_pub,
        OVERLAY_CERT_IDENTITY_VERSION,
        issued_at,
    );
    debug_assert_eq!(cert.account_id_pub, account_id_pub);

    Ok(ClientIdentity::new(user_id, device_id, device_signing, cert))
}

/// The user this device signs as, mirroring `ds_client::current_user_id`: prefer
/// the unlocked session, fall back to the accounts index before unlock.
async fn overlay_signing_user(state: &Arc<AppState>) -> anyhow::Result<String> {
    if let Some(u) = state.unlock.lock().await.as_ref() {
        if !u.user_id.is_empty() {
            return Ok(u.user_id.clone());
        }
    }
    let index = crate::accounts::read_accounts_index()
        .map_err(|e| anyhow::anyhow!("overlay: read accounts index: {e}"))?;
    index
        .last_active_user
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("overlay: no active user to sign relay handshake"))
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Load the configured relay endpoint(s) + the pinned QUIC leaf. Empty when the
/// endpoint or the pinned cert is absent/unreadable — the fail-closed state:
/// `RealRelayFactory::connect` then errors, so `Prefer` dials direct and `Strict`
/// degrades. A cert that can't be loaded is treated the same as none (never dial
/// an unverified relay).
fn load_relay_endpoints(config: &Config) -> Vec<RelayEndpoint> {
    let addrs = config.overlay_relay_endpoints();
    if addrs.is_empty() {
        return Vec::new();
    }
    let cert = match config.overlay_relay_cert.as_deref().and_then(load_pinned_cert) {
        Some(c) => c,
        None => {
            eprintln!("[overlay] relay endpoint set but no valid pinned cert — staying fail-closed");
            return Vec::new();
        }
    };
    addrs
        .into_iter()
        .map(|addr| RelayEndpoint { addr, cert: cert.clone() })
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

/// Start the overlay shim for `state` under `mode` (must be non-off). Builds the
/// routing policy + the real circuit factory and binds the loopback SOCKS5 shim.
/// The returned handle owns the shim task (aborted on drop) and lets the caller
/// flip Prefer↔Strict live. The factory loads the device identity lazily, so this
/// succeeds even before login — the shim runs (so `Strict` degrades) while
/// circuits fail-closed until a signing device is available.
pub(crate) async fn start_overlay_shim(
    state: &Arc<AppState>,
    mode: OverlayMode,
) -> Result<OverlayHandle> {
    let policy = overlay_policy(&state.config, mode);
    let endpoints = load_relay_endpoints(&state.config);
    let factory: Arc<dyn CircuitFactory> = Arc::new(RealRelayFactory::new(
        Arc::downgrade(state),
        endpoints,
        RELAY_DEAD_COOLDOWN,
        RELAY_DIAL_TIMEOUT,
    ));
    let handle = OverlayShim::start(policy, factory)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("overlay shim start: {e}")))?;
    let relay = state
        .config
        .overlay_relay_url
        .as_deref()
        .unwrap_or("<none configured>");
    eprintln!(
        "[overlay] shim on {} (mode={mode:?}, relay={relay})",
        handle.socks_addr()
    );
    Ok(handle)
}

// ── The libsql SOCKS connector (design §14.1) ──────────────────────────────

/// Build the libsql connector that routes Turso's Hrana/TLS through the overlay
/// shim. It mirrors libsql's own default remote connector (native roots verify
/// the REAL service cert via SNI from the request URI, http/1 ALPN, https-or-http)
/// but wraps a [`SocksConnector`] so the TCP lands on the loopback shim instead of
/// dialing Turso directly. The inner client TLS still terminates at the real host,
/// so the relay only ever forwards opaque bytes (§8).
pub(crate) fn overlay_connector(
    shim: SocketAddr,
) -> Result<hyper_rustls::HttpsConnector<SocksConnector>> {
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .map_err(|e| Error::Other(anyhow::anyhow!("overlay connector native roots: {e}")))?
        .https_or_http()
        .enable_http1()
        .wrap_connector(SocksConnector::new(shim));
    Ok(connector)
}

/// A `tower::Service<Uri>` that dials a target through the loopback SOCKS5 shim.
/// This is the inner connector libsql (via [`overlay_connector`]) calls to obtain
/// a TCP-shaped byte pipe, over which it then runs its own TLS to the real host.
#[derive(Clone, Copy)]
pub(crate) struct SocksConnector {
    shim: SocketAddr,
}

impl SocksConnector {
    pub(crate) fn new(shim: SocketAddr) -> Self {
        SocksConnector { shim }
    }
}

impl Service<Uri> for SocksConnector {
    type Response = SocksStream;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = std::io::Result<SocksStream>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let shim = self.shim;
        Box::pin(async move {
            let host = uri
                .host()
                .ok_or_else(|| io_err("overlay connector: request URI has no host"))?
                .to_string();
            // Turso/Hrana is always TLS; default to 443 when the URI omits a port.
            let port = uri.port_u16().unwrap_or(443);
            let inner = socks5_connect(shim, &host, port).await?;
            Ok(SocksStream { inner })
        })
    }
}

/// A TCP stream to the shim, wrapped so it satisfies libsql's `Socket` bound
/// (`hyper::client::connect::Connection` + `AsyncRead`/`AsyncWrite`).
pub(crate) struct SocksStream {
    inner: TcpStream,
}

impl Connection for SocksStream {
    fn connected(&self) -> Connected {
        // A plain proxied TCP hop — no HTTP/2 negotiation to advertise.
        Connected::new()
    }
}

impl AsyncRead for SocksStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for SocksStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::other(msg.to_string())
}

/// Open a TCP connection to `host:port` via a SOCKS5 CONNECT to the loopback
/// `shim`, using proxy-side DNS (ATYP=DOMAIN) so the shim resolves + allowlists
/// the real hostname. Mirrors the client half of `pollis_relay::shim`'s server.
async fn socks5_connect(shim: SocketAddr, host: &str, port: u16) -> std::io::Result<TcpStream> {
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

    use ed25519_dalek::SigningKey;
    use pollis_relay::circuit::{Hop, SingleHopFactory};
    use pollis_relay::client::ClientIdentity;
    use pollis_relay::proto::DeviceCertMaterial;
    use pollis_relay::server::{Allowlist as RelayAllowlist, RelayConfig, RelayServer, RelayStats};
    use rustls::pki_types::{CertificateDer, ServerName};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use zeroize::Zeroizing;

    use crate::commands::pin::UnlockState;
    use crate::db::remote::RemoteDb;

    const USER: &str = "u_overlay_test";
    const DEVICE: &str = "d_overlay_test";
    const ORIGIN_NAME: &str = "origin.test";
    const ISSUED_AT: u64 = 1_700_000_000;

    /// The device signing key (signs the relay handshake).
    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[11u8; 32])
    }

    /// The account identity key that certifies the device into a cert chain.
    fn account_key() -> SigningKey {
        SigningKey::from_bytes(&[12u8; 32])
    }

    /// A client identity carrying the device key + a valid offline cert chain —
    /// exactly what the client-side presents in production (built from the
    /// device's stored cert material).
    fn identity() -> Arc<ClientIdentity> {
        let device = signing_key();
        let cert = DeviceCertMaterial::mint(
            &account_key(),
            DEVICE,
            &device.verifying_key().to_bytes(),
            1,
            ISSUED_AT,
        );
        Arc::new(ClientIdentity::new(USER, DEVICE, device, cert))
    }

    fn cfg(mode: OverlayMode, relay: Option<&str>) -> Config {
        Config {
            turso_url: "libsql://turso.example.com".into(),
            turso_token: "t".into(),
            log_db_url: None,
            log_db_token: None,
            r2_endpoint: "https://r2.example.com".into(),
            r2_public_url: "https://cdn.example.com".into(),
            livekit_url: "wss://livekit.example.com".into(),
            pollis_delivery_url: Some("https://api.example.com".into()),
            overlay_mode: mode,
            overlay_relay_url: relay.map(|s| s.to_string()),
            overlay_relay_cert: None,
        }
    }

    /// A base64 (STANDARD) DER encoding of a relay's pinned cert, for
    /// `POLLIS_OVERLAY_RELAY_CERT`.
    fn cert_b64(cert: &CertificateDer<'static>) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(cert.as_ref())
    }

    /// A never-connecting factory: the fail-closed shape the shim sees when a
    /// device can't authenticate to a relay (no identity) or none is configured.
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
            RelayAllowlist::from_patterns(allow.iter().copied()),
        )
        .unwrap();
        for (host, ip) in overrides {
            config.resolve_overrides.insert((*host).to_string(), *ip);
        }
        let cert = config.server_cert();
        let stats = config.stats.clone();
        let (task, addr) = RelayServer::spawn(config).unwrap();
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

    /// A loopback TLS "origin" for `origin.test`, returning its addr + the CA to
    /// trust + a counter of accepted connections.
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

    // ── (a) POLLIS_OVERLAY parsing ─────────────────────────────────────────

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
        assert_eq!(host_of("libsql://turso.example.com").as_deref(), Some("turso.example.com"));
        assert_eq!(host_of("https://api.example.com:8080/v1").as_deref(), Some("api.example.com"));
        assert_eq!(host_of("wss://LiveKit.Example.com").as_deref(), Some("livekit.example.com"));
        assert_eq!(host_of("bare-host:443").as_deref(), Some("bare-host"));
        assert_eq!(host_of(""), None);
    }

    #[test]
    fn policy_routes_control_plane_and_leaves_media_direct() {
        let policy = overlay_policy(&cfg(OverlayMode::Prefer, None), OverlayMode::Prefer);
        use pollis_relay::PlannedRoute;
        // Control-plane hosts route overlay (with direct fallback in Prefer).
        assert_eq!(
            policy.plan("turso.example.com"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        assert_eq!(
            policy.plan("api.example.com"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        assert_eq!(
            policy.plan("r2.example.com"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        // LiveKit stays direct even in Prefer/Strict.
        assert_eq!(policy.plan("livekit.example.com"), PlannedRoute::Direct);
        // A non-first-party host (e.g. Expo push) is direct.
        assert_eq!(policy.plan("exp.host"), PlannedRoute::Direct);
    }

    // ── (b) overlay-off ⇒ inert ────────────────────────────────────────────

    /// `Off` derives a policy that routes every host — control-plane included —
    /// direct, so no shim is ever consulted. (The apply state machine never even
    /// starts a shim for `Off`; that is covered in `commands::overlay` tests.)
    #[test]
    fn off_mode_policy_is_all_direct() {
        use pollis_relay::PlannedRoute;
        let policy = overlay_policy(&cfg(OverlayMode::Off, None), OverlayMode::Off);
        assert_eq!(policy.plan("turso.example.com"), PlannedRoute::Direct);
        assert_eq!(policy.plan("api.example.com"), PlannedRoute::Direct);
    }

    #[tokio::test]
    async fn off_mode_http_client_is_proxyless() {
        let addr = spawn_plain_http("direct-and-inert").await;
        // http_client(None) is a plain client — reaches a plain origin with no proxy.
        let client = http_client(None);
        let resp = client
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("direct request with the overlay off");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "direct-and-inert");
    }

    /// Off is behaviorally distinct from on: with a Strict shim whose relay is
    /// unavailable, a proxied client FAILS for a control-plane host, while the
    /// proxy-less `http_client(None)` reaches the same host directly.
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

        // Proxied + Strict + no relay ⇒ degraded, so the request fails.
        let proxied = http_client(Some(&shim));
        let via_overlay = proxied
            .get(format!("http://{host}:{}/", addr.port()))
            .send()
            .await;
        assert!(via_overlay.is_err(), "Strict with no relay must not silently go direct");

        // Proxy-less ⇒ reaches the origin directly.
        let direct = http_client(None)
            .get(format!("http://{host}:{}/", addr.port()))
            .send()
            .await
            .expect("proxy-less client reaches the origin directly");
        assert_eq!(direct.status(), 200);
        drop(shim);
    }

    // ── (c) end-to-end: reqwest AND the libsql connector route through the shim ─

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

    /// The libsql-shaped path: drive the exact `SocksConnector` that
    /// `overlay_connector` feeds libsql, then run client TLS over the stream it
    /// returns — the cert is verified for the REAL name `origin.test` through
    /// shim→relay, proving end-to-end TLS survives tunneling (design §14.1, T2).
    #[tokio::test]
    async fn libsql_socks_connector_carries_verified_tls() {
        let (origin_addr, ca, _c) = spawn_tls_origin("libsql-connector-shape").await;
        let relay = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([ORIGIN_NAME]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, relay.factory()).await.unwrap();

        // The inner connector: SOCKS-dial the real name through the shim.
        let mut connector = SocksConnector::new(shim.socks_addr());
        let uri: Uri = format!("https://{ORIGIN_NAME}:{}", origin_addr.port())
            .parse()
            .unwrap();
        let stream = Service::call(&mut connector, uri)
            .await
            .expect("SOCKS connect through shim");

        // Client TLS over the proxied stream, verifying `origin.test`.
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
        assert!(resp.contains("libsql-connector-shape"));
        assert_eq!(relay.stats.dials(), 1);

        // And the production connector builds (native roots) for this shim.
        assert!(overlay_connector(shim.socks_addr()).is_ok());
        drop(shim);
    }

    /// Strict + non-off mode with no usable circuit must surface a degraded error
    /// rather than silently dialing direct (messages-must-work). The shim runs
    /// (so the mode is honored) but every control-plane CONNECT fails.
    #[tokio::test]
    async fn strict_without_relay_degrades_not_silent_direct() {
        let addr = spawn_plain_http("must-not-be-reached").await;
        let host = addr.ip().to_string();
        // Route the echo host as control-plane so Strict applies, with a
        // fail-closed factory standing in for "no relay reachable".
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([host.as_str()]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, Arc::new(FailingFactory))
            .await
            .expect("Strict starts the shim even with no relay reachable");

        let mut connector = SocksConnector::new(shim.socks_addr());
        let uri: Uri = format!("https://{host}:{}", addr.port()).parse().unwrap();
        let result = Service::call(&mut connector, uri).await;
        assert!(result.is_err(), "Strict + no relay must degrade, never silent-direct");
        drop(shim);
    }

    // ── (d) LIVE application: set_overlay_mode actually routes traffic ──────────

    /// Build an `AppState` with device cert material provisioned (unlocked
    /// session + account identity key + open local DB + device id), so the
    /// `RealRelayFactory` can load a `ClientIdentity` and authenticate to a relay.
    /// `remote_db` is a (lazy) remote handle so `set_overlay_shim` exercises the
    /// real rebuild path; it is never queried here.
    async fn provisioned_state(config: Config) -> Arc<AppState> {
        let remote = Arc::new(RemoteDb::connect(&config.turso_url, "tok").await.unwrap());
        let keystore: Arc<dyn crate::keystore::Keystore> =
            Arc::new(crate::keystore::InMemoryKeystore::new());
        // log_db == remote_db (unconfigured commit-log DB), like production.
        let state = Arc::new(AppState::new_with_parts(
            config,
            Arc::clone(&remote),
            remote,
            keystore,
        ));
        *state.unlock.lock().await = Some(UnlockState {
            user_id: USER.to_string(),
            db_key: Zeroizing::new(vec![7u8; 32]),
            account_id_key: Zeroizing::new(account_key().to_bytes().to_vec()),
        });
        *state.device_id.lock().await = Some(DEVICE.to_string());
        *state.local_db.lock().await = Some(crate::db::local::LocalDb::open_in_memory().unwrap());
        state
    }

    /// Drive the exact libsql-shaped `SocksConnector` through `shim` to the TLS
    /// origin and verify the inner TLS terminates at the REAL name `origin.test`.
    async fn tls_probe_through_shim(shim: SocketAddr, port: u16, ca: &CertificateDer<'static>) {
        let mut connector = SocksConnector::new(shim);
        let uri: Uri = format!("https://{ORIGIN_NAME}:{port}").parse().unwrap();
        let stream = Service::call(&mut connector, uri)
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
            "libsql-shaped probe did not reach origin through the relay"
        );
    }

    /// THE live-application proof: flipping `set_overlay_mode` genuinely routes a
    /// reqwest control-plane call AND a libsql-shaped connection through an
    /// in-process relay (cert verified for the real name), Prefer↔Strict flips the
    /// live policy with no shim restart / DB reconnect, and Off restores the
    /// byte-for-byte direct path (relay sees no further dials).
    #[tokio::test]
    async fn set_overlay_mode_routes_live_through_relay() {
        let (origin_addr, ca, origin_conns) = spawn_tls_origin("live-apply").await;
        let relay = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);

        let mut config = cfg(OverlayMode::Off, Some(&relay.addr.to_string()));
        // origin.test is the control-plane (Turso) host, so it routes overlay.
        config.turso_url = format!("libsql://{ORIGIN_NAME}");
        config.overlay_relay_cert = Some(cert_b64(&relay.cert));
        let state = provisioned_state(config).await;

        use crate::commands::overlay::{apply_overlay_mode, get_overlay_mode};

        // Off → Prefer: shim up, both remote DBs repointed through it.
        apply_overlay_mode(&state, OverlayMode::Prefer).await.unwrap();
        assert_eq!(get_overlay_mode(&state).await.unwrap(), "prefer");
        let handle = state.overlay_handle().expect("shim running after Prefer");
        assert_eq!(handle.mode(), OverlayMode::Prefer);
        let shim_addr = handle.socks_addr();
        assert_eq!(
            state.remote_db.overlay_shim(),
            Some(shim_addr),
            "remote_db must be routed through the shim after Prefer"
        );

        // (1) A reqwest control-plane call routes THROUGH THE RELAY.
        let client = pollis_relay::http::http_client_builder(Some(handle.as_ref()))
            .add_root_certificate(reqwest::Certificate::from_der(&ca).unwrap())
            .build()
            .unwrap();
        let url = format!("https://{ORIGIN_NAME}:{}/", origin_addr.port());
        let resp = client.get(&url).send().await.expect("reqwest routes through relay");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "live-apply");
        assert_eq!(relay.stats.dials(), 1, "relay must have dialed the origin (reqwest)");

        // (2) A libsql-shaped connection routes through the relay too, cert
        //     verified for the REAL name origin.test.
        tls_probe_through_shim(shim_addr, origin_addr.port(), &ca).await;
        assert_eq!(relay.stats.dials(), 2, "relay must have dialed the origin (libsql shape)");
        assert!(origin_conns.load(Ordering::Relaxed) >= 2);

        // Prefer → Strict: live policy flip — SAME shim, no DB reconnect.
        apply_overlay_mode(&state, OverlayMode::Strict).await.unwrap();
        assert_eq!(get_overlay_mode(&state).await.unwrap(), "strict");
        let handle2 = state.overlay_handle().expect("shim still running after Strict");
        assert_eq!(handle2.mode(), OverlayMode::Strict);
        assert_eq!(handle2.socks_addr(), shim_addr, "Prefer↔Strict must not restart the shim");
        assert_eq!(
            state.remote_db.overlay_shim(),
            Some(shim_addr),
            "Prefer↔Strict must not reconnect the DBs"
        );

        // Strict → Off: shim dropped, DBs back to direct, relay sees no new dials.
        apply_overlay_mode(&state, OverlayMode::Off).await.unwrap();
        assert_eq!(get_overlay_mode(&state).await.unwrap(), "off");
        assert!(state.overlay_handle().is_none(), "shim must stop after Off");
        assert_eq!(state.remote_db.overlay_shim(), None, "remote_db must be direct after Off");

        // A direct call now bypasses the relay entirely.
        let plain = spawn_plain_http("direct-after-off").await;
        let direct = http_client(None)
            .get(format!("http://{plain}/"))
            .send()
            .await
            .expect("direct request after Off");
        assert_eq!(direct.status(), 200);
        assert_eq!(relay.stats.dials(), 2, "Off routes direct — relay sees no new dials");
    }

    /// Strict with the relay DOWN, applied LIVE, must surface a degraded error —
    /// never a silent direct send.
    #[tokio::test]
    async fn set_overlay_strict_relay_down_degrades_live() {
        // A pinned cert we can load, but an endpoint that points nowhere.
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        let mut config = cfg(OverlayMode::Off, Some("127.0.0.1:1"));
        config.turso_url = format!("libsql://{ORIGIN_NAME}");
        config.overlay_relay_cert = Some(cert_b64(&cert));
        let state = provisioned_state(config).await;

        crate::commands::overlay::apply_overlay_mode(&state, OverlayMode::Strict)
            .await
            .unwrap();
        let handle = state.overlay_handle().unwrap();

        let mut connector = SocksConnector::new(handle.socks_addr());
        let uri: Uri = format!("https://{ORIGIN_NAME}:443").parse().unwrap();
        assert!(
            Service::call(&mut connector, uri).await.is_err(),
            "Strict + relay down must degrade, never silent-direct"
        );
    }

    /// Prefer with the relay DOWN, applied LIVE, falls back to a direct dial of
    /// the (directly reachable) control-plane host.
    #[tokio::test]
    async fn set_overlay_prefer_relay_down_falls_back_direct_live() {
        let plain = spawn_plain_http("prefer-fallback").await;
        let host = plain.ip().to_string();
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        let mut config = cfg(OverlayMode::Off, Some("127.0.0.1:1"));
        // Control-plane host = the directly-connectable plain origin.
        config.turso_url = format!("libsql://{host}");
        config.overlay_relay_cert = Some(cert_b64(&cert));
        let state = provisioned_state(config).await;

        crate::commands::overlay::apply_overlay_mode(&state, OverlayMode::Prefer)
            .await
            .unwrap();
        let handle = state.overlay_handle().unwrap();

        let mut connector = SocksConnector::new(handle.socks_addr());
        let uri: Uri = format!("http://{host}:{}", plain.port()).parse().unwrap();
        assert!(
            Service::call(&mut connector, uri).await.is_ok(),
            "Prefer + relay down must fall back to a direct dial"
        );
    }

    // ── (e) POOL + health + failover (design §14.1, "messages must work") ───────

    const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// A relay that resolves `origin.test` to loopback and allows it as a target
    /// — an in-process pool member the client can dial an origin through.
    fn spawn_pool_relay() -> TestRelay {
        spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, LOCALHOST)])
    }

    /// A `RealRelayFactory` over `endpoints`, drawing its device identity from a
    /// provisioned `AppState` (kept alive by the caller via the returned `Arc`).
    fn pool_factory(
        state: &Arc<AppState>,
        endpoints: Vec<RelayEndpoint>,
        cooldown: Duration,
    ) -> RealRelayFactory {
        // Short dial timeout so an unreachable endpoint fails over fast in tests.
        RealRelayFactory::new(
            Arc::downgrade(state),
            endpoints,
            cooldown,
            Duration::from_secs(2),
        )
    }

    fn endpoint(addr: String, cert: CertificateDer<'static>) -> RelayEndpoint {
        RelayEndpoint { addr, cert }
    }

    /// FAILOVER: [relayA(unreachable), relayB(up)] → a control-plane dial still
    /// succeeds THROUGH relayB, relayA is tried first and marked unhealthy.
    #[tokio::test]
    async fn pool_fails_over_to_healthy_relay() {
        let origin = spawn_plain_http("failover-target").await;
        let relay_b = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        // relayA is an unreachable address at index 0 (tried first on the first
        // connect: start offset is 0); relayB is the live pool member at index 1.
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

    /// ALL-DOWN → policy holds: every endpoint fails → `connect` errors (so Prefer
    /// falls back to direct / Strict degrades), and every endpoint is marked dead.
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

    /// ALL-DOWN, applied LIVE via a comma-separated multi-endpoint config: Prefer
    /// still falls back to a direct dial (the whole pool → policy path).
    #[tokio::test]
    async fn pool_multi_endpoint_prefer_falls_back_direct_live() {
        let plain = spawn_plain_http("pool-prefer-fallback").await;
        let host = plain.ip().to_string();
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        // Two dead relays, comma-separated — parsed into a two-member pool.
        let mut config = cfg(OverlayMode::Off, Some("127.0.0.1:1,127.0.0.1:2"));
        config.turso_url = format!("libsql://{host}");
        config.overlay_relay_cert = Some(cert_b64(&cert));
        let state = provisioned_state(config).await;

        crate::commands::overlay::apply_overlay_mode(&state, OverlayMode::Prefer)
            .await
            .unwrap();
        let handle = state.overlay_handle().unwrap();

        let mut connector = SocksConnector::new(handle.socks_addr());
        let uri: Uri = format!("http://{host}:{}", plain.port()).parse().unwrap();
        assert!(
            Service::call(&mut connector, uri).await.is_ok(),
            "Prefer + whole pool down must still fall back to a direct dial"
        );
    }

    /// HEALTH/COOLDOWN: an endpoint in its cooldown window is skipped (the healthy
    /// one is preferred); after the cooldown expires it is retried.
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

        // Both relays are UP, but pin endpoint 0 as dead within its cooldown.
        factory.health.lock().unwrap()[0] = Some(Instant::now() + cooldown);

        // The dead endpoint is skipped — endpoint 1 serves the connect.
        factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        assert_eq!(relay0.stats.dials(), 0, "dead endpoint 0 skipped while in cooldown");
        assert_eq!(relay1.stats.dials(), 1, "healthy endpoint 1 served the connect");

        // After the cooldown expires, endpoint 0 is eligible again; over a few
        // rotating connects it is retried and dials.
        tokio::time::sleep(cooldown + Duration::from_millis(50)).await;
        for _ in 0..4 {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert!(
            relay0.stats.dials() >= 1,
            "endpoint 0 is retried once its cooldown has expired"
        );
    }

    /// FAIL-OPEN: if ALL endpoints are marked dead, they are still TRIED — a
    /// transient outage that marked the whole pool dead must never wedge it.
    #[tokio::test]
    async fn pool_tries_all_dead_endpoints_fail_open() {
        let origin = spawn_plain_http("fail-open-target").await;
        let relay = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![endpoint(relay.addr.to_string(), relay.cert.clone())];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));

        // Mark the only endpoint dead for the full cooldown; fail-open must still
        // try it rather than refuse to dial.
        factory.health.lock().unwrap()[0] = Some(Instant::now() + Duration::from_secs(30));

        factory
            .connect(ORIGIN_NAME, origin.port())
            .await
            .expect("fail-open: a fully-dead pool is still tried");
        assert_eq!(relay.stats.dials(), 1, "the dead endpoint was tried anyway");
        // A successful dial clears its dead mark.
        assert!(factory.health.lock().unwrap()[0].is_none());
    }

    /// LOAD SPREAD: with two healthy relays, repeated connects do not all land on
    /// endpoint 0 — the rotating start index deals them out deterministically.
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
        // Rotation alternates the first-tried (and, both being up, dialed)
        // endpoint, so each takes exactly half — deterministic, never flaky.
        assert_eq!(relay0.stats.dials(), N / 2, "endpoint 0 took its share");
        assert_eq!(relay1.stats.dials(), N / 2, "endpoint 1 took its share");
        assert_eq!(relay0.stats.dials() + relay1.stats.dials(), N);
    }
}
