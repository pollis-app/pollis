//! Proof / de-risk tests for the closed-overlay relay v0 (design §14.3 gate).
//!
//! Everything runs headless on loopback — no network egress, deterministic
//! signing keys, and certs generated in-test.
//!
//! - T1: reqwest → SOCKS5 shim → relay → local TLS origin; body round-trips and
//!   the relay actually dialed the origin (traffic traversed the hop). Inner TLS
//!   is verified for the *real* name `origin.test`.
//! - T2: rustls-over-the-shim (a SOCKS-dialing connector under TLS) reaches the
//!   origin with SNI/cert intact — de-risks the libsql `.connector()` shape.
//! - T3: a `Connect` to a non-allowlisted host is refused (NotAllowed); the SOCKS
//!   client sees a clean failure and the relay never dials.
//! - T4: missing / forged / expired / unknown device signatures are rejected; a
//!   valid one is accepted.
//! - T5: policy — Off→direct; Strict+no-relay→degraded; Prefer+no-relay→direct
//!   fallback; a media host is direct in every mode.
//! - T6: `http_client(None)` is a plain direct client — the overlay is inert.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use pollis_relay::client::{ClientIdentity, RelayClient};
use pollis_relay::circuit::{CircuitFactory, Hop, SingleHopFactory};
use pollis_relay::policy::{FinalAction, OverlayMode, PlannedRoute, RoutingPolicy};
use pollis_relay::proto::{self, Handshake, InMemoryKeyResolver, RejectReason};
use pollis_relay::server::{Allowlist, RelayConfig, RelayServer, RelayStats};
use pollis_relay::shim::OverlayShim;
use pollis_relay::stream::BoxedStream;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const USER: &str = "u_test";
const DEVICE: &str = "d_test";
const ORIGIN_NAME: &str = "origin.test";

fn client_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// A resolver that knows the one authorized test device.
fn authorized_resolver() -> Arc<InMemoryKeyResolver> {
    let mut resolver = InMemoryKeyResolver::new();
    resolver.insert(USER, DEVICE, client_signing_key().verifying_key());
    Arc::new(resolver)
}

fn client_identity() -> Arc<ClientIdentity> {
    Arc::new(ClientIdentity::new(USER, DEVICE, client_signing_key()))
}

// ---- test infrastructure ---------------------------------------------------

/// A running relay plus what a client needs to reach it.
struct TestRelay {
    addr: SocketAddr,
    cert: CertificateDer<'static>,
    stats: Arc<RelayStats>,
    _task: tokio::task::JoinHandle<()>,
}

/// Spawn a relay allowing `allow` hosts, with `overrides` mapping a DNS name to
/// a loopback IP so a "real" name can be dialed on 127.0.0.1.
fn spawn_relay(allow: &[&str], overrides: &[(&str, IpAddr)]) -> TestRelay {
    let mut config = RelayConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        Allowlist::from_patterns(allow.iter().copied()),
        authorized_resolver(),
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
    fn hop(&self) -> Hop {
        Hop::new(self.addr, self.cert.clone())
    }
    fn factory(&self) -> Arc<dyn CircuitFactory> {
        Arc::new(SingleHopFactory::new(self.hop(), client_identity()))
    }
}

/// A factory with no relay — every overlay attempt fails.
struct NoRelayFactory;

#[async_trait::async_trait]
impl CircuitFactory for NoRelayFactory {
    async fn connect(&self, _host: &str, _port: u16) -> anyhow::Result<BoxedStream> {
        anyhow::bail!("no relay available")
    }
}

fn policy(mode: OverlayMode, overlay: &[&str], direct: &[&str]) -> RoutingPolicy {
    RoutingPolicy::new(
        mode,
        Allowlist::from_patterns(overlay.iter().copied()),
        Allowlist::from_patterns(direct.iter().copied()),
    )
}

/// A loopback TCP echo server. Returns its address and a per-connection counter.
async fn spawn_echo() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            c.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    (addr, count)
}

/// A loopback TLS "origin" for `origin.test`. Returns its address, the CA the
/// client must trust, and a counter of accepted connections.
async fn spawn_tls_origin(body: &'static str) -> (SocketAddr, CertificateDer<'static>, Arc<AtomicUsize>) {
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
        loop {
            let (tcp, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let acceptor = acceptor.clone();
            let c = c.clone();
            tokio::spawn(async move {
                let mut tls = match acceptor.accept(tcp).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                c.fetch_add(1, Ordering::Relaxed);
                // Read the request headers, then reply with a fixed 200.
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

/// Read bytes until the end of the HTTP request head (`\r\n\r\n`).
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

// ---- T1 --------------------------------------------------------------------

#[tokio::test]
async fn t1_reqwest_through_overlay_to_tls_origin() {
    let (origin_addr, ca, origin_conns) = spawn_tls_origin("hello-through-the-relay").await;
    let relay = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);

    let shim = OverlayShim::start(policy(OverlayMode::Strict, &[ORIGIN_NAME], &[]), relay.factory())
        .await
        .unwrap();

    // The reqwest client trusts the test CA and routes through the shim. This is
    // the production proxy path (http_client_builder) plus a test root; the
    // proxy wiring is identical to `http_client(Some(&shim))`.
    let client = pollis_relay::http::http_client_builder(Some(&shim))
        .add_root_certificate(reqwest::Certificate::from_der(&ca).unwrap())
        .build()
        .unwrap();

    let url = format!("https://{ORIGIN_NAME}:{}/", origin_addr.port());
    let resp = client.get(&url).send().await.expect("request through overlay");
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert_eq!(text, "hello-through-the-relay");

    // Traffic actually traversed the hop: the relay dialed the origin, and the
    // origin saw exactly that one connection.
    assert_eq!(relay.stats.dials(), 1, "relay must have dialed the origin");
    assert_eq!(origin_conns.load(Ordering::Relaxed), 1);
    drop(shim);
}

// ---- T2 --------------------------------------------------------------------

#[tokio::test]
async fn t2_rustls_connector_through_overlay() {
    let (origin_addr, ca, _c) = spawn_tls_origin("libsql-connector-shape").await;
    let relay = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);
    let shim = OverlayShim::start(policy(OverlayMode::Strict, &[ORIGIN_NAME], &[]), relay.factory())
        .await
        .unwrap();

    // A SOCKS-dialing connector: dial the shim (proxy-side DNS keeps the real
    // hostname), then run rustls over it verifying the cert for `origin.test`.
    let target = (ORIGIN_NAME, origin_addr.port());
    let socks = tokio_socks::tcp::Socks5Stream::connect(shim.socks_addr(), target)
        .await
        .expect("SOCKS connect through shim");

    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    let mut client_cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));

    let server_name = ServerName::try_from(ORIGIN_NAME).unwrap();
    let mut tls = connector
        .connect(server_name, socks)
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
    drop(shim);
}

// ---- T3 --------------------------------------------------------------------

#[tokio::test]
async fn t3_allowlist_rejects_unlisted_host() {
    // Relay allows only origin.test; the shim routes blocked.test to overlay.
    let relay = spawn_relay(&[ORIGIN_NAME], &[]);
    let shim = OverlayShim::start(policy(OverlayMode::Strict, &["blocked.test"], &[]), relay.factory())
        .await
        .unwrap();

    let result =
        tokio_socks::tcp::Socks5Stream::connect(shim.socks_addr(), ("blocked.test", 443u16)).await;
    assert!(result.is_err(), "SOCKS connect to a non-allowlisted host must fail cleanly");

    // The relay authorized the device, refused the host, and never dialed.
    assert_eq!(relay.stats.dials(), 0, "relay must not dial a non-allowlisted host");
    assert!(relay.stats.rejected() >= 1, "relay must record the rejection");
    drop(shim);
}

// ---- T4 --------------------------------------------------------------------

#[test]
fn t4_handshake_verification_unit() {
    let resolver = authorized_resolver();
    let key = client_signing_key();
    let now = proto::now_unix();

    // Valid.
    let good = proto::sign_handshake(&key, USER, DEVICE, now, [1u8; 32]);
    assert_eq!(proto::verify_handshake(resolver.as_ref(), &good, now).unwrap(), USER);

    // Expired (outside the skew window).
    let expired = proto::sign_handshake(&key, USER, DEVICE, now - 10_000, [1u8; 32]);
    assert_eq!(
        proto::verify_handshake(resolver.as_ref(), &expired, now).unwrap_err(),
        RejectReason::Unauthorized
    );

    // Forged: flip a signature byte.
    let mut forged = good.clone();
    forged.signature[0] ^= 0xFF;
    assert_eq!(
        proto::verify_handshake(resolver.as_ref(), &forged, now).unwrap_err(),
        RejectReason::Unauthorized
    );

    // Unknown device: empty resolver.
    let empty = InMemoryKeyResolver::new();
    assert_eq!(
        proto::verify_handshake(&empty, &good, now).unwrap_err(),
        RejectReason::Unauthorized
    );

    // Missing identity fields.
    let blank = Handshake { user_id: String::new(), ..good.clone() };
    assert_eq!(
        proto::verify_handshake(resolver.as_ref(), &blank, now).unwrap_err(),
        RejectReason::Unauthorized
    );
}

#[tokio::test]
async fn t4_relay_accepts_valid_rejects_forged() {
    let (echo_addr, _c) = spawn_echo().await;
    let allow = [echo_addr.ip().to_string()];
    let relay = spawn_relay(&allow.iter().map(|s| s.as_str()).collect::<Vec<_>>(), &[]);

    // Valid device → stream opens and echoes.
    let mut stream = RelayClient::connect(
        relay.addr,
        &relay.cert,
        &client_identity(),
        &echo_addr.ip().to_string(),
        echo_addr.port(),
    )
    .await
    .expect("valid handshake accepted");
    stream.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ping");
    assert_eq!(relay.stats.dials(), 1);

    // Wrong key (not in the resolver) → rejected, no dial.
    let bad_identity = Arc::new(ClientIdentity::new(USER, DEVICE, SigningKey::from_bytes(&[9u8; 32])));
    let err = RelayClient::connect(
        relay.addr,
        &relay.cert,
        &bad_identity,
        &echo_addr.ip().to_string(),
        echo_addr.port(),
    )
    .await;
    assert!(err.is_err(), "forged device signature must be rejected");
    assert_eq!(relay.stats.dials(), 1, "rejected client must not cause a dial");
    assert!(relay.stats.rejected() >= 1);
}

// ---- T5 --------------------------------------------------------------------

#[test]
fn t5_policy_pure() {
    // Off → everything direct.
    let off = policy(OverlayMode::Off, &["control.test"], &["media.test"]);
    assert_eq!(off.plan("control.test"), PlannedRoute::Direct);

    // Media host is direct in every mode.
    for mode in [OverlayMode::Off, OverlayMode::Prefer, OverlayMode::Strict] {
        let p = policy(mode, &["control.test"], &["media.test"]);
        assert_eq!(p.plan("media.test"), PlannedRoute::Direct, "media must be direct in {mode:?}");
    }

    // Prefer → overlay with direct fallback; Strict → overlay, degrade on fail.
    let prefer = policy(OverlayMode::Prefer, &["control.test"], &[]);
    assert_eq!(prefer.plan("control.test"), PlannedRoute::Overlay { fallback_to_direct: true });
    let strict = policy(OverlayMode::Strict, &["control.test"], &[]);
    assert_eq!(strict.plan("control.test"), PlannedRoute::Overlay { fallback_to_direct: false });

    // Reconcile: Strict + failed overlay → Degraded (never silent direct).
    assert_eq!(
        RoutingPolicy::reconcile(PlannedRoute::Overlay { fallback_to_direct: false }, Some(false)),
        FinalAction::Degraded
    );
    // Prefer + failed overlay → Direct.
    assert_eq!(
        RoutingPolicy::reconcile(PlannedRoute::Overlay { fallback_to_direct: true }, Some(false)),
        FinalAction::Direct
    );
    // Succeeded overlay → Overlay.
    assert_eq!(
        RoutingPolicy::reconcile(PlannedRoute::Overlay { fallback_to_direct: false }, Some(true)),
        FinalAction::Overlay
    );
}

#[tokio::test]
async fn t5_strict_no_relay_is_degraded() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    // Strict, host routed to overlay, but no relay behind the factory.
    let shim = OverlayShim::start(
        policy(OverlayMode::Strict, &[host.as_str()], &[]),
        Arc::new(NoRelayFactory),
    )
    .await
    .unwrap();

    let result = tokio_socks::tcp::Socks5Stream::connect(shim.socks_addr(), (host.as_str(), echo_addr.port())).await;
    assert!(result.is_err(), "Strict + no relay must surface a degraded error");
    // And it must NOT have silently fallen back to a direct dial.
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0, "Strict must not fall back to direct");
    drop(shim);
}

#[tokio::test]
async fn t5_prefer_no_relay_falls_back_to_direct() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    // Prefer, host routed to overlay, no relay → should fall back to a direct dial.
    let shim = OverlayShim::start(
        policy(OverlayMode::Prefer, &[host.as_str()], &[]),
        Arc::new(NoRelayFactory),
    )
    .await
    .unwrap();

    let mut sock = tokio_socks::tcp::Socks5Stream::connect(shim.socks_addr(), (host.as_str(), echo_addr.port()))
        .await
        .expect("Prefer must fall back to a direct dial");
    sock.write_all(b"pong").await.unwrap();
    let mut buf = [0u8; 4];
    sock.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"pong");
    assert_eq!(echo_conns.load(Ordering::Relaxed), 1, "direct fallback must reach the target");
    drop(shim);
}

// ---- T6 --------------------------------------------------------------------

/// A plain (non-TLS) loopback HTTP/1.1 server that never involves the overlay.
async fn spawn_plain_http(body: &'static str) -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
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

#[tokio::test]
async fn t6_off_mode_client_is_direct() {
    let addr = spawn_plain_http("direct-and-inert").await;
    // http_client(None): no proxy, behaviorally a direct dial.
    let client = pollis_relay::http::http_client(None);
    let resp = client
        .get(format!("http://{addr}/"))
        .send()
        .await
        .expect("direct request with the overlay off");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "direct-and-inert");
}
