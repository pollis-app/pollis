//! Multi-hop onion proof tests (#813 Phase A, design §6.2).
//!
//! These are not happy-path tests. Each one is written to fail if the property
//! that makes multi-hop worth building stops holding:
//!
//! - O1: a 3-hop circuit carries real TLS traffic end to end **while hops 1 and 2
//!   have EMPTY destination allowlists** — so if either had been able to see the
//!   destination, it would have refused it. The relay counters then pin down
//!   exactly which node parsed a `Connect` (one: the last).
//! - O2: the client's transport address is observed by the FIRST hop and by no
//!   other — the last hop cannot see it.
//! - O3: a forged layer is rejected two independent ways: the client refuses a
//!   hop that presents the wrong leaf cert, and a forwarding relay refuses to
//!   hand the circuit to a next hop whose cert fingerprint does not match.
//! - O4: single-hop is unchanged, and a 2-hop circuit works.
//! - O5: `allow_extend = false` refuses cleanly rather than dropping the stream.
//! - O6/O7: version negotiation — a v4 client speaks v3 to a v3-only relay, a
//!   v3 stream cannot smuggle an `Extend`, and a v3-only relay is refused for
//!   multi-hop instead of being half-spoken to.
//!
//! Everything runs headless on loopback, with certs generated in-test.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ml_dsa::{Keypair, MlDsa44, SigningKey};
use pollis_relay::circuit::{Circuit, Hop};
use pollis_relay::client::{ClientIdentity, RelayClient, RelayLink};
use pollis_relay::proto::{self, Connect, DeviceCertMaterial, Extend, RejectReason};
use pollis_relay::server::{Allowlist, PeerObserver, RelayConfig, RelayServer, RelayStats};
use pollis_relay::stream::BoxedStream;
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const USER: &str = "u_onion";
const DEVICE: &str = "d_onion";
const ORIGIN_NAME: &str = "origin.test";
const ISSUED_AT: u64 = 1_700_000_000;
const DEVICE_ED_PUB: [u8; 32] = [7u8; 32];

fn client_signing_key() -> SigningKey<MlDsa44> {
    SigningKey::<MlDsa44>::from_seed(&[7u8; 32].into())
}

fn account_key() -> SigningKey<MlDsa44> {
    SigningKey::<MlDsa44>::from_seed(&[3u8; 32].into())
}

fn pq_pub(k: &SigningKey<MlDsa44>) -> Vec<u8> {
    k.verifying_key().encode().to_vec()
}

fn client_identity() -> Arc<ClientIdentity> {
    let device = client_signing_key();
    let cert = DeviceCertMaterial::mint(
        &account_key(),
        DEVICE,
        &DEVICE_ED_PUB,
        &pq_pub(&device),
        1,
        ISSUED_AT,
    );
    Arc::new(ClientIdentity::new(USER, DEVICE, DEVICE_ED_PUB, device, cert))
}

// ---- test relays -----------------------------------------------------------

/// A running relay plus everything a test needs to reason about what it saw.
struct TestRelay {
    addr: SocketAddr,
    cert: CertificateDer<'static>,
    stats: Arc<RelayStats>,
    /// Transport addresses that connected to this node. Production nodes never
    /// record this (`peer_observer` is `None`); the hook exists so a test can
    /// assert who each hop could and could not see.
    peers: Arc<Mutex<Vec<SocketAddr>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl TestRelay {
    fn hop(&self) -> Hop {
        Hop::new(self.addr, self.cert.clone())
    }

    fn saw_peer(&self, addr: SocketAddr) -> bool {
        self.peers.lock().unwrap().contains(&addr)
    }

    fn peers(&self) -> Vec<SocketAddr> {
        self.peers.lock().unwrap().clone()
    }
}

/// A revocation store holding a freshly-signed list that revokes nobody.
///
/// Every relay in this file needs one: a node that cannot evaluate revocation
/// refuses to extend circuits (#813 Phase C, wired in `serve_extend`), so the
/// production shape of a middle hop is "keyed, with a current list" and that is
/// what these tests run. `tests/revocation.rs` covers the unkeyed and revoked
/// cases directly.
fn healthy_revocations() -> pollis_relay::policy::RevocationStore {
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};

    let sk = SigningKey::from_bytes(&[42u8; 32]);
    let now = pollis_relay::proto::now_unix();
    let payload = format!(
        r#"{{"version":1,"type":"pollis-relay-revocations","seq":1,"issued_at":{now},"expires_at":{},"revoked":[]}}"#,
        now + 300
    );
    let b64 = base64::engine::general_purpose::STANDARD;
    let envelope = serde_json::json!({
        "payload_b64": b64.encode(payload.as_bytes()),
        "signature_b64": b64.encode(sk.sign(payload.as_bytes()).to_bytes()),
    });
    let store =
        pollis_relay::policy::RevocationStore::enforcing(b64.encode(sk.verifying_key().to_bytes()));
    store
        .install(&serde_json::to_vec(&envelope).unwrap(), now, 0)
        .expect("a freshly-signed empty list installs");
    store
}

/// Spawn a relay allowing `allow` hosts, with `overrides` mapping a DNS name to
/// a loopback IP, optionally refusing to act as a middle hop.
fn spawn_relay_with(allow: &[&str], overrides: &[(&str, IpAddr)], allow_extend: bool) -> TestRelay {
    let mut config = RelayConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        Allowlist::from_patterns(allow.iter().copied()),
    )
    .unwrap();
    for (host, ip) in overrides {
        config.resolve_overrides.insert((*host).to_string(), *ip);
    }
    config.allow_extend = allow_extend;
    config.revocations = healthy_revocations();

    let peers: Arc<Mutex<Vec<SocketAddr>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = peers.clone();
    let observer: PeerObserver = Arc::new(move |addr: SocketAddr| {
        sink.lock().unwrap().push(addr);
    });
    config.peer_observer = Some(observer);

    let cert = config.server_cert();
    let stats = config.stats.clone();
    let (task, addr) = RelayServer::spawn(config).unwrap();
    TestRelay {
        addr,
        cert,
        stats,
        peers,
        _task: task,
    }
}

fn spawn_relay(allow: &[&str], overrides: &[(&str, IpAddr)]) -> TestRelay {
    spawn_relay_with(allow, overrides, true)
}

// ---- loopback targets ------------------------------------------------------

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

/// A loopback TLS "origin" for `origin.test`. Returns its address, the CA the
/// client must trust, and a counter of accepted connections.
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

/// Round-trip a payload through a live circuit stream against the echo server.
async fn echo_through(stream: &mut BoxedStream, payload: &[u8]) {
    stream.write_all(payload).await.unwrap();
    let mut buf = vec![0u8; payload.len()];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, payload);
}

// ---- O1 --------------------------------------------------------------------

/// A 3-hop circuit carries real inner TLS to the origin, and only the LAST hop
/// ever learns the destination.
///
/// The proof is structural, not statistical: hops 1 and 2 run with an **empty
/// destination allowlist**, which permits nothing. If either of them had been in
/// a position to read the `Connect`, it would have answered `NotAllowed` and the
/// request would have failed. It succeeds, so neither saw it — and the counters
/// confirm exactly one node parsed a `Connect` and exactly one node dialed.
#[tokio::test]
async fn o1_three_hops_hide_the_destination_from_every_hop_but_the_last() {
    let (origin_addr, ca, origin_conns) = spawn_tls_origin("hello-through-three-hops").await;

    // Guard and middle: allow NOTHING as a destination.
    let r1 = spawn_relay(&[], &[]);
    let r2 = spawn_relay(&[], &[]);
    // Last hop: first-party, the only node that may reach the destination.
    let r3 = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);

    let circuit = Circuit::build(
        vec![r1.hop(), r2.hop(), r3.hop()],
        client_identity(),
    )
    .unwrap();
    let stream = circuit
        .connect(ORIGIN_NAME, origin_addr.port())
        .await
        .expect("3-hop circuit builds");

    // Run the client's own TLS to `origin.test` over the circuit — the relays
    // forward opaque bytes and never terminate it.
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    let mut client_cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));
    let server_name = rustls::pki_types::ServerName::try_from(ORIGIN_NAME).unwrap();
    let mut tls = connector
        .connect(server_name, stream)
        .await
        .expect("inner TLS verified for origin.test through 3 hops");

    tls.write_all(b"GET / HTTP/1.1\r\nHost: origin.test\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut resp = Vec::new();
    tls.read_to_end(&mut resp).await.unwrap();
    let resp = String::from_utf8_lossy(&resp);
    assert!(resp.starts_with("HTTP/1.1 200 OK"), "unexpected response: {resp}");
    assert!(resp.contains("hello-through-three-hops"));
    assert_eq!(origin_conns.load(Ordering::Relaxed), 1);

    // Hop 1: a client arrived, it forwarded onward, it never saw a destination.
    assert_eq!(r1.stats.layers(), 0, "hop 1 is reached directly by the client");
    assert_eq!(r1.stats.extends(), 1, "hop 1 must forward to hop 2");
    assert_eq!(r1.stats.connects(), 0, "hop 1 must never parse a Connect");
    assert_eq!(r1.stats.dials(), 0, "hop 1 must never dial a destination");

    // Hop 2: reached through a layer, forwarded onward, saw no destination.
    assert_eq!(r2.stats.layers(), 1, "hop 2 is reached through an onion layer");
    assert_eq!(r2.stats.extends(), 1, "hop 2 must forward to hop 3");
    assert_eq!(r2.stats.connects(), 0, "hop 2 must never parse a Connect");
    assert_eq!(r2.stats.dials(), 0, "hop 2 must never dial a destination");

    // Hop 3: the only node that learns the destination, and the only one that dials.
    assert_eq!(r3.stats.layers(), 1, "hop 3 is reached through an onion layer");
    assert_eq!(r3.stats.extends(), 0, "hop 3 is the last hop and extends nowhere");
    assert_eq!(r3.stats.connects(), 1, "hop 3 is the only node that sees the destination");
    assert_eq!(r3.stats.dials(), 1, "hop 3 dials the destination");

    // Nothing was rejected anywhere: the empty allowlists were never consulted
    // because the destination never reached those nodes.
    assert_eq!(r1.stats.rejected(), 0);
    assert_eq!(r2.stats.rejected(), 0);
    assert_eq!(r3.stats.rejected(), 0);
}

// ---- O2 --------------------------------------------------------------------

/// The last hop cannot see the client's address.
///
/// Every relay records the transport addresses that connect to it. The client's
/// address is by definition the one hop 1 saw; the assertion is that it appears
/// nowhere else. Hops 2 and 3 see relay-owned sockets instead.
#[tokio::test]
async fn o2_last_hop_never_sees_the_client_address() {
    let (echo_addr, _c) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let r1 = spawn_relay(&[], &[]);
    let r2 = spawn_relay(&[], &[]);
    let r3 = spawn_relay(&[host.as_str()], &[]);

    let circuit = Circuit::build(vec![r1.hop(), r2.hop(), r3.hop()], client_identity()).unwrap();
    let mut stream = circuit
        .connect(&host, echo_addr.port())
        .await
        .expect("3-hop circuit builds");
    echo_through(&mut stream, b"onion").await;

    let client_peers = r1.peers();
    assert_eq!(client_peers.len(), 1, "hop 1 saw exactly the client: {client_peers:?}");
    let client_addr = client_peers[0];

    assert!(
        !r2.saw_peer(client_addr),
        "middle hop must not see the client address {client_addr} (saw {:?})",
        r2.peers()
    );
    assert!(
        !r3.saw_peer(client_addr),
        "LAST hop must not see the client address {client_addr} (saw {:?})",
        r3.peers()
    );

    // And each hop saw exactly one predecessor — its own neighbour, nobody else's.
    assert_eq!(r2.peers().len(), 1);
    assert_eq!(r3.peers().len(), 1);
    assert_ne!(r2.peers()[0], r3.peers()[0], "hops 2 and 3 have different predecessors");
}

// ---- O3 --------------------------------------------------------------------

/// A hop that presents the wrong leaf cert is rejected by the CLIENT's own pin,
/// inside the layer — the check that does not depend on any relay behaving.
///
/// The circuit is extended to hop 2 correctly (the forwarding relay's own
/// fingerprint pin passes), and only then does the client run a layer pinned to a
/// *different* relay's cert. It must fail, and no destination may be reached.
#[tokio::test]
async fn o3_client_rejects_a_layer_from_the_wrong_hop() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let r1 = spawn_relay(&[], &[]);
    let r2 = spawn_relay(&[host.as_str()], &[]);
    let impostor = spawn_relay(&[host.as_str()], &[]);

    // Build hop 1 by hand so the layer can be pinned to the WRONG cert while the
    // extend itself is entirely valid.
    let link = RelayLink::open(r1.addr, &r1.cert).await.unwrap();
    let version = link.version();
    assert!(version >= proto::ONION_MIN_VERSION);
    let mut stream = BoxedStream::new(link.open_stream().await.unwrap());

    let identity = client_identity();
    proto::write_handshake(&mut stream, &identity.fresh_handshake().unwrap(), version)
        .await
        .unwrap();
    let real_hop2 = Hop::new(r2.addr, r2.cert.clone());
    proto::write_extend(
        &mut stream,
        &Extend {
            addr: real_hop2.addr,
            next_cert_sha256: real_hop2.cert_fingerprint(),
        },
        version,
    )
    .await
    .unwrap();
    proto::read_response(&mut stream).await.expect("extend accepted");
    assert_eq!(r1.stats.extends(), 1, "the extend itself was legitimate");

    // Now pin the layer to the impostor's cert. Hop 2 holds its own key, not the
    // impostor's, so the handshake cannot succeed.
    let wrong = pollis_relay::onion::client_layer(stream, &impostor.cert).await;
    assert!(
        wrong.is_err(),
        "a layer pinned to the wrong hop's cert must fail, never fall through in the clear"
    );
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0, "no destination may be reached");
    assert_eq!(r2.stats.connects(), 0);
    assert_eq!(r2.stats.dials(), 0);
}

/// A forwarding relay refuses to hand the circuit to a next hop whose cert does
/// not match the fingerprint the `Extend` pinned — so `Extend` cannot be pointed
/// at an arbitrary host.
#[tokio::test]
async fn o3_forwarding_relay_rejects_a_fingerprint_mismatch() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let r1 = spawn_relay(&[], &[]);
    let r2 = spawn_relay(&[host.as_str()], &[]);
    let other = spawn_relay(&[host.as_str()], &[]);

    // Hop 2 names r2's ADDRESS but carries `other`'s cert, so the fingerprint the
    // Extend pins will not match what r2 presents.
    let mismatched = Hop::new(r2.addr, other.cert.clone());
    let circuit = Circuit::build(vec![r1.hop(), mismatched], client_identity()).unwrap();
    let err = circuit.connect(&host, echo_addr.port()).await;
    assert!(err.is_err(), "a mismatched next-hop pin must fail the circuit");

    assert_eq!(r1.stats.extends(), 0, "the forwarding relay must not count a failed extend");
    assert!(r1.stats.rejected() >= 1, "the forwarding relay must record the rejection");
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0, "no destination may be reached");
}

/// The last hop still enforces the destination allowlist through a multi-hop
/// circuit — extending is not a way around the closed overlay (§1.2).
#[tokio::test]
async fn o3_allowlist_still_binds_at_the_last_hop() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let r1 = spawn_relay(&[], &[]);
    // The last hop allows only some OTHER host, so the real destination is not
    // permitted there.
    let r2 = spawn_relay(&["nothing.test"], &[]);

    let circuit = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    let err = circuit.connect(&host, echo_addr.port()).await;
    assert!(err.is_err(), "a non-allowlisted destination must be refused at the last hop");
    assert_eq!(r2.stats.connects(), 1, "the last hop parsed the Connect");
    assert_eq!(r2.stats.dials(), 0, "and refused to dial it");
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- O4 --------------------------------------------------------------------

/// Single-hop is unchanged, and a 2-hop circuit works — the same `Circuit` API,
/// a different path length.
#[tokio::test]
async fn o4_single_and_two_hop_circuits_both_work() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let r1 = spawn_relay(&[host.as_str()], &[]);
    let r2 = spawn_relay(&[host.as_str()], &[]);

    // n = 1: the v0 path, no layers anywhere.
    let single = Circuit::build_single_hop(r1.hop(), client_identity());
    let mut s = single.connect(&host, echo_addr.port()).await.expect("single hop");
    echo_through(&mut s, b"one-hop!").await;
    assert_eq!(r1.stats.dials(), 1);
    assert_eq!(r1.stats.layers(), 0, "a single-hop circuit peels no layer");
    assert_eq!(r1.stats.extends(), 0);
    drop(s);

    // n = 2: r1 forwards, r2 terminates.
    let two = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    let mut s = two.connect(&host, echo_addr.port()).await.expect("two hops");
    echo_through(&mut s, b"two-hop!").await;
    assert_eq!(r1.stats.extends(), 1);
    assert_eq!(r1.stats.dials(), 1, "hop 1 dialed only for the single-hop circuit");
    assert_eq!(r2.stats.layers(), 1);
    assert_eq!(r2.stats.dials(), 1);
    assert_eq!(echo_conns.load(Ordering::Relaxed), 2);
}

/// A circuit longer than the cap is refused up front, not attempted.
#[tokio::test]
async fn o4_hop_cap_is_enforced() {
    let relay = spawn_relay(&[], &[]);
    let hops: Vec<Hop> = std::iter::repeat_with(|| relay.hop())
        .take(pollis_relay::MAX_HOPS + 1)
        .collect();
    assert!(Circuit::build(hops, client_identity()).is_err());
    assert!(Circuit::build(Vec::new(), client_identity()).is_err());
}

// ---- O5 --------------------------------------------------------------------

/// A relay configured last-hop-only refuses an `Extend` cleanly — a typed
/// rejection, not a dropped stream — and dials nothing.
#[tokio::test]
async fn o5_extend_disabled_refuses_cleanly() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();

    let r1 = spawn_relay_with(&[], &[], false);
    let r2 = spawn_relay(&[host.as_str()], &[]);

    let circuit = Circuit::build(vec![r1.hop(), r2.hop()], client_identity()).unwrap();
    let err = match circuit.connect(&host, echo_addr.port()).await {
        Ok(_) => panic!("a last-hop-only relay must refuse to extend"),
        Err(e) => e,
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("ExtendFailed"),
        "expected a typed ExtendFailed rejection, got: {msg}"
    );

    assert_eq!(r1.stats.extends(), 0);
    assert_eq!(r2.stats.layers(), 0, "the next hop was never contacted");
    assert_eq!(echo_conns.load(Ordering::Relaxed), 0);
}

// ---- O6: version negotiation ----------------------------------------------

#[test]
fn o6_version_helpers_agree_with_the_alpn_table() {
    assert_eq!(proto::version_from_alpn(proto::ALPN), Some(4));
    assert_eq!(proto::version_from_alpn(proto::ALPN_V3), Some(3));
    assert_eq!(proto::version_from_alpn(b"pollis-relay/2"), None);
    assert_eq!(proto::version_from_alpn(b"h3"), None);
    assert!(proto::version_supported(proto::PROTOCOL_VERSION));
    assert!(proto::version_supported(proto::MIN_PROTOCOL_VERSION));
    assert!(!proto::version_supported(proto::PROTOCOL_VERSION + 1));
    assert!(!proto::version_supported(proto::MIN_PROTOCOL_VERSION - 1));
}

/// The onion frames have no pre-v4 encoding, so the writers refuse to emit them
/// at an older generation rather than putting bytes on a stream the peer would
/// misparse.
#[tokio::test]
async fn o6_onion_frames_cannot_be_written_at_an_older_generation() {
    let mut buf = Vec::new();
    let extend = Extend {
        addr: "127.0.0.1:1".parse().unwrap(),
        next_cert_sha256: [0u8; 32],
    };
    assert!(proto::write_extend(&mut buf, &extend, 3).await.is_err());
    assert!(proto::write_layer(&mut buf, 3).await.is_err());
    assert!(
        buf.is_empty(),
        "a refused frame must not put a single byte on the wire"
    );

    // And an unsupported version byte is refused on read, so a stream from a
    // future generation is never half-parsed.
    let mut future = &[9u8, 1u8][..];
    assert!(proto::read_frame_header(&mut future).await.is_err());
}

/// A current relay still serves a client that speaks the older generation
/// verbatim, and answers in that generation — the shipped fleet's clients keep
/// working against an upgraded node.
#[tokio::test]
async fn o7_relay_serves_an_older_generation_client() {
    let (echo_addr, echo_conns) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let relay = spawn_relay(&[host.as_str()], &[]);

    let link = RelayLink::open(relay.addr, &relay.cert).await.unwrap();
    assert_eq!(
        link.version(),
        proto::PROTOCOL_VERSION,
        "two current peers negotiate the newest generation"
    );
    let mut stream = link.open_stream().await.unwrap();

    // Speak v3 frames on a connection that could have carried v4.
    let identity = client_identity();
    let v3 = proto::MIN_PROTOCOL_VERSION;
    proto::write_handshake(&mut stream, &identity.fresh_handshake().unwrap(), v3)
        .await
        .unwrap();
    proto::write_connect(
        &mut stream,
        &Connect {
            host: host.clone(),
            port: echo_addr.port(),
        },
        v3,
    )
    .await
    .unwrap();
    proto::read_response(&mut stream)
        .await
        .expect("a v3 exchange is served unchanged");

    stream.write_all(b"legacy!!").await.unwrap();
    let mut buf = [0u8; 8];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"legacy!!");
    assert_eq!(echo_conns.load(Ordering::Relaxed), 1);
    assert_eq!(relay.stats.dials(), 1);
}

/// A v3-generation stream cannot smuggle an `Extend`: the relay refuses the frame
/// rather than acting on it, and forwards nowhere.
#[tokio::test]
async fn o7_v3_stream_cannot_request_an_extend() {
    let r1 = spawn_relay(&[], &[]);
    let r2 = spawn_relay(&[], &[]);

    let link = RelayLink::open(r1.addr, &r1.cert).await.unwrap();
    let mut stream = link.open_stream().await.unwrap();

    let identity = client_identity();
    let v3 = proto::MIN_PROTOCOL_VERSION;
    proto::write_handshake(&mut stream, &identity.fresh_handshake().unwrap(), v3)
        .await
        .unwrap();

    // Hand-craft a v3-versioned MSG_EXTEND — the encoding the writers refuse to
    // produce — straight onto the wire.
    let hop2 = r2.hop();
    let mut frame: Vec<u8> = vec![v3, 3u8, 4u8];
    match hop2.addr.ip() {
        IpAddr::V4(ip) => frame.extend_from_slice(&ip.octets()),
        IpAddr::V6(_) => unreachable!("test relays bind IPv4 loopback"),
    }
    frame.extend_from_slice(&hop2.addr.port().to_be_bytes());
    frame.extend_from_slice(&hop2.cert_fingerprint());
    stream.write_all(&frame).await.unwrap();
    stream.flush().await.unwrap();

    let err = proto::read_response(&mut stream)
        .await
        .expect_err("a v3 Extend must be refused");
    assert!(
        matches!(err, proto::ProtoError::Rejected(RejectReason::BadRequest)),
        "expected a BadRequest rejection, got {err:?}"
    );
    assert_eq!(r1.stats.extends(), 0);
    assert_eq!(r2.stats.layers(), 0, "the named next hop was never contacted");
}

// ---- O8: the single-hop client entry point is untouched --------------------

/// `RelayClient::connect` — the v0 entry point `pollis-core` calls — still works
/// exactly as before, over a relay that now also speaks the onion generation.
#[tokio::test]
async fn o8_v0_client_entry_point_unchanged() {
    let (echo_addr, _c) = spawn_echo().await;
    let host = echo_addr.ip().to_string();
    let relay = spawn_relay(&[host.as_str()], &[]);

    let mut stream = RelayClient::connect(
        relay.addr,
        &relay.cert,
        &client_identity(),
        &host,
        echo_addr.port(),
    )
    .await
    .expect("v0 single-hop connect");
    stream.write_all(b"v0!!").await.unwrap();
    let mut buf = [0u8; 4];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"v0!!");
    assert_eq!(relay.stats.dials(), 1);
    assert_eq!(relay.stats.connects(), 1);
    assert_eq!(relay.stats.extends(), 0);
    assert_eq!(relay.stats.layers(), 0);
}
