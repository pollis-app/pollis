//! The minimal relay server: QUIC in, allowlisted TCP dial out, bytes piped —
//! or, on a multi-hop circuit, QUIC in and QUIC on to the next relay.
//!
//! Per bi-stream: the offline device-cert handshake (§9.4, §11.1), rate limits (§11.5), then one command:
//!
//! - **`Connect`** — last hop: allowlist (§14.0), TCP dial, pipe opaque bytes (§8).
//! - **`Extend`** (v4+) — middle hop: directory ([`crate::nexthop`]) and revocation checks, QUIC dial, `Layer`, splice.
//!
//! A **`Park`** (v4+) from a NAT'd peer relay keeps its connection for later `Extend` splices ([`crate::park`]).
//!
//! An optional **`Anchor`** (v4+) may precede the command ([`crate::anchor`]).
//!
//! A `Layer`-opened stream is served like a direct one and **keyed on its address for per-IP limits**.
//!
//! `Connect` reaches only the last hop, so the allowlist (§1.2) holds for every path length.
//!
//! Graceful shutdown: [`RelayServer::spawn_with_shutdown`].
//!
//! Before any handshake: QUIC Retry, per-IP accept caps, a completed-only global cap, [`RelayConfig::first_frame_timeout`].

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
    /// Matches no host at all — what a malformed entry parses to, so that a
    /// typo can only ever close a destination, never open one.
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
        // `parse` lowercased the stored forms, so compare ASCII-insensitively without copying.
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

/// A destination matcher: host **and** port, so an allowlisted host's other ports stay closed.
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
    /// A malformed entry matches **nothing** (fail closed); [`HostPattern::try_parse`] reports it instead.
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
        // The allowlist names hosts, never IPv6 literals, so the last colon can
        // only be the port separator; more than one colon is not an entry.
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

    /// True if `host:port` is a destination this pattern permits.
    pub(crate) fn matches(&self, host: &str, port: u16) -> bool {
        let port_ok = match self.port {
            PortPattern::Any => true,
            PortPattern::Exact(p) => p == port,
        };
        port_ok && self.host.matches(host)
    }

    /// True if `host` matches the host half: the shim's routing question, not the egress one.
    pub(crate) fn matches_host(&self, host: &str) -> bool {
        self.host.matches(host)
    }
}

/// The relay's static destination allowlist.
#[derive(Debug, Clone, Default)]
pub struct Allowlist(Vec<HostPattern>);

impl Allowlist {
    /// Build from raw patterns; a malformed entry matches nothing (see [`HostPattern::parse`]).
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

    /// True if `host:port` is permitted. An empty allowlist permits nothing.
    pub fn permits(&self, host: &str, port: u16) -> bool {
        self.0.iter().any(|p| p.matches(host, port))
    }

    /// Consume into the raw patterns (used by the routing policy).
    pub fn into_patterns(self) -> Vec<HostPattern> {
        self.0
    }
}

/// Observable relay counters over shared atomics.
#[derive(Debug, Default)]
pub struct RelayStats {
    /// Streams whose handshake verified.
    pub authorized: AtomicU64,
    /// Streams rejected before a dial (auth, allowlist, or rate limit).
    pub rejected: AtomicU64,
    /// Streams rejected specifically for tripping a rate/concurrency limit.
    pub rate_limited: AtomicU64,
    /// Targets the relay actually TCP-dialed — proof a hop was traversed.
    pub dials: AtomicU64,
    /// `Connect` frames parsed — i.e. how many times this node was a circuit's
    /// LAST hop and therefore learned a destination.
    pub connects: AtomicU64,
    /// `Extend` frames honoured — how many times this node was a middle hop and
    /// forwarded to another relay without learning the destination.
    pub extends: AtomicU64,
    /// Of those, how many were spliced into a **parked peer** rather than dialled.
    pub splices: AtomicU64,
    /// Onion layers peeled — how many streams reached this node from another
    /// relay rather than straight from a client.
    pub layers: AtomicU64,
    /// Circuits open on this node **right now** — incremented once a stream is
    /// admitted and decremented when it ends, on every exit path.
    pub live_circuits: AtomicI64,
    /// Transparency-log account anchors that verified (#813 Phase E2).
    pub anchors_verified: AtomicU64,
    /// Anchors presented and refused, separate from `rejected`.
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
    /// Circuits carried right now.
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

/// An optional callback invoked with the transport peer address of every
/// accepted QUIC connection.
///
/// `None` in production (the relay stores nothing about who connects, §5.2); tests use it.
pub type PeerObserver = Arc<dyn Fn(SocketAddr) + Send + Sync>;

/// Configuration for a [`RelayServer`].
pub struct RelayConfig {
    /// UDP socket to bind the QUIC endpoint to.
    pub bind: SocketAddr,
    /// Static destination allowlist (design §1.2).
    pub allowlist: Allowlist,
    /// The relay's own QUIC identity (self-signed). Callers keep the cert to pin
    /// it client-side.
    pub identity: SelfSignedIdentity,
    /// Host → IP overrides applied before dialing. Lets tests point a real DNS
    /// name (e.g. `origin.test`) at a loopback listener; also a pinning hook.
    pub resolve_overrides: HashMap<String, IpAddr>,
    /// In-memory abuse control (design §11.5).
    pub rate_limits: RateLimitConfig,
    /// Global cap on handshake-completed QUIC connections (half-open ones are bounded separately).
    pub max_concurrent_connections: u32,
    /// Per-source-IP QUIC connection cap, at accept after address validation.
    pub max_connections_per_ip: u32,
    /// How long a new stream may take to finish negotiating before it is refused.
    pub first_frame_timeout: Duration,
    /// Whether this node will act as a **middle hop** and honour `Extend` by
    /// dialing another relay (design §6.2). Default `true`.
    pub allow_extend: bool,
    /// Which addresses this node will open a next leg to ([`crate::nexthop`]); defaults to permitting **nothing**.
    pub next_hops: NextHopDirectory,
    /// Let `Extend` name directory-listed private addresses. **Test rigs and lab pools only.**
    pub allow_private_next_hops: bool,
    /// Deadline for opening the next leg (dial or splice, handshake, `Layer` ack).
    pub extend_dial_timeout: Duration,
    /// Live relay revocation (#813 Phase C), consulted before this node hands a
    /// circuit to a next hop.
    ///
    /// Defaults to [`RevocationStore::unconfigured`], which admits **nothing**.
    pub revocations: RevocationStore,
    /// Transparency-log account anchoring (#813 Phase E2). Defaults to
    /// [`AnchorPolicy::Ignore`] — no log key, no claim.
    pub anchor: AnchorPolicy,
    /// Peers currently parked at this node, shared with the health endpoint.
    pub parked: Arc<ParkedPeers>,
    /// Shared counters.
    pub stats: Arc<RelayStats>,
    /// Optional peer-address observation hook. `None` in production — see
    /// [`PeerObserver`].
    pub peer_observer: Option<PeerObserver>,
}

impl RelayConfig {
    /// Build a config with a fresh self-signed identity; deployable nodes use [`RelayConfig::with_identity`].
    pub fn new(bind: SocketAddr, allowlist: Allowlist) -> anyhow::Result<Self> {
        let identity = tls::generate_self_signed(tls::RELAY_SERVER_NAME)?;
        Ok(Self::with_identity(bind, allowlist, identity))
    }

    /// Build a config around a caller-supplied (e.g. persisted) QUIC identity.
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

    /// The DER cert a client must pin to connect to this relay.
    pub fn server_cert(&self) -> rustls::pki_types::CertificateDer<'static> {
        self.identity.cert_der.clone()
    }
}

/// Fields the per-stream handler needs, shared behind an `Arc`.
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
    /// This node's own QUIC leaf, so its own directory entry is recognisable —
    /// a hop to ourselves is a loop, never a next hop.
    own_leaf_der: Vec<u8>,
    revocations: RevocationStore,
    anchor: AnchorPolicy,
    parked: Arc<ParkedPeers>,
    peer_observer: Option<PeerObserver>,
    /// Terminates an onion layer addressed to this node. Built once — it holds
    /// only our own leaf cert, which is what clients pin.
    layer_acceptor: TlsAcceptor,
    /// One shared outbound QUIC endpoint per address family, created on first use.
    extend_endpoint_v4: OnceCell<quinn::Endpoint>,
    extend_endpoint_v6: OnceCell<quinn::Endpoint>,
}

impl RelayInner {
    /// The shared outbound QUIC endpoint for extends to `family`.
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

/// A running relay.
pub struct RelayServer;

impl RelayServer {
    /// Spawn a relay, returning the accept task and bound address; see [`RelayServer::spawn_with_shutdown`].
    pub fn spawn(config: RelayConfig) -> anyhow::Result<(JoinHandle<()>, SocketAddr)> {
        Self::spawn_with_shutdown(config, std::future::pending::<()>(), Duration::from_secs(0))
    }

    /// Spawn a relay that stops accepting on `shutdown`, drains up to `drain_timeout`, then returns.
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
        // Every generation, newest first: rustls picks the first of OUR list the peer offered.
        server_crypto.alpn_protocols = proto::SUPPORTED_ALPNS.iter().map(|a| a.to_vec()).collect();

        let quic_crypto = QuicServerConfig::try_from(server_crypto)?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
        let mut transport = quinn::TransportConfig::default();
        // One bi-stream per connection is all a legitimate peer uses; quinn's default of 100 is far too many.
        transport.max_concurrent_bidi_streams(quinn::VarInt::from_u32(
            MAX_BIDI_STREAMS_PER_CONNECTION,
        ));
        // Nothing in this protocol is unidirectional.
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
        // Established connections and in-flight handshakes are counted apart, so Initials cannot fill the first.
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
                        // Address validation first (RFC 9000 §8.1): a spoofed source costs nothing and is never counted.
                        if !incoming.remote_address_validated() {
                            if let Err(e) = incoming.retry() {
                                e.into_incoming().refuse();
                            }
                            continue;
                        }
                        // Hashed on the spot: the limiter only ever holds the
                        // per-boot pseudonym, never the address.
                        let ip = IpKey::of(incoming.remote_address().ip());
                        // Shed load cleanly: at either global cap, refuse rather
                        // than handshake for nothing.
                        if handshaking.load(Ordering::Relaxed) >= max_conns
                            || live_conns.load(Ordering::Relaxed) >= max_conns
                        {
                            incoming.refuse();
                            continue;
                        }
                        // Per-IP slot, held for the connection's whole life.
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
                            // Re-check the global cap: the pre-check raced other handshakes.
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

            // Stop accepting, then drain in-flight connections to the deadline.
            let _ = tokio::time::timeout(drain_timeout, endpoint.wait_idle()).await;
            endpoint.close(0u32.into(), b"relay shutting down");
        });

        Ok((handle, local_addr))
    }
}

/// Where a stream came from, as far as this node can know.
///
/// `ip` is the carrying QUIC peer's; every admission keys on it, so no stream escapes per-IP bounds.
///
/// `connection` is `None` for a layered stream, which makes parking through a layer impossible.
struct StreamOrigin {
    /// The peer's per-boot pseudonym (`IpKey::of`), never its address.
    ip: IpKey,
    connection: Option<quinn::Connection>,
}

impl StreamOrigin {
    /// The same peer address, minus the connection: what a stream that came
    /// through an onion layer on this connection is attributed to.
    fn layered(&self) -> StreamOrigin {
        StreamOrigin {
            ip: self.ip,
            connection: None,
        }
    }
}

/// Bi-streams one connection may hold open (one in use, plus teardown slack).
const MAX_BIDI_STREAMS_PER_CONNECTION: u32 = 4;

/// The instant by which a stream must have finished negotiating.
type Deadline = tokio::time::Instant;

/// Run one negotiation read under the deadline; a timeout counts as a malformed exchange.
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
    // Each target gets its own bi-stream; serve them until the peer goes away.
    while let Ok((send, recv)) = connection.accept_bi().await {
        let inner = inner.clone();
        let origin = StreamOrigin {
            ip: peer_key,
            connection: Some(connection.clone()),
        };
        // The clock on negotiating starts the moment the stream exists.
        let deadline = Deadline::now() + inner.first_frame_timeout;
        tokio::spawn(async move {
            if let Err(e) = handle_quic_stream(send, recv, inner, origin, deadline).await {
                tracing::debug!("relay: stream ended: {e}");
            }
        });
    }
}

/// Serve one bi-stream off the endpoint; the only place `Layer` is legal, and it buys no limit exemption.
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
            // Acknowledge to the previous relay (the client never sees this), then peel the layer.
            proto::write_response(&mut stream, Ok(()), header.version).await?;
            // The layer's TLS handshake is part of negotiating, so it runs
            // under the same deadline as every frame.
            let accept = inner.layer_acceptor.accept(stream);
            let layered = match tokio::time::timeout_at(deadline, accept).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::debug!("relay: onion layer handshake failed: {e}");
                    // Counted but not answered: the `Ok` above has already gone
                    // out on this stream, so there is no second verdict to send.
                    inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
                Err(_) => {
                    tracing::debug!("relay: onion layer handshake timed out");
                    inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
            };
            // Whoever sent the `Layer` is on the far end of this connection, and
            // what is inside it is limited under that address.
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

/// Read the handshake body, then serve what follows.
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

/// Serve a stream that arrived inside an onion layer. Identical to the direct
/// path except that it cannot park (no connection handle) and a `Layer` is no
/// longer accepted (layers are announced hop-to-hop, never nested in-band).
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

/// Count one refusal and tell the client why; the counter must track responses exactly.
async fn reject<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    stats: &RelayStats,
    reason: RejectReason,
    version: u8,
) {
    stats.rejected.fetch_add(1, Ordering::Relaxed);
    let _ = proto::write_response(stream, Err(reason), version).await;
}

/// Holds [`RelayStats::live_circuits`] up while an admitted stream is served, on every exit path.
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

/// Verify the handshake, admit under the rate limits, then serve the command.
async fn serve_authenticated<S: DuplexStream>(
    mut stream: S,
    inner: Arc<RelayInner>,
    origin: StreamOrigin,
    handshake: Handshake,
    version: u8,
    deadline: Deadline,
) -> anyhow::Result<()> {
    // 1. Handshake — reject on any auth failure, never fail open.
    let verified = match proto::verify_handshake(&handshake, proto::now_unix()) {
        Ok(v) => v,
        Err(reason) => {
            reject(&mut stream, &inner.stats, reason, version).await;
            return Ok(());
        }
    };
    inner.stats.authorized.fetch_add(1, Ordering::Relaxed);

    // 2. Rate limits on the account AND the carrying QUIC peer's address (§11.5).
    let admitted = inner
        .rate_limiter
        .admit(origin.ip, verified.account_fingerprint);
    let Some(_circuit_guard) = admitted else {
        inner.stats.rate_limited.fetch_add(1, Ordering::Relaxed);
        reject(&mut stream, &inner.stats, RejectReason::RateLimited, version).await;
        return Ok(());
    };

    // Admitted, so this stream is a circuit this node is carrying until it ends.
    // (A parked peer's stream gives this back below: it carries no circuit.)
    let live = LiveCircuitGuard::new(inner.stats.clone());

    // 3. An optional anchor, then exactly one terminal frame: a command or a park.
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
            // A second anchor is malformed: it could only try to overwrite a verdict.
            ClientFrame::Anchor(_) if anchored => {
                reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
                return Ok(());
            }
            ClientFrame::Anchor(anchor) => {
                anchored = true;
                // Without a log key there is no basis to judge; with one, a bad anchor is a forged credential.
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

    // Anchor required but absent; a v3 client gets a reason code its generation understands.
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
            // Parking needs the connection itself, so a stream that arrived
            // inside an onion layer cannot park: there is nothing to park.
            let Some(connection) = origin.connection else {
                reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
                return Ok(());
            };
            // A parked connection is not a circuit until an `Extend` is spliced in.
            drop(live);
            serve_park(stream, inner, park, connection, version).await
        }
    }
}

/// The single frame that ends a client's negotiation.
enum Terminal {
    Command(Command),
    Park(Park),
}

/// A peer offering this connection as a middle hop (#813 Phase P1).
///
/// Registration lasts exactly as long as this stream; nothing more is read or written.
async fn serve_park<S: DuplexStream>(
    mut stream: S,
    inner: Arc<RelayInner>,
    park: Park,
    connection: quinn::Connection,
    version: u8,
) -> anyhow::Result<()> {
    let Some(_registration) = inner.parked.park(park.relay_leaf_der, connection) else {
        // Already parked here by a live connection — see `ParkedPeers::park` for
        // why the incumbent keeps it.
        reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
        return Ok(());
    };

    // Acknowledged with the ordinary `Ok` response.
    proto::write_response(&mut stream, Ok(()), version).await?;

    // Hold the registration until the stream ends. A byte arriving here is a
    // protocol violation, not a keep-alive, and unparks just as a close does.
    let mut byte = [0u8; 1];
    let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut byte).await;
    Ok(())
}

/// Last hop: allowlist, dial, splice.
async fn serve_connect<S: DuplexStream>(
    mut stream: S,
    inner: Arc<RelayInner>,
    connect: Connect,
    version: u8,
) -> anyhow::Result<()> {
    inner.stats.connects.fetch_add(1, Ordering::Relaxed);

    // Allowlist (§1.2), enforced wherever `Connect` lands, binding host AND port.
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

    // The pipe is live; the accept loop keeps the QUIC connection alive.
    proto::write_response(&mut stream, Ok(()), version).await?;
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
    Ok(())
}

/// The next hop's identity as far as it is knowable **before** its QUIC
/// handshake: its advertised address, and no cert, because the DER leaf does not
/// exist until the peer presents it.
///
/// Only `addr` can match here; [`open_next_hop`] re-checks the full identity, and an empty DER fails closed.
fn predial_identity(addr: &str) -> RelayIdentity<'_> {
    RelayIdentity {
        addr,
        cert_der: &[],
    }
}

/// Middle hop: open the next leg (dial, or splice into a parked peer), announce the layer, splice.
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

    // The address bound (#813); a parked peer is exempt because there is no dial.
    if parked.is_none() {
        let verdict = inner.next_hops.verdict(
            extend.addr,
            &inner.own_leaf_der,
            proto::now_unix(),
            inner.allow_private_next_hops,
        );
        if !verdict.allowed() {
            // The reason is a class, never the address: a relay records nothing
            // about who asked it for what (§5.2, §11.7).
            tracing::debug!("relay: refusing to extend — {}", verdict.reason());
            reject(&mut stream, &inner.stats, RejectReason::ExtendFailed, version).await;
            return Ok(());
        }
    }

    // Revocation BEFORE dial or splice; `Unevaluable` resolves like `Revoked`, parked peers included.
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
    // `_tunnel` must outlive the splice. Bounded, so a blackhole costs one refusal.
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

/// Open the next leg of a circuit: splice into a parked peer if the frame named
/// one, else dial.
///
/// Both paths end in [`open_next_hop`], with the same pin and re-check.
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

/// Dial the next relay of a circuit at `dial_addr` and get it ready to receive
/// an onion layer.
///
/// Pinned to the `Extend`'s cert fingerprint, offering only the current ALPN.
///
/// For a parked peer `dial_addr` is a loopback tunnel; the same pin makes parking safe.
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

    // Now the DER exists, re-check revocation on the full identity; an unknown cert is not an admission.
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

    // Wait for the next hop's ack so a failure surfaces as a clean rejection.
    proto::write_layer(&mut next, proto::PROTOCOL_VERSION).await?;
    proto::read_response(&mut next).await?;
    Ok(next)
}

/// The DER leaf certificate the peer of an outbound QUIC connection presented.
///
/// `None` must be treated as "cannot evaluate", never "no objection".
fn peer_leaf_der(connection: &quinn::Connection) -> Option<Vec<u8>> {
    let identity = connection.peer_identity()?;
    let chain = identity
        .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
        .ok()?;
    chain.first().map(|cert| cert.as_ref().to_vec())
}

/// Resolve `connect.host` (honoring overrides) and open a TCP connection.
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
        // The whole point: an allowlisted host is one service, not a machine.
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
        // The shim asks "is this an overlay host?" without a port; the entry's
        // port restriction is the relay's business at the far end.
        let pattern = HostPattern::parse("api.pollis.com:8443");
        assert!(pattern.matches_host("api.pollis.com"));
        assert!(!pattern.matches("api.pollis.com", 443));
    }

    #[test]
    fn an_empty_allowlist_permits_nothing() {
        assert!(!Allowlist::default().permits("api.pollis.com", 443));
    }
}
