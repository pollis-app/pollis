//! Closed-overlay relay wiring for `pollis-core` (design
//! `docs/relay-overlay-design.md` §14). This is the CONSUMER side of the
//! `pollis-relay` transport crate: it derives the routing policy from `Config`,
//! starts the loopback SOCKS5 shim, hands out the shared reqwest client, and
//! builds the libsql SOCKS connector.
//!
//! **Off-by-default is sacred.** With `POLLIS_OVERLAY` unset (`OverlayMode::Off`)
//! [`start_overlay`] returns `None`, `AppState.overlay` stays `None`, and every
//! network path is byte-for-byte identical to a build without the overlay:
//! [`http_client`] returns a proxy-less `reqwest::Client`, and `RemoteDb::connect`
//! takes libsql's unchanged `.build()` path (no `.connector()`).

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use hyper::client::connect::{Connected, Connection};
use hyper::Uri;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tower_service::Service;

use pollis_relay::circuit::CircuitFactory;
use pollis_relay::stream::BoxedStream;
use pollis_relay::{Allowlist, OverlayHandle, OverlayMode, OverlayShim, RoutingPolicy};

use crate::config::Config;
use crate::error::{Error, Result};

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

/// Build the per-host routing policy from `Config`: the first-party control
/// plane (Turso + optional commit-log DB, the DS, R2) routes through the overlay;
/// LiveKit stays DIRECT in every mode (the media plane, §6.4). Any host not on
/// either list is dialed direct (e.g. non-first-party Expo push, §14.4).
pub(crate) fn overlay_policy(config: &Config) -> RoutingPolicy {
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
        config.overlay_mode,
        Allowlist::from_patterns(overlay_hosts),
        Allowlist::from_patterns(direct_hosts),
    )
}

// ── Shim startup (design §9.2, §14.1) ──────────────────────────────────────

/// A circuit factory that never yields a circuit.
///
/// Slice 1b wires the transport, but a real single-hop circuit needs the
/// deployed relay's pinned cert AND the logged-in device's Ed25519 key — neither
/// exists at `AppState::new` time (no user is logged in; no relay is deployed).
/// Until that provisioning lands (Slice 2), the factory fails fast, so `Prefer`
/// falls back to a direct dial and `Strict` surfaces a degraded error — never a
/// silent direct send (messages-must-work, §7/§10.1). The shim itself still runs
/// whenever the mode is non-off, which is exactly what makes `Strict` degrade
/// instead of silently going direct.
struct PendingRelayFactory;

#[async_trait::async_trait]
impl CircuitFactory for PendingRelayFactory {
    async fn connect(&self, _host: &str, _port: u16) -> anyhow::Result<BoxedStream> {
        anyhow::bail!(
            "overlay circuit unavailable: relay cert + device-identity provisioning \
             lands in a later slice"
        )
    }
}

/// Start the overlay shim for this app, or `None` when the overlay is off.
///
/// - `OverlayMode::Off` → `None`: the shim never binds and the app is byte-for-byte
///   identical to a build without the overlay.
/// - `Prefer` / `Strict` → start the loopback SOCKS5 shim once and return its
///   handle. The handle owns the shim task, so it lives for the app's lifetime and
///   is aborted cleanly on drop (`AppState.overlay`).
pub(crate) async fn start_overlay(config: &Config) -> Option<OverlayHandle> {
    if config.overlay_mode == OverlayMode::Off {
        return None;
    }

    let policy = overlay_policy(config);
    let factory: Arc<dyn CircuitFactory> = Arc::new(PendingRelayFactory);
    match OverlayShim::start(policy, factory).await {
        Ok(handle) => {
            let relay = config
                .overlay_relay_url
                .as_deref()
                .unwrap_or("<none configured>");
            eprintln!(
                "[overlay] shim on {} (mode={:?}, relay={relay})",
                handle.socks_addr(),
                config.overlay_mode
            );
            Some(handle)
        }
        Err(e) => {
            eprintln!("[overlay] failed to start shim: {e}");
            None
        }
    }
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
    use pollis_relay::proto::InMemoryKeyResolver;
    use pollis_relay::server::{Allowlist as RelayAllowlist, RelayConfig, RelayServer, RelayStats};
    use rustls::pki_types::{CertificateDer, ServerName};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const USER: &str = "u_overlay_test";
    const DEVICE: &str = "d_overlay_test";
    const ORIGIN_NAME: &str = "origin.test";

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[11u8; 32])
    }

    fn identity() -> Arc<ClientIdentity> {
        Arc::new(ClientIdentity::new(USER, DEVICE, signing_key()))
    }

    fn resolver() -> Arc<InMemoryKeyResolver> {
        let mut r = InMemoryKeyResolver::new();
        r.insert(USER, DEVICE, signing_key().verifying_key());
        Arc::new(r)
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
            seal_sender: false,
            overlay_mode: mode,
            overlay_relay_url: relay.map(|s| s.to_string()),
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
            resolver(),
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
        let policy = overlay_policy(&cfg(OverlayMode::Prefer, None));
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

    #[tokio::test]
    async fn off_mode_starts_no_shim() {
        assert!(start_overlay(&cfg(OverlayMode::Off, None)).await.is_none());
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
        let shim = OverlayShim::start(policy, Arc::new(PendingRelayFactory))
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

    /// Strict + non-off mode with no relay must surface a degraded error rather
    /// than silently dialing direct (messages-must-work). The shim runs (so the
    /// mode is honored) but every control-plane CONNECT fails.
    #[tokio::test]
    async fn strict_without_relay_degrades_not_silent_direct() {
        let addr = spawn_plain_http("must-not-be-reached").await;
        let host = addr.ip().to_string();
        let shim = start_overlay(&{
            let mut c = cfg(OverlayMode::Strict, None);
            // Route the echo host as control-plane so Strict applies.
            c.turso_url = format!("libsql://{host}");
            c
        })
        .await
        .expect("Strict starts the shim even with no relay configured");

        let connector = SocksConnector::new(shim.socks_addr());
        let uri: Uri = format!("https://{host}:{}", addr.port()).parse().unwrap();
        let mut connector = connector;
        let result = Service::call(&mut connector, uri).await;
        assert!(result.is_err(), "Strict + no relay must degrade, never silent-direct");
        drop(shim);
    }
}
