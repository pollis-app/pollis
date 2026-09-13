//! Unauthenticated-surface limits: what an attacker can do to a relay node
//! **before** any handshake, and what a self-certified client can do with an
//! opening `Layer` frame.
//!
//! Each test encodes an invariant that the code once did not hold:
//!
//! - L1: QUIC connections are capped **per source IP** at accept — one host
//!   cannot fill the global cap on its own.
//! - L2: a stream that opens and then says nothing (or half of something) is
//!   refused at the deadline, and the circuit slot it held is freed. Without
//!   this, every such stream parked a task and a slot forever.
//! - L3: a stream that opens with `Layer` — which any client can send, since it
//!   is unauthenticated — is limited under the sending connection's address
//!   like a direct stream. Previously it was admitted account-only, which with
//!   self-minted accounts is no limit at all.
//! - L4: the egress allowlist binds `host:port`, not host alone: a bare entry
//!   means 443 only, and an allowlisted host's other ports stay unreachable.
//!
//! Everything runs headless on loopback with certs generated in-test.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ml_dsa::{Keypair, MlDsa44, SigningKey};
use pollis_relay::client::{ClientIdentity, RelayClient};
use pollis_relay::onion;
use pollis_relay::proto::{self, Connect, DeviceCertMaterial, ProtoError, RejectReason};
use pollis_relay::ratelimit::RateLimitConfig;
use pollis_relay::server::{Allowlist, RelayConfig, RelayServer, RelayStats};
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const USER: &str = "u_limits";
const DEVICE: &str = "d_limits";
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

// ---- test infrastructure ---------------------------------------------------

struct TestRelay {
    addr: SocketAddr,
    cert: CertificateDer<'static>,
    stats: Arc<RelayStats>,
    _task: tokio::task::JoinHandle<()>,
}

/// Spawn a relay with the given allowlist entries (verbatim — no port is
/// appended, because the port rule is what some of these tests are about) and
/// a chance to adjust the config.
fn spawn_relay(allow: &[String], tune: impl FnOnce(&mut RelayConfig)) -> TestRelay {
    let mut config = RelayConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        Allowlist::from_patterns(allow.iter().map(|s| s.as_str())),
    )
    .unwrap();
    tune(&mut config);
    let cert = config.server_cert();
    let stats = config.stats.clone();
    let (task, addr) = RelayServer::spawn(config).unwrap();
    TestRelay {
        addr,
        cert,
        stats,
        _task: task,
    }
}

/// A loopback TCP echo server.
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

/// `host:port` for an echo server — the exact form of a deployed entry.
fn exact(addr: SocketAddr) -> String {
    format!("{}:{}", addr.ip(), addr.port())
}

/// Dial a relay raw: pinned cert, current ALPN, keep-alives so a held
/// connection stays up without any stream traffic (as an attacker's would).
async fn dial(relay: &TestRelay) -> anyhow::Result<(quinn::Endpoint, quinn::Connection)> {
    use quinn::crypto::rustls::QuicClientConfig;

    pollis_relay::tls::ensure_crypto_provider();
    let mut client_crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(pollis_relay::tls::PinnedServerCertVerifier::new(
            relay.cert.clone(),
        ))
        .with_no_client_auth();
    client_crypto.alpn_protocols = proto::SUPPORTED_ALPNS.iter().map(|a| a.to_vec()).collect();
    let mut client_config =
        quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client_crypto)?));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_millis(500)));
    client_config.transport_config(Arc::new(transport));

    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap())?;
    endpoint.set_default_client_config(client_config);
    let connection = tokio::time::timeout(
        Duration::from_secs(5),
        endpoint.connect(relay.addr, pollis_relay::tls::RELAY_SERVER_NAME)?,
    )
    .await
    .map_err(|_| anyhow::anyhow!("QUIC handshake timed out"))??;
    Ok((endpoint, connection))
}

/// Open a single-hop circuit the ordinary way.
async fn connect(relay: &TestRelay, target: SocketAddr) -> Result<pollis_relay::RelayStream, ProtoError> {
    let result = RelayClient::connect(
        relay.addr,
        &relay.cert,
        &client_identity(),
        &target.ip().to_string(),
        target.port(),
    )
    .await;
    match result {
        Ok(stream) => Ok(stream),
        Err(e) => match e.downcast::<ProtoError>() {
            Ok(proto_err) => Err(proto_err),
            Err(other) => panic!("expected a protocol-level verdict, got: {other}"),
        },
    }
}

fn assert_rejected(result: Result<pollis_relay::RelayStream, ProtoError>, expected: RejectReason, context: &str) {
    match result {
        Ok(_) => panic!("{context}: admitted, expected Rejected({expected:?})"),
        Err(ProtoError::Rejected(reason)) => assert_eq!(reason, expected, "{context}"),
        Err(other) => panic!("{context}: expected Rejected({expected:?}), got {other}"),
    }
}

async fn echo_through<S: AsyncReadExt + AsyncWriteExt + Unpin>(stream: &mut S, payload: &[u8]) {
    stream.write_all(payload).await.unwrap();
    let mut buf = vec![0u8; payload.len()];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, payload);
}

// ---- L1: per-IP connection cap ---------------------------------------------

/// Two connections from one address are admitted under a cap of two; the third
/// is refused at accept — before any handshake frame, before any stream — and
/// closing one lets the next through.
#[tokio::test]
async fn l1_connections_are_capped_per_source_ip() {
    let relay = spawn_relay(&[], |c| {
        c.max_connections_per_ip = 2;
    });

    let first = dial(&relay).await.expect("first connection from this IP");
    let _second = dial(&relay).await.expect("second connection from this IP");
    let third = dial(&relay).await;
    assert!(
        third.is_err(),
        "third connection from one IP must be refused at accept"
    );

    // Freeing a slot readmits.
    drop(first);
    tokio::time::sleep(Duration::from_millis(300)).await;
    dial(&relay)
        .await
        .expect("a slot freed by a closed connection is reusable");
}

// ---- L2: negotiation deadline ----------------------------------------------

/// A stream that sends part of a frame and then stalls is refused at the
/// deadline instead of parking a task forever.
#[tokio::test]
async fn l2_a_stream_that_never_finishes_its_first_frame_is_refused() {
    let relay = spawn_relay(&[], |c| {
        c.first_frame_timeout = Duration::from_millis(300);
    });
    let (_endpoint, connection) = dial(&relay).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    // One byte — the version — so the stream exists on the relay, then silence.
    send.write_all(&[proto::PROTOCOL_VERSION]).await.unwrap();

    let verdict = tokio::time::timeout(Duration::from_secs(3), proto::read_response(&mut recv))
        .await
        .expect("relay must answer a stalled stream within its deadline");
    match verdict {
        Err(ProtoError::Rejected(RejectReason::BadRequest)) => {}
        other => panic!("expected Rejected(BadRequest) at the deadline, got {other:?}"),
    }
    assert_eq!(relay.stats.rejected(), 1);
}

/// A stream that authenticates and is admitted — holding a circuit slot — but
/// never sends its terminal command gives the slot back at the deadline.
#[tokio::test]
async fn l2_a_stalled_admitted_stream_frees_its_circuit_slot() {
    let (echo_addr, _c) = spawn_echo().await;
    let relay = spawn_relay(&[exact(echo_addr)], |c| {
        c.first_frame_timeout = Duration::from_millis(300);
        c.rate_limits = RateLimitConfig {
            max_concurrent_per_account: 1,
            ..Default::default()
        };
    });

    let (_endpoint, connection) = dial(&relay).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    let handshake = client_identity().fresh_handshake().unwrap();
    proto::write_handshake(&mut send, &handshake, proto::PROTOCOL_VERSION)
        .await
        .unwrap();
    // Admitted (the slot is taken) — and then nothing.
    let verdict = tokio::time::timeout(Duration::from_secs(3), proto::read_response(&mut recv))
        .await
        .expect("relay must give up on the stalled stream");
    match verdict {
        Err(ProtoError::Rejected(RejectReason::BadRequest)) => {}
        other => panic!("expected Rejected(BadRequest) at the deadline, got {other:?}"),
    }
    assert_eq!(relay.stats.authorized(), 1, "the handshake itself was fine");

    // The account's single slot must be free again: a real circuit goes through.
    let mut stream = connect(&relay, echo_addr)
        .await
        .expect("slot held by the stalled stream must be released");
    echo_through(&mut stream, b"after-timeout").await;
}

// ---- L3: Layer opener is per-IP limited ------------------------------------

/// Open a stream with `Layer`, complete the onion layer against the relay's own
/// pinned cert (public in the directory — anyone can), and run a handshake +
/// `Connect` inside it: exactly what a client does to pose as a previous hop.
async fn connect_through_a_forged_layer(
    relay: &TestRelay,
    target: SocketAddr,
) -> Result<(), ProtoError> {
    let (_endpoint, connection) = dial(relay).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    proto::write_layer(&mut send, proto::PROTOCOL_VERSION).await?;
    proto::read_response(&mut recv).await?;

    let joined = tokio::io::join(recv, send);
    let mut layer = onion::client_layer(joined, &relay.cert)
        .await
        .expect("the relay's leaf cert is public, so the layer completes");
    let handshake = client_identity().fresh_handshake().unwrap();
    proto::write_handshake(&mut layer, &handshake, proto::PROTOCOL_VERSION).await?;
    proto::write_connect(
        &mut layer,
        &Connect {
            host: target.ip().to_string(),
            port: target.port(),
        },
        proto::PROTOCOL_VERSION,
    )
    .await?;
    proto::read_response(&mut layer).await?;
    echo_through(&mut layer, b"through-a-forged-layer").await;
    // Hold the circuit until the caller drops the future's result; the stream
    // ends here, which is fine — the verdict is what was under test.
    Ok(())
}

/// With a per-IP cap of one circuit, a second circuit from the same address is
/// refused **even when it arrives inside a `Layer`** — the opener buys no
/// exemption. The same shape under a cap of two is admitted, so the refusal is
/// the limit and not a broken layer path.
#[tokio::test]
async fn l3_a_layer_opener_does_not_escape_the_per_ip_limits() {
    let (echo_addr, _c) = spawn_echo().await;

    let capped = spawn_relay(&[exact(echo_addr)], |c| {
        c.rate_limits = RateLimitConfig {
            max_concurrent_per_ip: 1,
            ..Default::default()
        };
    });
    // A direct circuit takes this address's only slot.
    let mut direct = connect(&capped, echo_addr).await.expect("first circuit");
    echo_through(&mut direct, b"direct").await;

    let verdict = connect_through_a_forged_layer(&capped, echo_addr).await;
    match verdict {
        Err(ProtoError::Rejected(RejectReason::RateLimited)) => {}
        Ok(()) => panic!("a Layer-prefixed stream escaped the per-IP circuit cap"),
        Err(other) => panic!("expected Rejected(RateLimited), got {other}"),
    }
    assert!(capped.stats.rate_limited() >= 1);
    assert_eq!(capped.stats.layers(), 1, "the layer was peeled and judged");

    // Control: the identical exchange is admitted when the address has room.
    let roomy = spawn_relay(&[exact(echo_addr)], |c| {
        c.rate_limits = RateLimitConfig {
            max_concurrent_per_ip: 2,
            ..Default::default()
        };
    });
    let mut direct = connect(&roomy, echo_addr).await.expect("first circuit");
    echo_through(&mut direct, b"direct").await;
    connect_through_a_forged_layer(&roomy, echo_addr)
        .await
        .expect("under the cap, a layered stream is served like a direct one");
}

// ---- L4: host:port allowlist ------------------------------------------------

/// A bare host entry means HTTPS only: the same host on another port is
/// `NotAllowed`. An explicit `host:port` permits exactly that port, and `:*`
/// opts a host into every port.
#[tokio::test]
async fn l4_allowlist_binds_host_and_port() {
    let (echo_a, dials_a) = spawn_echo().await;
    let (echo_b, dials_b) = spawn_echo().await;
    // Both echo servers share 127.0.0.1 and differ only by port.
    assert_eq!(echo_a.ip(), echo_b.ip());

    // Bare host ⇒ 443 only; the echo listens elsewhere, so it is unreachable.
    let bare = spawn_relay(&[echo_a.ip().to_string()], |_| {});
    assert_rejected(
        connect(&bare, echo_a).await,
        RejectReason::NotAllowed,
        "bare host entry must not open every port of that host",
    );
    assert_eq!(dials_a.load(Ordering::Relaxed), 0, "the relay must not have dialled");

    // Exact host:port ⇒ that port and no other port of the same host.
    let pinned = spawn_relay(&[exact(echo_a)], |_| {});
    let mut ok = connect(&pinned, echo_a).await.expect("the named port is reachable");
    echo_through(&mut ok, b"pinned").await;
    assert_rejected(
        connect(&pinned, echo_b).await,
        RejectReason::NotAllowed,
        "another port of an allowlisted host is not allowlisted",
    );
    assert_eq!(dials_b.load(Ordering::Relaxed), 0);
    assert_eq!(pinned.stats.dials(), 1);

    // `host:*` ⇒ every port, written out on purpose.
    let any_port = spawn_relay(&[format!("{}:*", echo_a.ip())], |_| {});
    let mut a = connect(&any_port, echo_a).await.expect("any port: a");
    echo_through(&mut a, b"a").await;
    let mut b = connect(&any_port, echo_b).await.expect("any port: b");
    echo_through(&mut b, b"b").await;
}
