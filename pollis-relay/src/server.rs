//! The relay server. Per bi-stream: offline device-cert handshake (design §9.4),
//! per-account / per-IP rate limits (§11.5), an optional `Anchor`, then one
//! terminal frame:
//!
//! - **`Connect`**: last hop. Host:port must match the static allowlist; the
//!   relay TCP-dials and pipes opaque bytes (never terminates inner TLS).
//! - **`Extend`** (v4+): middle hop. The next hop must be in the signed
//!   directory ([`crate::nexthop`]) and admitted by revocation (fail-closed),
//!   is dialled pinned to the frame's fingerprint, re-checked on the identity it
//!   presented, then spliced after a `Layer` announcement ([`crate::onion`]).
//! - **`Park`** (v4+): a NATed peer-hosted relay offers its own connection as a
//!   middle hop; a matching `Extend` is spliced into it ([`crate::park`]).
//!
//! A stream opening with `Layer` is another relay extending into us. That frame
//! is unauthenticated, so the layered stream is still per-IP limited on the
//! carrying connection's address; exempting it would bypass every per-IP bound.
//!
//! The allowlist is enforced wherever `Connect` lands (always the last hop), so
//! the closed-overlay guarantee (§1.2) holds for any path length.
//!
//! Pre-handshake surface: QUIC Retry address validation, a per-IP connection cap
//! at accept, a global cap on completed handshakes (half-open ones bounded
//! separately), [`RelayConfig::first_frame_timeout`], and a small per-connection
//! stream limit.

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use tokio::net::TcpStream;
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::anchor::AnchorPolicy;
use crate::nexthop::NextHopDirectory;
use crate::onion;
use crate::park::{ParkedPeer, ParkedPeers, Tunnel};
use crate::policy::{RelayIdentity, RevocationStore};
use crate::proto::{self, ClientFrame, Command, Connect, Extend, Handshake, Park, RejectReason};
use crate::ratelimit::{ConnectionLimiter, IpKey, RateLimitConfig, RateLimiter};
use crate::stream::{DuplexStream, RelayStream};
use crate::tls::{self, FingerprintPinnedVerifier, SelfSignedIdentity};

/// The port half of an allowlist entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortPattern {
    /// Exactly this port. An entry with no `:port` means [`DEFAULT_ALLOWED_PORT`].
    Exact(u16),
    /// Any port (`host:*`).
    Any,
}

/// The port an allowlist entry means when it names none: HTTPS, which is the
/// only port any first-party destination speaks. Anything else is written out.
pub const DEFAULT_ALLOWED_PORT: u16 = 443;

/// The host half of an allowlist entry.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HostMatcher {
    /// Exact host match (case-insensitive).
    Exact(String),
    /// Suffix match for a `*.example.com` glob — stored as `.example.com`.
    Suffix(String),
    /// Matches any host. Use only for a fully open relay (not first-party v0).
    Any,
    /// What a malformed entry parses to, so a typo can only close a destination.
    Nothing,
}

impl HostMatcher {
    fn parse(s: &str) -> HostMatcher {
        if s == "*" {
            HostMatcher::Any
        } else if let Some(rest) = s.strip_prefix("*.") {
            HostMatcher::Suffix(format!(".{}", rest.to_ascii_lowercase()))
        } else {
            HostMatcher::Exact(s.to_ascii_lowercase())
        }
    }

    fn matches(&self, host: &str) -> bool {
        match self {
            HostMatcher::Any => true,
            HostMatcher::Nothing => false,
            HostMatcher::Exact(h) => h.eq_ignore_ascii_case(host),
            HostMatcher::Suffix(suffix) => {
                let (host, suffix) = (host.as_bytes(), suffix.as_bytes());
                host.len() >= suffix.len()
                    && host[host.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
            }
        }
    }
}

/// A destination matcher: host pattern and port pattern. A `Connect` must match
/// both, so an allowlisted host is one service, not every port on the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPattern {
    host: HostMatcher,
    port: PortPattern,
}

impl HostPattern {
    /// Parse one allowlist entry — `host[:port]`, where the host is exact, a
    /// `*.suffix` glob, or `*`, and the port is a number, `*`, or omitted (⇒
    /// [`DEFAULT_ALLOWED_PORT`]).
    ///
    /// A malformed entry is logged and matches nothing (fail closed); the bin
    /// uses [`HostPattern::try_parse`] so a typo aborts startup instead.
    pub fn parse(s: &str) -> HostPattern {
        match HostPattern::try_parse(s) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("relay: ignoring malformed allowlist entry {s:?}: {e}");
                HostPattern {
                    host: HostMatcher::Nothing,
                    port: PortPattern::Any,
                }
            }
        }
    }

    /// [`HostPattern::parse`] that reports a malformed entry.
    pub fn try_parse(s: &str) -> anyhow::Result<HostPattern> {
        let s = s.trim();
        if s.is_empty() {
            anyhow::bail!("empty allowlist entry");
        }
        // No IPv6 literals, so the last colon can only be the port separator.
        let (host, port) = match s.rsplit_once(':') {
            Some((host, port)) => {
                if host.contains(':') {
                    anyhow::bail!(
                        "more than one ':' in {s:?} (IPv6 literals are not allowlist entries)"
                    );
                }
                let port = if port == "*" {
                    PortPattern::Any
                } else {
                    let n: u16 = port.parse().map_err(|_| {
                        anyhow::anyhow!("port {port:?} in {s:?} is not a number 1-65535 or '*'")
                    })?;
                    if n == 0 {
                        anyhow::bail!("port 0 in {s:?}");
                    }
                    PortPattern::Exact(n)
                };
                (host, port)
            }
            None => (s, PortPattern::Exact(DEFAULT_ALLOWED_PORT)),
        };
        if host.is_empty() {
            anyhow::bail!("empty host in {s:?}");
        }
        Ok(HostPattern {
            host: HostMatcher::parse(host),
            port,
        })
    }

    pub(crate) fn matches(&self, host: &str, port: u16) -> bool {
        let port_ok = match self.port {
            PortPattern::Any => true,
            PortPattern::Exact(p) => p == port,
        };
        port_ok && self.host.matches(host)
    }

    /// The routing question (which plane is this host on?), ignoring the port;
    /// egress is decided by [`HostPattern::matches`] at the far end.
    pub(crate) fn matches_host(&self, host: &str) -> bool {
        self.host.matches(host)
    }
}

/// The relay's static destination allowlist.
#[derive(Debug, Clone, Default)]
pub struct Allowlist(Vec<HostPattern>);

impl Allowlist {
    /// Build from raw patterns (`*.turso.io`, `api.pollis.com:443`, `*:*`). A
    /// malformed entry matches nothing; see [`Allowlist::try_from_patterns`].
    pub fn from_patterns<I, S>(patterns: I) -> Allowlist
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Allowlist(patterns.into_iter().map(|p| HostPattern::parse(p.as_ref())).collect())
    }

    /// [`Allowlist::from_patterns`] that refuses a malformed entry.
    pub fn try_from_patterns<I, S>(patterns: I) -> anyhow::Result<Allowlist>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        patterns
            .into_iter()
            .map(|p| HostPattern::try_parse(p.as_ref()))
            .collect::<anyhow::Result<Vec<_>>>()
            .map(Allowlist)
    }

    /// An empty allowlist permits nothing.
    pub fn permits(&self, host: &str, port: u16) -> bool {
        self.0.iter().any(|p| p.matches(host, port))
    }

    pub fn into_patterns(self) -> Vec<HostPattern> {
        self.0
    }
}

/// Aggregate relay counters, for tests and metrics. None identify a client.
#[derive(Debug, Default)]
pub struct RelayStats {
    /// Streams whose handshake verified.
    pub authorized: AtomicU64,
    /// Streams rejected before a dial (auth, allowlist, or rate limit).
    pub rejected: AtomicU64,
    /// Streams rejected specifically for tripping a rate/concurrency limit.
    pub rate_limited: AtomicU64,
    /// Targets the relay actually TCP-dialed.
    pub dials: AtomicU64,
    /// Times this node was a last hop (and so learned a destination).
    pub connects: AtomicU64,
    /// Times this node was a middle hop.
    pub extends: AtomicU64,
    /// Extends spliced into a parked peer rather than dialled.
    pub splices: AtomicU64,
    /// Streams that arrived from another relay via an onion layer.
    pub layers: AtomicU64,
    /// Gauge: circuits open right now (see [`LiveCircuitGuard`]).
    pub live_circuits: AtomicI64,
    pub anchors_verified: AtomicU64,
    /// Anchors presented and refused; separate from `rejected` so failing to
    /// anchor is distinguishable from failing to authenticate.
    pub anchors_refused: AtomicU64,
}

impl RelayStats {
    pub fn authorized(&self) -> u64 {
        self.authorized.load(Ordering::Relaxed)
    }
    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }
    pub fn rate_limited(&self) -> u64 {
        self.rate_limited.load(Ordering::Relaxed)
    }
    pub fn dials(&self) -> u64 {
        self.dials.load(Ordering::Relaxed)
    }
    pub fn connects(&self) -> u64 {
        self.connects.load(Ordering::Relaxed)
    }
    pub fn extends(&self) -> u64 {
        self.extends.load(Ordering::Relaxed)
    }
    pub fn splices(&self) -> u64 {
        self.splices.load(Ordering::Relaxed)
    }
    pub fn layers(&self) -> u64 {
        self.layers.load(Ordering::Relaxed)
    }
    pub fn live_circuits(&self) -> i64 {
        self.live_circuits.load(Ordering::Relaxed)
    }
    pub fn anchors_verified(&self) -> u64 {
        self.anchors_verified.load(Ordering::Relaxed)
    }
    pub fn anchors_refused(&self) -> u64 {
        self.anchors_refused.load(Ordering::Relaxed)
    }
}

/// Test-only hook called with each accepted connection's peer address, so tests
/// can assert a later hop never sees the client's address. Always `None` in
/// production: the relay records nothing about who connects (§5.2, §11.7).
pub type PeerObserver = Arc<dyn Fn(SocketAddr) + Send + Sync>;

/// Configuration for a [`RelayServer`].
pub struct RelayConfig {
    pub bind: SocketAddr,
    /// Static destination allowlist (design §1.2).
    pub allowlist: Allowlist,
    /// The relay's own self-signed QUIC identity, which clients pin.
    pub identity: SelfSignedIdentity,
    /// Host → IP overrides applied before dialing (tests; also a pinning hook).
    pub resolve_overrides: HashMap<String, IpAddr>,
    pub rate_limits: RateLimitConfig,
    /// Cap on connections whose handshake completed. In-flight handshakes are
    /// bounded separately by the same number, so half-open floods can't evict
    /// live ones.
    pub max_concurrent_connections: u32,
    /// Per-source-IP connection cap, enforced at accept after QUIC address
    /// validation. One circuit per connection, so this also bounds circuits.
    pub max_connections_per_ip: u32,
    /// Deadline for a new stream to finish negotiating, so a peer can't hold
    /// tasks and circuit slots by going quiet.
    pub first_frame_timeout: Duration,
    /// Whether this node honours `Extend` as a middle hop. Default `true`.
    pub allow_extend: bool,
    /// Which addresses this node will open a next leg to ([`crate::nexthop`]).
    /// The default, [`NextHopDirectory::unconfigured`], permits nothing.
    pub next_hops: NextHopDirectory,
    /// Allow next hops on private/loopback addresses. Test rigs and lab pools
    /// only: it stops a mis-signed directory turning nodes into intranet dialers.
    pub allow_private_next_hops: bool,
    /// Deadline for opening the next leg (dial or splice, QUIC handshake,
    /// `Layer` ack), so a blackholing address can't hold resources.
    pub extend_dial_timeout: Duration,
    /// Live relay revocation, checked before handing a circuit to a next hop.
    /// The default, [`RevocationStore::unconfigured`], admits nothing, so a node
    /// without the directory key can't be a middle hop. Deliberately strict.
    pub revocations: RevocationStore,
    /// Account anchoring; defaults to [`AnchorPolicy::Ignore`].
    pub anchor: AnchorPolicy,
    /// Peers parked here; shared so the health endpoint can publish their
    /// fingerprints for the directory reconciler.
    pub parked: Arc<ParkedPeers>,
    pub stats: Arc<RelayStats>,
    /// `None` in production; see [`PeerObserver`].
    pub peer_observer: Option<PeerObserver>,
}

impl RelayConfig {
    /// Build a config with a fresh self-signed identity. Deployable nodes use
    /// [`tls::load_or_generate_identity`] + [`RelayConfig::with_identity`].
    pub fn new(bind: SocketAddr, allowlist: Allowlist) -> anyhow::Result<Self> {
        let identity = tls::generate_self_signed(tls::RELAY_SERVER_NAME)?;
        Ok(Self::with_identity(bind, allowlist, identity))
    }

    pub fn with_identity(bind: SocketAddr, allowlist: Allowlist, identity: SelfSignedIdentity) -> Self {
        RelayConfig {
            bind,
            allowlist,
            identity,
            resolve_overrides: HashMap::new(),
            rate_limits: RateLimitConfig::default(),
            max_concurrent_connections: crate::config::DEFAULT_MAX_CONCURRENT_CONNECTIONS,
            max_connections_per_ip: crate::config::DEFAULT_MAX_CONNECTIONS_PER_IP,
            first_frame_timeout: Duration::from_secs(crate::config::DEFAULT_FIRST_FRAME_TIMEOUT_SECS),
            allow_extend: true,
            next_hops: NextHopDirectory::unconfigured(),
            allow_private_next_hops: false,
            extend_dial_timeout: Duration::from_secs(
                crate::config::DEFAULT_EXTEND_DIAL_TIMEOUT_SECS,
            ),
            revocations: RevocationStore::unconfigured(),
            anchor: AnchorPolicy::Ignore,
            parked: ParkedPeers::new(),
            stats: Arc::new(RelayStats::default()),
            peer_observer: None,
        }
    }

    pub fn server_cert(&self) -> rustls::pki_types::CertificateDer<'static> {
        self.identity.cert_der.clone()
    }
}

struct RelayInner {
    allowlist: Allowlist,
    resolve_overrides: HashMap<String, IpAddr>,
    rate_limiter: Arc<RateLimiter>,
    first_frame_timeout: Duration,
    stats: Arc<RelayStats>,
    allow_extend: bool,
    next_hops: NextHopDirectory,
    allow_private_next_hops: bool,
    extend_dial_timeout: Duration,
    /// Our own leaf, so a hop to ourselves (a loop) is recognisable.
    own_leaf_der: Vec<u8>,
    revocations: RevocationStore,
    anchor: AnchorPolicy,
    parked: Arc<ParkedPeers>,
    peer_observer: Option<PeerObserver>,
    layer_acceptor: TlsAcceptor,
    /// One lazily-created outbound endpoint per address family, so a busy middle
    /// hop doesn't burn a UDP socket per circuit.
    extend_endpoint_v4: OnceCell<quinn::Endpoint>,
    extend_endpoint_v6: OnceCell<quinn::Endpoint>,
}

impl RelayInner {
    async fn extend_endpoint(&self, ipv6: bool) -> anyhow::Result<&quinn::Endpoint> {
        let cell = if ipv6 {
            &self.extend_endpoint_v6
        } else {
            &self.extend_endpoint_v4
        };
        cell.get_or_try_init(|| async {
            let bind: SocketAddr = if ipv6 {
                "[::]:0".parse().unwrap()
            } else {
                "0.0.0.0:0".parse().unwrap()
            };
            quinn::Endpoint::client(bind).map_err(anyhow::Error::from)
        })
        .await
    }
}

pub struct RelayServer;

impl RelayServer {
    /// Spawn a relay on `config.bind`; returns the accept task and the bound
    /// address. Runs until aborted; see [`RelayServer::spawn_with_shutdown`].
    pub fn spawn(config: RelayConfig) -> anyhow::Result<(JoinHandle<()>, SocketAddr)> {
        Self::spawn_with_shutdown(config, std::future::pending::<()>(), Duration::from_secs(0))
    }

    /// Spawn a relay that, when `shutdown` resolves, stops accepting, drains
    /// in-flight pipes for up to `drain_timeout`, then closes the endpoint.
    pub fn spawn_with_shutdown<F>(
        config: RelayConfig,
        shutdown: F,
        drain_timeout: Duration,
    ) -> anyhow::Result<(JoinHandle<()>, SocketAddr)>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        tls::ensure_crypto_provider();

        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![config.identity.cert_der.clone()], config.identity.key_der.clone_key())?;
        server_crypto.alpn_protocols = proto::SUPPORTED_ALPNS.iter().map(|a| a.to_vec()).collect();

        let quic_crypto = QuicServerConfig::try_from(server_crypto)?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
        let mut transport = quinn::TransportConfig::default();
        // quinn's default of 100 would let one connection hold 100 negotiating
        // tasks; legitimate peers use one stream.
        transport.max_concurrent_bidi_streams(quinn::VarInt::from_u32(
            MAX_BIDI_STREAMS_PER_CONNECTION,
        ));
        transport.max_concurrent_uni_streams(quinn::VarInt::from_u32(0));
        server_config.transport_config(Arc::new(transport));

        let layer_acceptor = onion::layer_acceptor(&config.identity)?;

        let endpoint = quinn::Endpoint::server(server_config, config.bind)?;
        let local_addr = endpoint.local_addr()?;

        let inner = Arc::new(RelayInner {
            allowlist: config.allowlist,
            resolve_overrides: config.resolve_overrides,
            rate_limiter: RateLimiter::new(config.rate_limits),
            first_frame_timeout: config.first_frame_timeout,
            stats: config.stats,
            allow_extend: config.allow_extend,
            next_hops: config.next_hops,
            allow_private_next_hops: config.allow_private_next_hops,
            extend_dial_timeout: config.extend_dial_timeout,
            own_leaf_der: config.identity.cert_der.as_ref().to_vec(),
            revocations: config.revocations,
            anchor: config.anchor,
            parked: config.parked,
            peer_observer: config.peer_observer,
            layer_acceptor,
            extend_endpoint_v4: OnceCell::new(),
            extend_endpoint_v6: OnceCell::new(),
        });
        let max_conns = config.max_concurrent_connections.max(1) as u64;
        // Established and in-flight handshakes are counted apart, so a flood of
        // never-completing Initials fills its own bucket, not the established one.
        let live_conns = Arc::new(AtomicU64::new(0));
        let handshaking = Arc::new(AtomicU64::new(0));
        let conn_limiter = ConnectionLimiter::new(config.max_connections_per_ip);

        let handle = tokio::spawn(async move {
            tokio::pin!(shutdown);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown => {
                        break;
                    }
                    incoming = endpoint.accept() => {
                        let Some(incoming) = incoming else {
                            break;
                        };
                        // Address validation first (RFC 9000 §8.1): a stateless
                        // Retry makes a spoofed source cost nothing, and every
                        // cap below keys on a proven address.
                        if !incoming.remote_address_validated() {
                            if let Err(e) = incoming.retry() {
                                e.into_incoming().refuse();
                            }
                            continue;
                        }
                        // The limiter only holds the per-boot pseudonym.
                        let ip = IpKey::of(incoming.remote_address().ip());
                        if handshaking.load(Ordering::Relaxed) >= max_conns
                            || live_conns.load(Ordering::Relaxed) >= max_conns
                        {
                            incoming.refuse();
                            continue;
                        }
                        // Held for the connection's whole life, freed on drop.
                        let Some(slot) = conn_limiter.acquire(ip) else {
                            incoming.refuse();
                            continue;
                        };
                        handshaking.fetch_add(1, Ordering::Relaxed);
                        let inner = inner.clone();
                        let live = live_conns.clone();
                        let pending = handshaking.clone();
                        tokio::spawn(async move {
                            let _slot = slot;
                            let connection = match incoming.await {
                                Ok(connection) => connection,
                                Err(e) => {
                                    pending.fetch_sub(1, Ordering::Relaxed);
                                    tracing::debug!("relay: connection setup failed: {e}");
                                    return;
                                }
                            };
                            pending.fetch_sub(1, Ordering::Relaxed);
                            // Re-check: the pre-check raced other handshakes.
                            if live.fetch_add(1, Ordering::Relaxed) >= max_conns {
                                live.fetch_sub(1, Ordering::Relaxed);
                                connection.close(0u32.into(), b"relay at capacity");
                                return;
                            }
                            handle_connection(connection, inner).await;
                            live.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                }
            }

            let _ = tokio::time::timeout(drain_timeout, endpoint.wait_idle()).await;
            endpoint.close(0u32.into(), b"relay shutting down");
        });

        Ok((handle, local_addr))
    }
}

/// Where a stream came from. `ip` is the carrying QUIC peer (client, or claimed
/// previous relay) and every admission keys on it, so no opening frame escapes
/// per-IP limits. `connection` is `None` for a layered stream, which makes
/// parking through a layer structurally impossible.
struct StreamOrigin {
    /// Per-boot pseudonym (`IpKey::of`), never the address.
    ip: IpKey,
    connection: Option<quinn::Connection>,
}

impl StreamOrigin {
    fn layered(&self) -> StreamOrigin {
        StreamOrigin {
            ip: self.ip,
            connection: None,
        }
    }
}

/// Legitimate peers use one bi-stream; the rest is slack for teardown overlap.
const MAX_BIDI_STREAMS_PER_CONNECTION: u32 = 4;

type Deadline = tokio::time::Instant;

/// Run one negotiation read under the stream's deadline; a timeout surfaces as
/// an I/O error, refused like any malformed exchange.
async fn until<T, F>(deadline: Deadline, read: F) -> Result<T, proto::ProtoError>
where
    F: Future<Output = Result<T, proto::ProtoError>>,
{
    match tokio::time::timeout_at(deadline, read).await {
        Ok(result) => result,
        Err(_) => Err(proto::ProtoError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "stream did not finish negotiating in time",
        ))),
    }
}

async fn handle_connection(connection: quinn::Connection, inner: Arc<RelayInner>) {
    let peer = connection.remote_address();
    if let Some(observer) = &inner.peer_observer {
        observer(peer);
    }
    let peer_key = IpKey::of(peer.ip());
    while let Ok((send, recv)) = connection.accept_bi().await {
        let inner = inner.clone();
        let origin = StreamOrigin {
            ip: peer_key,
            connection: Some(connection.clone()),
        };
        let deadline = Deadline::now() + inner.first_frame_timeout;
        tokio::spawn(async move {
            if let Err(e) = handle_quic_stream(send, recv, inner, origin, deadline).await {
                tracing::debug!("relay: stream ended: {e}");
            }
        });
    }
}

/// Serve one bi-stream straight off the QUIC endpoint: the only place an
/// (unauthenticated) `Layer` frame is legal.
async fn handle_quic_stream(
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    inner: Arc<RelayInner>,
    origin: StreamOrigin,
    deadline: Deadline,
) -> anyhow::Result<()> {
    let mut stream = RelayStream::new(send, recv, None, None);

    let Ok(header) = until(deadline, proto::read_frame_header(&mut stream)).await else {
        let version = proto::MIN_PROTOCOL_VERSION;
        reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
        return Ok(());
    };

    match header.msg_type {
        proto::MSG_LAYER if header.version >= proto::ONION_MIN_VERSION => {
            inner.stats.layers.fetch_add(1, Ordering::Relaxed);
            // Ack to the previous relay (never reaches the client), then peel.
            proto::write_response(&mut stream, Ok(()), header.version).await?;
            let accept = inner.layer_acceptor.accept(stream);
            let layered = match tokio::time::timeout_at(deadline, accept).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::debug!("relay: onion layer handshake failed: {e}");
                    // Counted only: the `Ok` above already went out.
                    inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
                Err(_) => {
                    tracing::debug!("relay: onion layer handshake timed out");
                    inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
            };
            serve_layered_stream(layered, inner, origin.layered(), deadline).await
        }
        proto::MSG_HANDSHAKE => {
            read_handshake_and_serve(stream, inner, origin, header.version, deadline).await
        }
        other => {
            reject(
                &mut stream,
                &inner.stats,
                RejectReason::BadRequest,
                header.version,
            )
            .await;
            tracing::debug!("relay: unexpected opening frame {other}");
            Ok(())
        }
    }
}

async fn read_handshake_and_serve<S: DuplexStream>(
    mut stream: S,
    inner: Arc<RelayInner>,
    origin: StreamOrigin,
    version: u8,
    deadline: Deadline,
) -> anyhow::Result<()> {
    let Ok(handshake) = until(deadline, proto::read_handshake_body(&mut stream)).await else {
        reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
        return Ok(());
    };
    serve_authenticated(stream, inner, origin, handshake, version, deadline).await
}

/// Serve a stream from inside an onion layer: like the direct path, but it
/// cannot park and cannot open with another `Layer`.
async fn serve_layered_stream<S: DuplexStream>(
    mut stream: S,
    inner: Arc<RelayInner>,
    origin: StreamOrigin,
    deadline: Deadline,
) -> anyhow::Result<()> {
    let Ok(header) = until(deadline, proto::read_frame_header(&mut stream)).await else {
        let version = proto::MIN_PROTOCOL_VERSION;
        reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
        return Ok(());
    };
    if header.msg_type != proto::MSG_HANDSHAKE {
        reject(
            &mut stream,
            &inner.stats,
            RejectReason::BadRequest,
            header.version,
        )
        .await;
        return Ok(());
    }
    read_handshake_and_serve(stream, inner, origin, header.version, deadline).await
}

/// Count one refusal and tell the client why (best-effort write), keeping
/// `rejected` in lockstep with what clients were told.
async fn reject<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    stats: &RelayStats,
    reason: RejectReason,
    version: u8,
) {
    stats.rejected.fetch_add(1, Ordering::Relaxed);
    let _ = proto::write_response(stream, Err(reason), version).await;
}

/// Holds [`RelayStats::live_circuits`] up while one admitted stream is served,
/// decrementing on drop so no exit path can leak it.
struct LiveCircuitGuard(Arc<RelayStats>);

impl LiveCircuitGuard {
    fn new(stats: Arc<RelayStats>) -> LiveCircuitGuard {
        stats.live_circuits.fetch_add(1, Ordering::Relaxed);
        LiveCircuitGuard(stats)
    }
}

impl Drop for LiveCircuitGuard {
    fn drop(&mut self) {
        self.0.live_circuits.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Verify the handshake, admit under the rate limits, then serve the terminal
/// frame.
async fn serve_authenticated<S: DuplexStream>(
    mut stream: S,
    inner: Arc<RelayInner>,
    origin: StreamOrigin,
    handshake: Handshake,
    version: u8,
    deadline: Deadline,
) -> anyhow::Result<()> {
    let verified = match proto::verify_handshake(&handshake, proto::now_unix()) {
        Ok(v) => v,
        Err(reason) => {
            reject(&mut stream, &inner.stats, reason, version).await;
            return Ok(());
        }
    };
    inner.stats.authorized.fetch_add(1, Ordering::Relaxed);

    // Keyed on the account and the carrying peer's address (§11.5).
    let admitted = inner
        .rate_limiter
        .admit(origin.ip, verified.account_fingerprint);
    let Some(_circuit_guard) = admitted else {
        inner.stats.rate_limited.fetch_add(1, Ordering::Relaxed);
        reject(&mut stream, &inner.stats, RejectReason::RateLimited, version).await;
        return Ok(());
    };

    let live = LiveCircuitGuard::new(inner.stats.clone());

    let mut anchored = false;
    let terminal = loop {
        let Ok(frame) = until(deadline, proto::read_client_frame(&mut stream)).await else {
            reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
            return Ok(());
        };
        match frame {
            ClientFrame::Command(command) => {
                break Terminal::Command(command);
            }
            ClientFrame::Park(park) => {
                break Terminal::Park(park);
            }
            // A second anchor could only be an attempt to overwrite a verdict.
            ClientFrame::Anchor(_) if anchored => {
                reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
                return Ok(());
            }
            ClientFrame::Anchor(anchor) => {
                anchored = true;
                // Without a pinned log key the anchor is ignored; with one, a bad
                // anchor is treated like a forged cert.
                if let Some(verifier) = inner.anchor.verifier() {
                    let verdict = verifier.verify(
                        &anchor,
                        &verified.user_id,
                        &handshake.cert.account_id_pub,
                        handshake.cert.identity_version,
                        proto::now_unix(),
                    );
                    if let Err(e) = verdict {
                        tracing::debug!("relay: account anchor refused: {e}");
                        inner.stats.anchors_refused.fetch_add(1, Ordering::Relaxed);
                        reject(
                            &mut stream,
                            &inner.stats,
                            RejectReason::Unauthorized,
                            version,
                        )
                        .await;
                        return Ok(());
                    }
                    inner.stats.anchors_verified.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    };

    // v3 clients can't decode `AnchorRequired`, so they get `Unauthorized`.
    if inner.anchor.is_required() && !anchored {
        let reason = if version >= proto::ANCHOR_MIN_VERSION {
            RejectReason::AnchorRequired
        } else {
            RejectReason::Unauthorized
        };
        reject(&mut stream, &inner.stats, reason, version).await;
        return Ok(());
    }

    match terminal {
        Terminal::Command(Command::Connect(connect)) => {
            serve_connect(stream, inner, connect, version).await
        }
        Terminal::Command(Command::Extend(extend)) => {
            serve_extend(stream, inner, extend, version).await
        }
        Terminal::Park(park) => {
            let Some(connection) = origin.connection else {
                reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
                return Ok(());
            };
            // A parked connection is not itself a circuit.
            drop(live);
            serve_park(stream, inner, park, connection, version).await
        }
    }
}

enum Terminal {
    Command(Command),
    Park(Park),
}

/// A peer offering this connection as a middle hop. Registration lasts exactly
/// as long as this stream; QUIC keep-alives, not traffic here, keep it alive.
async fn serve_park<S: DuplexStream>(
    mut stream: S,
    inner: Arc<RelayInner>,
    park: Park,
    connection: quinn::Connection,
    version: u8,
) -> anyhow::Result<()> {
    let Some(_registration) = inner.parked.park(park.relay_leaf_der, connection) else {
        // Already parked by a live connection; see `ParkedPeers::park`.
        reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
        return Ok(());
    };

    proto::write_response(&mut stream, Ok(()), version).await?;

    // Hold until the stream ends; any byte arriving is a violation and unparks.
    let mut byte = [0u8; 1];
    let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut byte).await;
    Ok(())
}

async fn serve_connect<S: DuplexStream>(
    mut stream: S,
    inner: Arc<RelayInner>,
    connect: Connect,
    version: u8,
) -> anyhow::Result<()> {
    inner.stats.connects.fetch_add(1, Ordering::Relaxed);

    // The closed-overlay guarantee (design §1.2).
    if !inner.allowlist.permits(&connect.host, connect.port) {
        reject(&mut stream, &inner.stats, RejectReason::NotAllowed, version).await;
        return Ok(());
    }

    let mut tcp = match dial_target(&connect, &inner.resolve_overrides).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("relay: dial {}:{} failed: {e}", connect.host, connect.port);
            let _ = proto::write_response(&mut stream, Err(RejectReason::DialFailed), version).await;
            return Ok(());
        }
    };
    inner.stats.dials.fetch_add(1, Ordering::Relaxed);

    proto::write_response(&mut stream, Ok(()), version).await?;
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
    Ok(())
}

/// The next hop's identity before its handshake: address only. The full identity
/// is re-checked in [`open_next_hop`]; an empty DER can only fail closed.
fn predial_identity(addr: &str) -> RelayIdentity<'_> {
    RelayIdentity {
        addr,
        cert_der: &[],
    }
}

/// Middle hop: open the next leg (dial, or splice into a parked peer selected by
/// fingerprint), announce the layer, splice.
async fn serve_extend<S: DuplexStream>(
    mut stream: S,
    inner: Arc<RelayInner>,
    extend: Extend,
    version: u8,
) -> anyhow::Result<()> {
    if !inner.allow_extend {
        reject(&mut stream, &inner.stats, RejectReason::ExtendFailed, version).await;
        return Ok(());
    }

    let parked = inner.parked.lookup(&extend.next_cert_sha256);

    // The address bound; a parked peer is exempt because nothing is dialled.
    if parked.is_none() {
        let verdict = inner.next_hops.verdict(
            extend.addr,
            &inner.own_leaf_der,
            proto::now_unix(),
            inner.allow_private_next_hops,
        );
        if !verdict.allowed() {
            // Log the class, never the address (§5.2, §11.7).
            tracing::debug!("relay: refusing to extend — {}", verdict.reason());
            reject(&mut stream, &inner.stats, RejectReason::ExtendFailed, version).await;
            return Ok(());
        }
    }

    // Revocation before the dial or splice, so a seized hop is never contacted.
    // Parked peers are checked too, on their full DER.
    let addr = extend.addr.to_string();
    let identity = match &parked {
        Some(peer) => RelayIdentity {
            addr: &addr,
            cert_der: peer.leaf_der(),
        },
        None => predial_identity(&addr),
    };
    if !inner.revocations.admit(&identity, proto::now_unix()).admitted() {
        tracing::debug!("relay: refusing to extend to {addr} — not admitted by revocation");
        reject(&mut stream, &inner.stats, RejectReason::ExtendFailed, version).await;
        return Ok(());
    }

    let spliced = parked.is_some();
    // `_tunnel` must live for the whole splice: dropping it tears down the
    // loopback pump.
    let leg = tokio::time::timeout(
        inner.extend_dial_timeout,
        open_extend_leg(&inner, &extend, parked),
    )
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("next hop did not answer in time")));
    let (mut next, _tunnel) = match leg {
        Ok(leg) => leg,
        Err(e) => {
            tracing::debug!("relay: extend to {} failed: {e}", extend.addr);
            inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
            let _ = proto::write_response(&mut stream, Err(RejectReason::ExtendFailed), version).await;
            return Ok(());
        }
    };
    inner.stats.extends.fetch_add(1, Ordering::Relaxed);
    if spliced {
        inner.stats.splices.fetch_add(1, Ordering::Relaxed);
    }

    proto::write_response(&mut stream, Ok(()), version).await?;
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut next).await;
    Ok(())
}

/// Open the next leg: splice into a parked peer if named, else dial. Both paths
/// share [`open_next_hop`].
async fn open_extend_leg(
    inner: &RelayInner,
    extend: &Extend,
    parked: Option<ParkedPeer>,
) -> anyhow::Result<(RelayStream, Option<Tunnel>)> {
    match parked {
        Some(peer) => {
            let tunnel = peer.splice().await?;
            let next = open_next_hop(inner, extend, tunnel.addr()).await?;
            Ok((next, Some(tunnel)))
        }
        None => {
            let next = open_next_hop(inner, extend, extend.addr).await?;
            Ok((next, None))
        }
    }
}

/// Dial the next relay at `dial_addr` (a loopback tunnel for a parked peer),
/// pinned to the `Extend` fingerprint and offering only the current ALPN.
///
/// The identical pin is what makes parking safe: a peer that parked under a leaf
/// it lacks the key for can deny that hop's traffic but never read it.
async fn open_next_hop(
    inner: &RelayInner,
    extend: &Extend,
    dial_addr: SocketAddr,
) -> anyhow::Result<RelayStream> {
    tls::ensure_crypto_provider();

    let mut client_crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(FingerprintPinnedVerifier::new(extend.next_cert_sha256))
        .with_no_client_auth();
    client_crypto.alpn_protocols = vec![proto::ALPN.to_vec()];
    let quic_crypto = QuicClientConfig::try_from(client_crypto)?;
    let client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));

    let endpoint = inner.extend_endpoint(dial_addr.is_ipv6()).await?;
    let connection = endpoint
        .connect_with(client_config, dial_addr, tls::RELAY_SERVER_NAME)?
        .await?;

    // Re-check revocation on the full identity now that the leaf exists; a peer
    // that presents no leaf cannot be evaluated, which is not an admission.
    let addr = extend.addr.to_string();
    let leaf = peer_leaf_der(&connection)
        .ok_or_else(|| anyhow::anyhow!("next hop {addr} presented no leaf certificate"))?;
    if !inner
        .revocations
        .admit(
            &RelayIdentity {
                addr: &addr,
                cert_der: &leaf,
            },
            proto::now_unix(),
        )
        .admitted()
    {
        return Err(anyhow::anyhow!("next hop {addr} is not admitted by revocation"));
    }

    let (send, recv) = connection.open_bi().await?;
    let mut next = RelayStream::new(send, recv, Some(connection), None);

    // Wait for the ack so a failure is a clean rejection, not a dead stream.
    proto::write_layer(&mut next, proto::PROTOCOL_VERSION).await?;
    proto::read_response(&mut next).await?;
    Ok(next)
}

/// The DER leaf the outbound peer presented. `None` must be treated as
/// "cannot evaluate", never "no objection".
fn peer_leaf_der(connection: &quinn::Connection) -> Option<Vec<u8>> {
    let identity = connection.peer_identity()?;
    let chain = identity
        .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
        .ok()?;
    chain.first().map(|cert| cert.as_ref().to_vec())
}

async fn dial_target(connect: &Connect, overrides: &HashMap<String, IpAddr>) -> std::io::Result<TcpStream> {
    if let Some(ip) = overrides.get(&connect.host) {
        return TcpStream::connect(SocketAddr::new(*ip, connect.port)).await;
    }
    TcpStream::connect((connect.host.as_str(), connect.port)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_host_permits_only_https() {
        let list = Allowlist::from_patterns(["api.pollis.com"]);
        assert!(list.permits("api.pollis.com", 443));
        assert!(list.permits("API.pollis.com", 443), "host match is case-insensitive");
        assert!(!list.permits("api.pollis.com", 8443));
        assert!(!list.permits("api.pollis.com", 80));
        assert!(!list.permits("api.pollis.com", 5349));
    }

    #[test]
    fn an_explicit_port_permits_exactly_that_port() {
        let list = Allowlist::from_patterns(["api.pollis.com:8443"]);
        assert!(list.permits("api.pollis.com", 8443));
        assert!(!list.permits("api.pollis.com", 443));
    }

    #[test]
    fn a_wildcard_port_permits_any_port() {
        let list = Allowlist::from_patterns(["*.turso.io:*"]);
        assert!(list.permits("db.turso.io", 443));
        assert!(list.permits("db.turso.io", 1));
        assert!(!list.permits("turso.io", 443), "suffix glob needs a subdomain");
    }

    #[test]
    fn a_bare_star_is_any_host_on_https_only() {
        let list = Allowlist::from_patterns(["*"]);
        assert!(list.permits("anything.example", 443));
        assert!(!list.permits("anything.example", 8080));
        let open = Allowlist::from_patterns(["*:*"]);
        assert!(open.permits("anything.example", 8080));
    }

    #[test]
    fn a_malformed_entry_fails_closed_and_loudly() {
        let bad_entries = [
            "",
            "api.pollis.com:",
            "api.pollis.com:99999",
            "api.pollis.com:0",
            "api.pollis.com:https",
            ":443",
            "::1",
        ];
        for bad in bad_entries {
            assert!(HostPattern::try_parse(bad).is_err(), "{bad:?} must be rejected");
            assert!(Allowlist::try_from_patterns([bad]).is_err());
            let list = Allowlist::from_patterns([bad]);
            assert!(!list.permits("api.pollis.com", 443), "{bad:?} must permit nothing");
            assert!(!list.permits("", 0), "{bad:?} must permit nothing, not even an empty host");
        }
    }

    #[test]
    fn routing_matches_on_host_alone() {
        let pattern = HostPattern::parse("api.pollis.com:8443");
        assert!(pattern.matches_host("api.pollis.com"));
        assert!(!pattern.matches("api.pollis.com", 443));
    }

    #[test]
    fn an_empty_allowlist_permits_nothing() {
        assert!(!Allowlist::default().permits("api.pollis.com", 443));
    }
}
