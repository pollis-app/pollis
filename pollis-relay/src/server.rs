//! The minimal relay server: QUIC in, allowlisted TCP dial out, bytes piped —
//! or, on a multi-hop circuit, QUIC in and QUIC on to the next relay.
//!
//! Per accepted bi-stream the relay runs the OFFLINE device-cert handshake
//! (design §9.4 — no Turso query, no network call: the client presents its cert
//! chain and the relay verifies it locally, which is what keeps the relay tier
//! out of the metadata plane, §11.1), applies per-account / per-IP rate limits
//! (§11.5), and then reads one command:
//!
//! - **`Connect`** — this node is the circuit's last hop. It checks the host
//!   against the **static allowlist** (policy, not protocol — design §14.0),
//!   TCP-dials the target, and pipes bytes until either side closes. It never
//!   terminates the inner TLS — it forwards opaque bytes only (design §8).
//! - **`Extend`** (v4+) — this node is a middle hop. It checks the next hop
//!   against the live **revocation** store (#813 Phase C — fail-closed, and a
//!   node with no directory key configured admits nothing, so it cannot extend
//!   at all), opens a QUIC connection to the named next relay (pinning the
//!   fingerprint the frame carries), re-checks the identity the peer actually
//!   presented, announces itself with a `Layer` frame, and splices the two
//!   streams. From there it forwards ciphertext it cannot read: the client runs
//!   an onion layer to that next hop straight through it (design §6.2,
//!   [`crate::onion`]).
//!
//! A third terminal frame exists for one kind of client: a **`Park`** (v4+) from
//! a peer-hosted relay, which offers the connection it just opened as a middle
//! hop and keeps it open. A later `Extend` naming that peer's leaf fingerprint is
//! spliced into that connection instead of dialling — a peer behind NAT cannot
//! be dialled, and the relay is not an ICE agent, so the connection the peer
//! opened is the only one that can exist. See [`crate::park`]; the onion wire is
//! unchanged, and revocation applies to a parked next hop exactly as to a dialled
//! one.
//!
//! Between the handshake and that command a client may present one optional
//! **`Anchor`** frame (v4+): a transparency-log inclusion proof for its account
//! key, verified with zero I/O against this node's pinned log key (#813 Phase E2,
//! [`crate::anchor`]). Whether one is *required* is node policy.
//!
//! A stream that opens with a `Layer` frame is one another relay is extending
//! into us. We acknowledge, terminate the layer with our own leaf cert, and serve
//! whatever is inside exactly as if it had arrived directly. The peer address of
//! such a stream belongs to the previous relay — *if* the sender really is one.
//! An opening `Layer` is unauthenticated, any client can send it, and this node
//! holds no directory to check the address against, so a layered stream is
//! **keyed on that address for per-IP limits exactly like a direct one**.
//! Exempting it would make one frame the way past every per-IP bound.
//!
//! The allowlist is enforced on `Connect` at whichever node receives it, which is
//! by construction the last hop — so the closed-overlay guarantee (§1.2) holds
//! for every path length. `Extend` is not an allowlist bypass: it reaches relays,
//! never destinations, and the only thing that comes back out of it is more
//! ciphertext.
//!
//! Deployability (Slice 2a): a global concurrent-connection cap, in-memory rate
//! limiting, and **graceful shutdown** (stop accepting, drain in-flight pipes to
//! a bounded deadline, exit) via [`RelayServer::spawn_with_shutdown`].
//!
//! Unauthenticated surface (the part an attacker reaches before any handshake):
//! every new connection is **address-validated** with a QUIC Retry before it
//! costs this node anything, connections are bounded **per source IP** at
//! accept, the global cap counts only connections whose handshake **completed**
//! (half-open ones have their own, separate bound of the same size), a stream
//! that has not finished negotiating within [`RelayConfig::first_frame_timeout`]
//! is refused, and a connection may hold only a handful of streams — a client
//! legitimately needs one.

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
use crate::onion;
use crate::park::{ParkedPeer, ParkedPeers, Tunnel};
use crate::policy::{RelayIdentity, RevocationStore};
use crate::proto::{self, ClientFrame, Command, Connect, Extend, Handshake, Park, RejectReason};
use crate::ratelimit::{ConnectionLimiter, RateLimitConfig, RateLimiter};
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
        // `parse` already lowercased both stored forms, so an ASCII-insensitive
        // compare gives the same answer without lowercasing a fresh copy of the
        // host for every pattern in the list — this runs once per pattern per
        // `Connect`, and `Any` never looks at the host at all.
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

/// A destination matcher: a host pattern **and** a port pattern. The allowlist
/// is relay-side *policy*: the wire protocol carries an arbitrary `host:port`,
/// and the relay decides what it will dial. A `Connect` has to match both halves
/// — a host-only match would make every port of an allowlisted host reachable,
/// which is a different (and much larger) set of services than the one the
/// allowlist names.
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
    /// Infallible for callers that cannot surface an error: a malformed entry
    /// becomes a pattern that matches **nothing** (fail closed), and is logged.
    /// The deployable bin uses [`HostPattern::try_parse`] so a typo aborts
    /// startup instead.
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

    /// True if `host` matches the host half, whatever the port. This is the
    /// **routing** question ("does this host belong to the overlay plane?"),
    /// not the egress one — the shim decides where to send a connection by
    /// host, and the relay at the far end decides whether to dial it by both.
    pub(crate) fn matches_host(&self, host: &str) -> bool {
        self.host.matches(host)
    }
}

/// The relay's static destination allowlist.
#[derive(Debug, Clone, Default)]
pub struct Allowlist(Vec<HostPattern>);

impl Allowlist {
    /// Build from raw patterns (`*.turso.io`, `api.pollis.com:443`, `*:*`). A
    /// malformed entry matches nothing — see [`HostPattern::parse`]; use
    /// [`Allowlist::try_from_patterns`] where an error can be surfaced.
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

/// Observable relay counters, for tests and (later) metrics. Cloneable handle
/// over shared atomics.
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
    /// Of those, how many were spliced into a **parked peer** rather than dialled
    /// (#813 Phase P1). An aggregate count of a mechanism, like every other field
    /// here — it names no peer and no client.
    pub splices: AtomicU64,
    /// Onion layers peeled — how many streams reached this node from another
    /// relay rather than straight from a client.
    pub layers: AtomicU64,
    /// Circuits open on this node **right now** — incremented once a stream is
    /// admitted and decremented when it ends, on every exit path.
    ///
    /// A gauge, not a counter: every other field here only ever grows, so
    /// "how many circuits am I carrying" could previously only be approximated by
    /// counting live links, which under-counts a link carrying several circuits
    /// (requested by phase D1 for the be-a-relay status line). It is a bare
    /// number — it says nothing about who, from where, or to what.
    pub live_circuits: AtomicI64,
    /// Transparency-log account anchors that verified (#813 Phase E2).
    pub anchors_verified: AtomicU64,
    /// Anchors that were presented and refused. Counted separately from
    /// `rejected` so an operator can tell "clients are failing to anchor" from
    /// "clients are failing to authenticate" — both are aggregate counters, and
    /// neither records anything about who connected (§5.2, §11.7).
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
    /// Circuits carried right now. Never negative in practice — the guard that
    /// decrements is created at the same point the increment happens.
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
/// Production nodes leave this `None` — the relay stores nothing about who
/// connects to it, and that is load-bearing (§5.2, §11.7). It exists so a test
/// can assert the structural property that makes multi-hop worth doing: the
/// address a middle or last hop sees belongs to the previous relay, never to the
/// client.
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
    /// Global cap on simultaneously-open QUIC connections whose handshake has
    /// completed. Handshakes in flight are bounded separately by the same
    /// number, so a flood of half-open connections cannot evict live ones.
    pub max_concurrent_connections: u32,
    /// Cap on simultaneously-open QUIC connections from one source IP, enforced
    /// at accept — after QUIC address validation, before the handshake. Every
    /// circuit rides its own connection today (a client's, or a previous
    /// relay's extend leg), so this also bounds circuits per address.
    pub max_connections_per_ip: u32,
    /// How long a freshly-opened stream may take to finish negotiating — opening
    /// frame, handshake, optional anchor, terminal command — before it is
    /// refused. Bounds the tasks (and circuit slots) a peer can park by opening
    /// streams and going quiet.
    pub first_frame_timeout: Duration,
    /// Whether this node will act as a **middle hop** and honour `Extend` by
    /// dialing another relay (design §6.2). Default `true`.
    ///
    /// The capability is bounded rather than open-ended: only a client whose
    /// device cert verifies can ask for it, the target must complete a QUIC
    /// handshake presenting the exact pinned cert fingerprint the frame carries,
    /// and the existing per-account limits cap how many a device may hold open.
    /// An operator who wants a node to be last-hop-only sets this `false`.
    pub allow_extend: bool,
    /// Live relay revocation (#813 Phase C), consulted before this node hands a
    /// circuit to a next hop.
    ///
    /// Defaults to [`RevocationStore::unconfigured`], which admits **nothing** —
    /// so a node that was never given the pinned directory key cannot act as a
    /// middle hop at all. That is deliberate and is the whole reason the feature
    /// is not decoration: the permissive default is how revocation quietly stops
    /// meaning anything. `allow_extend` is the orthogonal coarse switch ("never
    /// be a middle hop"); this is "be one only for hops you can still vouch for".
    pub revocations: RevocationStore,
    /// Transparency-log account anchoring (#813 Phase E2). Defaults to
    /// [`AnchorPolicy::Ignore`] — no log key, no claim.
    pub anchor: AnchorPolicy,
    /// Peers currently parked at this node (#813 Phase P1). Shared like
    /// [`RelayConfig::stats`] so the health endpoint can publish the parked
    /// fingerprints — and nothing else — for the directory reconciler.
    pub parked: Arc<ParkedPeers>,
    /// Shared counters.
    pub stats: Arc<RelayStats>,
    /// Optional peer-address observation hook. `None` in production — see
    /// [`PeerObserver`].
    pub peer_observer: Option<PeerObserver>,
}

impl RelayConfig {
    /// Build a config, auto-generating a fresh self-signed QUIC identity. For a
    /// deployable node use [`tls::load_or_generate_identity`] +
    /// [`RelayConfig::with_identity`] so the identity is stable across restarts.
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
    revocations: RevocationStore,
    anchor: AnchorPolicy,
    parked: Arc<ParkedPeers>,
    peer_observer: Option<PeerObserver>,
    /// Terminates an onion layer addressed to this node. Built once — it holds
    /// only our own leaf cert, which is what clients pin.
    layer_acceptor: TlsAcceptor,
    /// One shared QUIC client endpoint per address family for outbound
    /// `Extend`s, created on first use. quinn multiplexes every connection over
    /// one endpoint, so this keeps a busy middle hop from burning a UDP socket
    /// per circuit.
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
    /// Spawn a relay on `config.bind`, returning the accept task and the actual
    /// bound address (useful when binding to port 0). The task runs until the
    /// `JoinHandle` is aborted. For clean shutdown use
    /// [`RelayServer::spawn_with_shutdown`].
    pub fn spawn(config: RelayConfig) -> anyhow::Result<(JoinHandle<()>, SocketAddr)> {
        Self::spawn_with_shutdown(config, std::future::pending::<()>(), Duration::from_secs(0))
    }

    /// Spawn a relay that shuts down gracefully when `shutdown` resolves: it stops
    /// accepting new connections, lets in-flight pipes drain for up to
    /// `drain_timeout`, then closes the endpoint and the task returns (exit 0 in
    /// the binary). The returned `JoinHandle` completes once draining is done.
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
        // Offer every generation we speak, newest first: rustls picks the first
        // of OUR list the peer also offered, so two current peers land on v4
        // while a still-shipped v3 client keeps working single-hop.
        server_crypto.alpn_protocols = proto::SUPPORTED_ALPNS.iter().map(|a| a.to_vec()).collect();

        let quic_crypto = QuicServerConfig::try_from(server_crypto)?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
        let mut transport = quinn::TransportConfig::default();
        // A client opens exactly one bi-stream per connection (one circuit); a
        // previous relay's extend leg and a parking peer likewise. quinn's
        // default of 100 would let one held connection park 100 negotiating
        // tasks; a little slack over one is all a legitimate peer can use.
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
            revocations: config.revocations,
            anchor: config.anchor,
            parked: config.parked,
            peer_observer: config.peer_observer,
            layer_acceptor,
            extend_endpoint_v4: OnceCell::new(),
            extend_endpoint_v6: OnceCell::new(),
        });
        let max_conns = config.max_concurrent_connections.max(1) as u64;
        // Established connections and handshakes in flight are counted apart:
        // the first is what `max_concurrent_connections` promises, the second
        // is bounded by the same number so a flood of Initials that never
        // complete can fill its own bucket but never the established one.
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
                        // Address validation first (RFC 9000 §8.1): a Retry is
                        // stateless and no larger than the Initial it answers,
                        // so a spoofed source costs this node nothing and is
                        // never counted anywhere below. Every cap after this
                        // line keys on an address the sender has proved it can
                        // receive at.
                        if !incoming.remote_address_validated() {
                            if let Err(e) = incoming.retry() {
                                e.into_incoming().refuse();
                            }
                            continue;
                        }
                        let ip = incoming.remote_address().ip();
                        // Shed load cleanly: at either global cap, refuse rather
                        // than handshake for nothing.
                        if handshaking.load(Ordering::Relaxed) >= max_conns
                            || live_conns.load(Ordering::Relaxed) >= max_conns
                        {
                            incoming.refuse();
                            continue;
                        }
                        // Per-IP cap. The slot is held for the connection's whole
                        // life — handshake included — and freed when the task
                        // ends, however it ends.
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
                            // Handshake done: this is the connection the global
                            // cap is about. Re-check it here, because the
                            // pre-check above raced other handshakes finishing.
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

            // Graceful shutdown: we've left the accept loop, so no new connection
            // is served (quinn drops un-accepted incoming). Drain the in-flight
            // ones to the deadline — `wait_idle` returns once all connections have
            // closed — then close the endpoint.
            let _ = tokio::time::timeout(drain_timeout, endpoint.wait_idle()).await;
            endpoint.close(0u32.into(), b"relay shutting down");
        });

        Ok((handle, local_addr))
    }
}

/// Where a stream came from, as far as this node can know.
///
/// `ip` is the address of the QUIC peer that carried it — the client for a
/// direct stream, the previous relay (or whoever *claims* to be one, since an
/// opening `Layer` is unauthenticated) for a layered one. Every stream has one,
/// and every admission keys on it: there is no way to reach the rate limiter
/// without an address, so no stream escapes the per-IP bounds by the frame it
/// opens with.
///
/// `connection` is the QUIC connection itself, and only a stream straight off
/// this node's endpoint has it. A layered stream gets `None` — which is what
/// makes "a `Park` cannot be smuggled through a layer" structural rather than
/// remembered: parking needs the connection handle, and there is none.
struct StreamOrigin {
    ip: IpAddr,
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

/// How many bi-streams one connection may hold open at once. One is what a
/// client, a previous relay's extend leg, or a parking peer uses; the rest is
/// slack for a stream that is being torn down while its successor opens.
const MAX_BIDI_STREAMS_PER_CONNECTION: u32 = 4;

/// The instant by which a stream must have finished negotiating.
type Deadline = tokio::time::Instant;

/// Run one read of the negotiation under the stream's deadline. A timeout is
/// reported as an I/O error, which every caller already treats as a malformed
/// exchange — a stream that says nothing is refused the same way as one that
/// says something wrong.
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
    // Each target gets its own bi-stream; serve them until the peer goes away.
    while let Ok((send, recv)) = connection.accept_bi().await {
        let inner = inner.clone();
        let origin = StreamOrigin {
            ip: peer.ip(),
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

/// Serve one bi-stream straight off the QUIC endpoint. This is the only place a
/// `Layer` frame is legal: it means the peer says it is a relay extending a
/// circuit into us. It is not believed on that — the stream inside the layer is
/// limited under this connection's address like any other.
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
            // Acknowledge to the *previous relay* — this response never reaches
            // the client, which is still waiting on that relay's own Ok — then
            // peel the layer and serve what's inside.
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

/// Read the handshake body a caller has already committed to, then serve what
/// follows it. `origin.connection` is `Some` only for a stream straight off this
/// node's QUIC endpoint; a layered one carries the address but not the handle.
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

/// Count one refusal and tell the client why. Every rejection path does exactly
/// this pair, and the counter has to move in lockstep with the response — a
/// `rejected` that drifts from what clients were actually told is worse than no
/// counter at all. The write is best-effort: a peer that has already gone away
/// cannot be told anything, and that is not this node's problem.
async fn reject<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    stats: &RelayStats,
    reason: RejectReason,
    version: u8,
) {
    stats.rejected.fetch_add(1, Ordering::Relaxed);
    let _ = proto::write_response(stream, Err(reason), version).await;
}

/// Holds [`RelayStats::live_circuits`] up for as long as one admitted stream is
/// being served, and puts it back down however that stream ends — clean close,
/// error, or dropped future. An explicit decrement at each `return` would be one
/// forgotten branch away from a gauge that only climbs.
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

/// Verify the device-cert handshake, admit under the rate limits, then serve the
/// single command that follows. `origin.connection` is `Some` only when the
/// stream came straight off this node's QUIC endpoint rather than through an
/// onion layer; `origin.ip` is always the address the limits key on.
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

    // 2. Rate / concurrency limits, keyed on the authenticated account AND the
    //    address of the QUIC peer that carried this stream (§11.5) — a client's,
    //    or the previous relay's for a layered stream. The guard frees the
    //    concurrency slots when this stream ends.
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

    // 3. An OPTIONAL transparency-log account anchor (v4+, #813 Phase E2), then
    //    exactly one terminal frame: a command (terminate here, or forward to the
    //    next relay) or a park (offer this connection as a middle hop).
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
            // A second anchor on one stream is not a legitimate shape — it would
            // only ever be an attempt to have a later one overwrite an earlier
            // verdict — so it is a malformed exchange, not a re-check.
            ClientFrame::Anchor(_) if anchored => {
                reject(&mut stream, &inner.stats, RejectReason::BadRequest, version).await;
                return Ok(());
            }
            ClientFrame::Anchor(anchor) => {
                anchored = true;
                // A node with no pinned log key has no basis to judge an anchor,
                // so it does not pretend to: it neither accepts nor rejects on
                // it. A node that HAS one treats a bad anchor exactly like a
                // forged cert — presenting one is never better than presenting
                // none.
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

    // A node that REQUIRES an anchor refuses a client that presented none. A v3
    // client cannot present one at all, so it gets the reason code its generation
    // understands rather than one that would decode as `Internal`.
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
            // A parked connection is not a circuit — it carries none until an
            // `Extend` is spliced into it, and each of those is counted where it
            // is served.
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
/// Registration lasts exactly as long as this stream: the peer unparks by
/// closing it, a dead connection ends it, and either way the guard drops and the
/// entry goes. Nothing is written back afterwards and nothing may be read — a
/// parked connection is kept alive by QUIC keep-alives, not by traffic here.
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

    // Acknowledged with the ordinary `Ok` response: no second codec, and a
    // refusal above already reported a typed reason on the same frame.
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

    // Allowlist — the closed-overlay guarantee (design §1.2). It is enforced
    // wherever the `Connect` lands, which is by construction the last hop, and
    // it binds host AND port: an allowlisted host is one service, not every
    // service that happens to run on that machine.
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

    // Tell the client the pipe is live, then splice bytes both ways. The QUIC
    // connection stays alive in the accept loop, so the stream doesn't need to
    // own it here.
    proto::write_response(&mut stream, Ok(()), version).await?;
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
    Ok(())
}

/// The next hop's identity as far as it is knowable **before** its QUIC
/// handshake: its advertised address, and no cert, because the DER leaf does not
/// exist until the peer presents it.
///
/// So only the `addr` selector can match here. That is not the whole check — the
/// full identity, including the DER the peer actually presented, is re-checked in
/// [`open_next_hop`] before a single byte is spliced. Passing an empty DER can
/// only ever fail *closed* (it hashes to a digest no relay cert has), never open.
fn predial_identity(addr: &str) -> RelayIdentity<'_> {
    RelayIdentity {
        addr,
        cert_der: &[],
    }
}

/// Middle hop: open the next leg, announce the layer, splice. Everything that
/// crosses this splice afterwards is addressed to a hop further along and is
/// unreadable here — which is the entire point of the frame.
///
/// The next leg is opened one of two ways, and which one is invisible to every
/// other party: a **dial** to the address the frame names, or — when the frame's
/// fingerprint matches a peer parked here — a **splice into the connection that
/// peer already opened** ([`crate::park`]). A parked peer is unreachable by dial,
/// so the address a client sends for one carries no weight; the fingerprint is
/// what selects the hop, and the pinned TLS handshake is what proves it.
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

    // Live revocation (#813 Phase C), BEFORE the dial or the splice, so a seized
    // next hop is never even contacted. `admitted()` is the single fail-closed
    // encoding: `Unevaluable` — no list held, expired at USE time, or no
    // directory key configured on this node — resolves identically to `Revoked`.
    // A node that cannot evaluate revocation does not get to guess, and parking
    // buys no exemption: a parked peer is checked here exactly as a dialled one
    // is, on MORE of its identity, because its DER leaf is already in hand.
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
    // `_tunnel` is held for the life of the splice: dropping the last handle
    // tears down the loopback pump the tunnelled QUIC is riding on.
    let (mut next, _tunnel) = match open_extend_leg(&inner, &extend, parked).await {
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
/// Both paths end in the same [`open_next_hop`] — same pinned fingerprint, same
/// post-handshake revocation re-check, same `Layer` announcement — because the
/// only thing parking changes is which address carries the packets.
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
/// The QUIC hop is pinned to the exact cert fingerprint the `Extend` carried, so
/// this is not a "connect anywhere" primitive; and it offers only the current
/// ALPN, so a node that does not speak the onion generation fails negotiation
/// rather than receiving something it would misparse.
///
/// `dial_addr` is the address the frame named for an ordinary next hop, and a
/// **loopback tunnel** address for a parked peer. That the pin is applied
/// identically either way is what makes parking safe without a new credential:
/// a device that parked under a leaf it does not hold the key for cannot
/// complete this handshake, so it can deny that peer's traffic but never carry
/// or read it.
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

    // Now the peer's DER leaf exists, so the revocation check can run on the FULL
    // identity — address *and* cert digest — not just the address. The cert
    // selector is the one that binds a per-node or peer-hosted relay identity
    // (§7 / phase D1), where every node no longer shares one pooled leaf. A peer
    // that will not say what cert it presented cannot be evaluated, and
    // "cannot evaluate" is not an admission.
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

    // Announce the layer and wait for the next hop to acknowledge before telling
    // our own client the circuit is live — so a failure here surfaces as a clean
    // rejection instead of a stream that silently goes nowhere.
    proto::write_layer(&mut next, proto::PROTOCOL_VERSION).await?;
    proto::read_response(&mut next).await?;
    Ok(next)
}

/// The DER leaf certificate the peer of an outbound QUIC connection presented.
///
/// `None` when the transport cannot report one, which callers must treat as
/// "cannot evaluate this peer's identity" — never as "no objection".
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
