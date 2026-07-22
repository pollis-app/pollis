//! The minimal relay server: QUIC in, allowlisted TCP dial out, bytes piped.
//!
//! Per accepted bi-stream the relay runs the OFFLINE device-cert handshake
//! (design §9.4 — no Turso query, no network call: the client presents its cert
//! chain and the relay verifies it locally, which is what keeps the relay tier
//! out of the metadata plane, §11.1), applies per-account / per-IP rate limits
//! (§11.5), reads the `Connect` frame, checks the host against the **static
//! allowlist** (policy, not protocol — design §14.0), TCP-dials the target, and
//! pipes bytes until either side closes. It never terminates the inner TLS — it
//! forwards opaque bytes only (design §8).
//!
//! Deployability (Slice 2a): a global concurrent-connection cap, in-memory rate
//! limiting, and **graceful shutdown** (stop accepting, drain in-flight pipes to
//! a bounded deadline, exit) via [`RelayServer::spawn_with_shutdown`].

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::QuicServerConfig;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

use crate::proto::{self, Connect, Handshake, RejectReason};
use crate::ratelimit::{RateLimitConfig, RateLimiter};
use crate::stream::RelayStream;
use crate::tls::{self, SelfSignedIdentity};

/// A destination-host matcher. The allowlist is relay-side *policy*: the wire
/// protocol carries an arbitrary host, and the relay decides what it will dial.
#[derive(Debug, Clone)]
pub enum HostPattern {
    /// Exact host match (case-insensitive).
    Exact(String),
    /// Suffix match for a `*.example.com` glob — stored as `.example.com`.
    Suffix(String),
    /// Matches any host. Use only for a fully open relay (not first-party v0).
    Any,
}

impl HostPattern {
    /// Parse one allowlist entry: `*` → any, `*.foo` → suffix, else exact.
    pub fn parse(s: &str) -> HostPattern {
        if s == "*" {
            HostPattern::Any
        } else if let Some(rest) = s.strip_prefix("*.") {
            HostPattern::Suffix(format!(".{}", rest.to_ascii_lowercase()))
        } else {
            HostPattern::Exact(s.to_ascii_lowercase())
        }
    }

    pub(crate) fn matches(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        match self {
            HostPattern::Any => true,
            HostPattern::Exact(h) => *h == host,
            HostPattern::Suffix(suffix) => host.ends_with(suffix.as_str()),
        }
    }
}

/// The relay's static destination allowlist.
#[derive(Debug, Clone, Default)]
pub struct Allowlist(Vec<HostPattern>);

impl Allowlist {
    /// Build from raw patterns (`turso.io`, `*.pollis.com`, `*`).
    pub fn from_patterns<I, S>(patterns: I) -> Allowlist
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Allowlist(patterns.into_iter().map(|p| HostPattern::parse(p.as_ref())).collect())
    }

    /// True if `host` is permitted. An empty allowlist permits nothing.
    pub fn permits(&self, host: &str) -> bool {
        self.0.iter().any(|p| p.matches(host))
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
}

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
    /// Global cap on simultaneously-open QUIC connections.
    pub max_concurrent_connections: u32,
    /// Shared counters.
    pub stats: Arc<RelayStats>,
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
            stats: Arc::new(RelayStats::default()),
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
    stats: Arc<RelayStats>,
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
        server_crypto.alpn_protocols = vec![proto::ALPN.to_vec()];

        let quic_crypto = QuicServerConfig::try_from(server_crypto)?;
        let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));

        let endpoint = quinn::Endpoint::server(server_config, config.bind)?;
        let local_addr = endpoint.local_addr()?;

        let inner = Arc::new(RelayInner {
            allowlist: config.allowlist,
            resolve_overrides: config.resolve_overrides,
            rate_limiter: RateLimiter::new(config.rate_limits),
            stats: config.stats,
        });
        let max_conns = config.max_concurrent_connections.max(1) as u64;
        let live_conns = Arc::new(AtomicU64::new(0));

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
                        // Global concurrent-connection cap: shed load cleanly.
                        if live_conns.load(Ordering::Relaxed) >= max_conns {
                            incoming.refuse();
                            continue;
                        }
                        live_conns.fetch_add(1, Ordering::Relaxed);
                        let inner = inner.clone();
                        let live = live_conns.clone();
                        tokio::spawn(async move {
                            match incoming.await {
                                Ok(connection) => handle_connection(connection, inner).await,
                                Err(e) => tracing::debug!("relay: connection setup failed: {e}"),
                            }
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

async fn handle_connection(connection: quinn::Connection, inner: Arc<RelayInner>) {
    let peer_ip = connection.remote_address().ip();
    // Each target gets its own bi-stream; serve them until the peer goes away.
    while let Ok((send, recv)) = connection.accept_bi().await {
        let inner = inner.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_stream(send, recv, inner, peer_ip).await {
                tracing::debug!("relay: stream ended: {e}");
            }
        });
    }
}

async fn handle_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    inner: Arc<RelayInner>,
    peer_ip: IpAddr,
) -> anyhow::Result<()> {
    // 1. Handshake — reject on any auth failure, never fail open.
    let handshake: Handshake = match proto::read_handshake(&mut recv).await {
        Ok(h) => h,
        Err(_) => {
            inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
            let _ = proto::write_response(&mut send, Err(RejectReason::BadRequest)).await;
            return Ok(());
        }
    };
    let verified = match proto::verify_handshake(&handshake, proto::now_unix()) {
        Ok(v) => v,
        Err(reason) => {
            inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
            let _ = proto::write_response(&mut send, Err(reason)).await;
            return Ok(());
        }
    };
    inner.stats.authorized.fetch_add(1, Ordering::Relaxed);

    // 2. Rate / concurrency limits, keyed on BOTH the source IP and the
    //    authenticated account (§11.5). The guard frees the concurrency slots
    //    when this stream ends.
    let _circuit_guard = match inner.rate_limiter.admit(peer_ip, verified.account_id_pub) {
        Some(g) => g,
        None => {
            inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
            inner.stats.rate_limited.fetch_add(1, Ordering::Relaxed);
            let _ = proto::write_response(&mut send, Err(RejectReason::RateLimited)).await;
            return Ok(());
        }
    };

    // 3. Connect frame.
    let connect: Connect = match proto::read_connect(&mut recv).await {
        Ok(c) => c,
        Err(_) => {
            let _ = proto::write_response(&mut send, Err(RejectReason::BadRequest)).await;
            return Ok(());
        }
    };

    // 4. Allowlist — the closed-overlay guarantee (design §1.2).
    if !inner.allowlist.permits(&connect.host) {
        inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
        let _ = proto::write_response(&mut send, Err(RejectReason::NotAllowed)).await;
        return Ok(());
    }

    // 5. Dial the target (applying any host→IP override).
    let tcp = match dial_target(&connect, &inner.resolve_overrides).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("relay: dial {}:{} failed: {e}", connect.host, connect.port);
            let _ = proto::write_response(&mut send, Err(RejectReason::DialFailed)).await;
            return Ok(());
        }
    };
    inner.stats.dials.fetch_add(1, Ordering::Relaxed);

    // 6. Tell the client the pipe is live, then splice bytes both ways. The
    //    QUIC connection stays alive in the accept loop, so the stream doesn't
    //    need to own it here.
    proto::write_response(&mut send, Ok(())).await?;

    let mut relay_stream = RelayStream::new(send, recv, None, None);
    let mut tcp = tcp;
    let _ = tokio::io::copy_bidirectional(&mut relay_stream, &mut tcp).await;
    Ok(())
}

/// Resolve `connect.host` (honoring overrides) and open a TCP connection.
async fn dial_target(connect: &Connect, overrides: &HashMap<String, IpAddr>) -> std::io::Result<TcpStream> {
    if let Some(ip) = overrides.get(&connect.host) {
        return TcpStream::connect(SocketAddr::new(*ip, connect.port)).await;
    }
    TcpStream::connect((connect.host.as_str(), connect.port)).await
}
