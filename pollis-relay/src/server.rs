//! The minimal relay server: QUIC in, allowlisted TCP dial out, bytes piped.
//!
//! Per accepted bi-stream the relay runs the device-signature handshake, reads
//! the `Connect` frame, checks the host against the **static allowlist** (policy,
//! not protocol — design §14.0), TCP-dials the target, and pipes bytes until
//! either side closes. It never terminates the inner TLS — it forwards opaque
//! bytes only (design §8).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use quinn::crypto::rustls::QuicServerConfig;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

use crate::proto::{
    self, Connect, Handshake, KeyResolver, RejectReason,
};
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
    /// Streams rejected before a dial (auth or allowlist).
    pub rejected: AtomicU64,
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
    /// Device-key lookup for the handshake.
    pub key_resolver: Arc<dyn KeyResolver>,
    /// The relay's own QUIC identity (self-signed). Callers keep the cert to pin
    /// it client-side.
    pub identity: SelfSignedIdentity,
    /// Host → IP overrides applied before dialing. Lets tests point a real DNS
    /// name (e.g. `origin.test`) at a loopback listener; also a pinning hook.
    pub resolve_overrides: HashMap<String, IpAddr>,
    /// Shared counters.
    pub stats: Arc<RelayStats>,
}

impl RelayConfig {
    /// Build a config, auto-generating a fresh self-signed QUIC identity.
    pub fn new(bind: SocketAddr, allowlist: Allowlist, key_resolver: Arc<dyn KeyResolver>) -> anyhow::Result<Self> {
        let identity = tls::generate_self_signed(tls::RELAY_SERVER_NAME)?;
        Ok(RelayConfig {
            bind,
            allowlist,
            key_resolver,
            identity,
            resolve_overrides: HashMap::new(),
            stats: Arc::new(RelayStats::default()),
        })
    }

    /// The DER cert a client must pin to connect to this relay.
    pub fn server_cert(&self) -> rustls::pki_types::CertificateDer<'static> {
        self.identity.cert_der.clone()
    }
}

/// Fields the per-stream handler needs, shared behind an `Arc`.
struct RelayInner {
    allowlist: Allowlist,
    key_resolver: Arc<dyn KeyResolver>,
    resolve_overrides: HashMap<String, IpAddr>,
    stats: Arc<RelayStats>,
}

/// A running relay.
pub struct RelayServer;

impl RelayServer {
    /// Spawn a relay on `config.bind`, returning the accept task and the actual
    /// bound address (useful when binding to port 0). Dropping the returned
    /// `JoinHandle` does not stop it; abort it to shut down.
    pub fn spawn(config: RelayConfig) -> anyhow::Result<(JoinHandle<()>, SocketAddr)> {
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
            key_resolver: config.key_resolver,
            resolve_overrides: config.resolve_overrides,
            stats: config.stats,
        });

        let handle = tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let inner = inner.clone();
                tokio::spawn(async move {
                    match incoming.await {
                        Ok(connection) => handle_connection(connection, inner).await,
                        Err(e) => tracing::debug!("relay: connection setup failed: {e}"),
                    }
                });
            }
        });

        Ok((handle, local_addr))
    }
}

async fn handle_connection(connection: quinn::Connection, inner: Arc<RelayInner>) {
    // Each target gets its own bi-stream; serve them until the peer goes away.
    while let Ok((send, recv)) = connection.accept_bi().await {
        let inner = inner.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_stream(send, recv, inner).await {
                tracing::debug!("relay: stream ended: {e}");
            }
        });
    }
}

async fn handle_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    inner: Arc<RelayInner>,
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
    if let Err(reason) = proto::verify_handshake(inner.key_resolver.as_ref(), &handshake, proto::now_unix()) {
        inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
        let _ = proto::write_response(&mut send, Err(reason)).await;
        return Ok(());
    }
    inner.stats.authorized.fetch_add(1, Ordering::Relaxed);

    // 2. Connect frame.
    let connect: Connect = match proto::read_connect(&mut recv).await {
        Ok(c) => c,
        Err(_) => {
            let _ = proto::write_response(&mut send, Err(RejectReason::BadRequest)).await;
            return Ok(());
        }
    };

    // 3. Allowlist — the closed-overlay guarantee (design §1.2).
    if !inner.allowlist.permits(&connect.host) {
        inner.stats.rejected.fetch_add(1, Ordering::Relaxed);
        let _ = proto::write_response(&mut send, Err(RejectReason::NotAllowed)).await;
        return Ok(());
    }

    // 4. Dial the target (applying any host→IP override).
    let tcp = match dial_target(&connect, &inner.resolve_overrides).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("relay: dial {}:{} failed: {e}", connect.host, connect.port);
            let _ = proto::write_response(&mut send, Err(RejectReason::DialFailed)).await;
            return Ok(());
        }
    };
    inner.stats.dials.fetch_add(1, Ordering::Relaxed);

    // 5. Tell the client the pipe is live, then splice bytes both ways. The
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
