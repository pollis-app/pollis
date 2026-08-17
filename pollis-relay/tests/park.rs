//! Parked peer-relay proof tests (#813 Phase P1, design §7.1).
//!
//! A peer-hosted relay is a device behind NAT. It cannot be dialled, so it opens
//! a connection outbound, parks it, and a first-party relay splices an `Extend`
//! into that connection instead of dialling. Every test here is written to fail
//! if a property that makes that safe stops holding — the same way
//! `tests/onion.rs` proves a hop never saw the destination by giving it an empty
//! allowlist:
//!
//! - P1: a full circuit `client → first-party guard → parked peer → first-party
//!   exit` carries bytes, **while the peer's advertised address is a dead port**
//!   and the guard's allowlist is empty. A dial cannot have happened, and the
//!   guard cannot have seen the destination.
//! - P2: a parked peer whose cert is **revoked** is refused. Parking buys no
//!   exemption, and the check runs on the cert selector, which is the one that
//!   binds a per-peer identity.
//! - P3: a parked connection that **dies** is evicted from the registry, and a
//!   later `Extend` for it falls back to dialling (and fails) rather than
//!   splicing into a corpse.
//! - P4: a **v3** stream cannot park, even hand-rolling the frame — the version
//!   gate is at the reader, not just at our writer.
//! - P5: parking **twice** under one fingerprint does not displace the incumbent
//!   or leave two entries.
//! - P6: the health port publishes the **parked peers' own leaf certs and
//!   nothing else** — the shape P3's reconciler reads.
//!
//! Everything runs headless on loopback, with certs generated in-test.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey as Ed25519SigningKey};
use ml_dsa::{Keypair, MlDsa44, SigningKey};
use pollis_relay::circuit::{Circuit, Hop};
use pollis_relay::client::ClientIdentity;
use pollis_relay::park::{read_tunnel_datagram, write_tunnel_datagram, ParkedPeers};
use pollis_relay::policy::RevocationStore;
use pollis_relay::proto::{self, write_park, DeviceCertMaterial, Park};
use pollis_relay::server::{Allowlist, PeerObserver, RelayConfig, RelayServer, RelayStats};
use pollis_relay::stream::BoxedStream;
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

const USER: &str = "u_park";
const DEVICE: &str = "d_park";
const ORIGIN_NAME: &str = "origin.test";
const DEVICE_ED_PUB: [u8; 32] = [11u8; 32];

/// The address a client advertises for a parked peer. Port 0 is **not
/// dialable** — quinn refuses it outright — so a circuit that works cannot have
/// been dialled (P1), and a fallback dial fails immediately rather than waiting
/// out a handshake timeout (P3). A real client would send the peer's `parked_at`
/// relay address here; the relay ignores it either way, because a parked peer is
/// selected by fingerprint.
const DEAD_ADDR: &str = "127.0.0.1:0";

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

fn client_identity() -> Arc<ClientIdentity> {
    let device = SigningKey::<MlDsa44>::from_seed(&[11u8; 32].into());
    let account = SigningKey::<MlDsa44>::from_seed(&[12u8; 32].into());
    let cert = DeviceCertMaterial::mint(
        &account,
        DEVICE,
        &DEVICE_ED_PUB,
        &device.verifying_key().encode(),
        1,
        1_700_000_000,
    );
    Arc::new(ClientIdentity::new(
        USER,
        DEVICE,
        DEVICE_ED_PUB,
        device,
        cert,
    ))
}

// ---- signed revocation artifacts -------------------------------------------

fn directory_key() -> Ed25519SigningKey {
    Ed25519SigningKey::from_bytes(&[42u8; 32])
}

/// A store keyed on the directory key, holding a fresh list revoking `revoked`
/// (a JSON fragment — empty for "nobody").
///
/// Every relay here needs one: a node that cannot evaluate revocation refuses to
/// extend at all (#813 Phase C), so "keyed, with a current list" is the
/// production shape of a middle hop and the shape these tests run.
fn store_revoking(revoked: &str) -> RevocationStore {
    let sk = directory_key();
    let now = proto::now_unix();
    let payload = format!(
        r#"{{"version":1,"type":"pollis-relay-revocations","seq":1,"issued_at":{now},"expires_at":{},"revoked":[{revoked}]}}"#,
        now + 300
    );
    let envelope = serde_json::to_vec(&serde_json::json!({
        "payload_b64": B64.encode(payload.as_bytes()),
        "signature_b64": B64.encode(sk.sign(payload.as_bytes()).to_bytes()),
    }))
    .unwrap();
    let store = RevocationStore::enforcing(B64.encode(sk.verifying_key().to_bytes()));
    store
        .install(&envelope, now, 0)
        .expect("a freshly-signed list installs");
    store
}

fn healthy_revocations() -> RevocationStore {
    store_revoking("")
}

/// The `cert_sha256_b64` revocation selector for a DER leaf.
fn cert_selector(cert: &CertificateDer<'static>) -> String {
    use sha2::{Digest, Sha256};
    B64.encode(Sha256::digest(cert.as_ref()))
}

// ---- test relays -----------------------------------------------------------

struct TestRelay {
    addr: SocketAddr,
    cert: CertificateDer<'static>,
    stats: Arc<RelayStats>,
    parked: Arc<ParkedPeers>,
    peers: Arc<Mutex<Vec<SocketAddr>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl TestRelay {
    fn hop(&self) -> Hop {
        Hop::new(self.addr, self.cert.clone())
    }
}

/// A relay allowing `allow` hosts, optionally resolving `origin.test` to
/// loopback, with the given revocation store.
fn spawn_relay(allow: &[&str], resolve_origin: bool, revocations: RevocationStore) -> TestRelay {
    let mut config = RelayConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        Allowlist::from_patterns(allow.iter().copied()),
    )
    .unwrap();
    if resolve_origin {
        config
            .resolve_overrides
            .insert(ORIGIN_NAME.to_string(), "127.0.0.1".parse().unwrap());
    }
    config.revocations = revocations;

    let peers: Arc<Mutex<Vec<SocketAddr>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = peers.clone();
    let observer: PeerObserver = Arc::new(move |addr: SocketAddr| {
        sink.lock().unwrap().push(addr);
    });
    config.peer_observer = Some(observer);

    let cert = config.server_cert();
    let stats = config.stats.clone();
    let parked = config.parked.clone();
    let (task, addr) = RelayServer::spawn(config).unwrap();
    TestRelay {
        addr,
        cert,
        stats,
        parked,
        peers,
        _task: task,
    }
}

// ---- the peer --------------------------------------------------------------

/// A peer-hosted relay: a relay engine on loopback with an **empty destination
/// allowlist** (so it can never be an exit), reached only through the connection
/// it parked at a first-party relay.
///
/// A deliberate stand-in for the shipped device side, which lives in
/// `pollis-core`'s `net::peer::park` (phase P2) and cannot be reached from here
/// — `pollis-core` depends on this crate, not the other way round. It speaks the
/// same wire byte for byte: handshake + `Park` on one stream it opens, then every
/// stream the relay opens back is a splice carrying length-delimited packets for
/// the loopback engine. The mirror image of P2's own `StandInRelay`.
struct TestPeer {
    cert: CertificateDer<'static>,
    engine_addr: SocketAddr,
    stats: Arc<RelayStats>,
    /// Whether anything ever completed a QUIC handshake with the peer's engine.
    /// The peer's own `peer_observer`, used the same way `tests/revocation.rs`
    /// uses it.
    contacted: Arc<Mutex<Vec<SocketAddr>>>,
    /// Streams the relay has opened on the parked connection. Every one is a
    /// splice, so this counts how many times this device was actually used as a
    /// hop — and, at zero, proves a refusal happened BEFORE the relay reached
    /// for the peer at all. It moves on the tunnel's very first packet, well
    /// before any handshake could complete, which is what makes it a reliable
    /// signal where "did the engine see a connection" is a race.
    splices_seen: Arc<AtomicUsize>,
    /// Holding these holds the parking: the park stream keeps the registration,
    /// and dropping the endpoint closes the connection.
    parking: Option<Parking>,
    _engine: tokio::task::JoinHandle<()>,
}

/// What a parked device holds: the connection, the stream the registration hangs
/// on, and the task accepting splices.
struct Parking {
    _endpoint: quinn::Endpoint,
    _connection: quinn::Connection,
    _park_stream: quinn::SendStream,
    accept: tokio::task::JoinHandle<()>,
}

impl Drop for Parking {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

impl TestPeer {
    /// Stand up the engine. Separate from parking so a test can revoke the
    /// peer's cert before the relay it parks at is even built.
    fn start() -> TestPeer {
        let mut config =
            RelayConfig::new("127.0.0.1:0".parse().unwrap(), Allowlist::default()).unwrap();
        // Forwarding to the next relay is the peer's whole job, so it needs a
        // usable revocation store; the empty allowlist is what stops it exiting.
        config.revocations = healthy_revocations();
        let contacted: Arc<Mutex<Vec<SocketAddr>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = contacted.clone();
        let observer: PeerObserver = Arc::new(move |addr: SocketAddr| {
            sink.lock().unwrap().push(addr);
        });
        config.peer_observer = Some(observer);
        let cert = config.server_cert();
        let stats = config.stats.clone();
        let (engine, engine_addr) = RelayServer::spawn(config).unwrap();
        TestPeer {
            cert,
            engine_addr,
            stats,
            contacted,
            splices_seen: Arc::new(AtomicUsize::new(0)),
            parking: None,
            _engine: engine,
        }
    }

    /// Dial `relay`, run the ordinary device handshake, park, and start serving
    /// whatever the relay splices back.
    async fn park(&mut self, relay: &TestRelay) {
        self.try_park(relay)
            .await
            .expect("a peer parks at a first-party relay");
    }

    async fn try_park(&mut self, relay: &TestRelay) -> anyhow::Result<()> {
        let (endpoint, connection) = dial(relay).await?;
        let (mut send, mut recv) = connection.open_bi().await?;

        let identity = client_identity();
        let handshake = identity.fresh_handshake()?;
        proto::write_handshake(&mut send, &handshake, proto::PROTOCOL_VERSION).await?;
        write_park(
            &mut send,
            &Park {
                relay_leaf_der: self.cert.as_ref().to_vec(),
            },
            proto::PROTOCOL_VERSION,
        )
        .await?;
        // The acknowledgement is the ordinary response frame, so a refusal
        // arrives as a typed reason rather than a closed stream.
        proto::read_response(&mut recv).await?;

        // Every stream the relay opens on this connection is a splice.
        let engine_addr = self.engine_addr;
        let splices = connection.clone();
        let seen = self.splices_seen.clone();
        let accept = tokio::spawn(async move {
            while let Ok((send, recv)) = splices.accept_bi().await {
                seen.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(serve_splice(send, recv, engine_addr));
            }
        });

        self.parking = Some(Parking {
            _endpoint: endpoint,
            _connection: connection,
            _park_stream: send,
            accept,
        });
        Ok(())
    }

    /// The hop a client puts in a path for this peer. The address is deliberately
    /// undialable: a parked peer is selected by fingerprint, never reached by
    /// dialling.
    fn hop(&self) -> Hop {
        Hop::new(DEAD_ADDR.parse().unwrap(), self.cert.clone())
    }

    /// True once something has completed a QUIC handshake with the engine.
    fn was_contacted(&self) -> bool {
        !self.contacted.lock().unwrap().is_empty()
    }

    /// How many circuits the relay has spliced into this device.
    fn splices_seen(&self) -> usize {
        self.splices_seen.load(Ordering::Relaxed)
    }

    /// Pull the plug on the parked connection, as a peer going offline does.
    fn unpark(&mut self) {
        self.parking.take();
    }
}

/// Dial a relay the way a parked device does: pinned cert, v4 ALPN, keep-alives.
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
    transport.keep_alive_interval(Some(Duration::from_secs(10)));
    client_config.transport_config(Arc::new(transport));

    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())?;
    endpoint.set_default_client_config(client_config);
    let connection = endpoint
        .connect(relay.addr, pollis_relay::tls::RELAY_SERVER_NAME)?
        .await?;
    Ok((endpoint, connection))
}

/// Serve one spliced stream: unframe packets onto the loopback engine, frame
/// what comes back.
///
/// The two directions are separate tasks, never one `select!` — a framed read is
/// not cancel-safe, and a cancelled `read_exact` mid length-prefix would
/// resynchronise the tunnel onto garbage.
async fn serve_splice(mut send: quinn::SendStream, mut recv: quinn::RecvStream, engine: SocketAddr) {
    let Ok(socket) = UdpSocket::bind(("127.0.0.1", 0)).await else {
        return;
    };
    if socket.connect(engine).await.is_err() {
        return;
    }
    let socket = Arc::new(socket);

    let out = socket.clone();
    let reader = tokio::spawn(async move {
        while let Ok(Some(packet)) = read_tunnel_datagram(&mut recv).await {
            let _ = out.send(&packet).await;
        }
    });

    let mut buf = vec![0u8; 4096];
    while let Ok(n) = socket.recv(&mut buf).await {
        if write_tunnel_datagram(&mut send, &buf[..n]).await.is_err() {
            break;
        }
    }
    reader.abort();
}

// ---- loopback target -------------------------------------------------------

async fn spawn_echo() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            c.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    (addr, count)
}

async fn echo_through(stream: &mut BoxedStream, payload: &[u8]) {
    stream.write_all(payload).await.unwrap();
    let mut buf = vec![0u8; payload.len()];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, payload);
}

/// Wait, briefly, for the registry to settle on `expected` entries.
async fn await_parked(peers: &Arc<ParkedPeers>, expected: usize) {
    for _ in 0..200 {
        if peers.len() == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("registry never settled on {expected} parked peer(s), saw {}", peers.len());
}

fn assert_extend_failed(result: anyhow::Result<BoxedStream>, context: &str) {
    let err = match result {
        Ok(_) => panic!("{context}"),
        Err(e) => e,
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("ExtendFailed"),
        "expected a typed ExtendFailed rejection, got: {msg}"
    );
}

// ---- P1 --------------------------------------------------------------------

/// The whole feature, end to end: a circuit through a peer that could not
/// possibly have been dialled.
///
/// Three things are arranged so that only a splice can explain success:
/// the peer's advertised address is a **dead port**; the peer's engine is on
/// loopback with an **empty allowlist**, so it could not have served the
/// destination either; and the guard's allowlist is **empty** too, so it never
/// saw one. The counters then pin down which node did what.
#[tokio::test]
async fn p1_a_parked_peer_is_spliced_into_rather_than_dialled() {
    let (origin, origin_conns) = spawn_echo().await;

    // Guard: allows NOTHING as a destination — it can only ever extend.
    let guard = spawn_relay(&[], false, healthy_revocations());
    // Exit: the only first-party node that may reach the origin.
    let exit = spawn_relay(&[ORIGIN_NAME], true, healthy_revocations());
    let mut peer = TestPeer::start();
    peer.park(&guard).await;
    await_parked(&guard.parked, 1).await;

    let circuit = Circuit::build(
        vec![guard.hop(), peer.hop(), exit.hop()],
        client_identity(),
    )
    .unwrap();
    let mut stream = circuit
        .connect(ORIGIN_NAME, origin.port())
        .await
        .expect("a circuit through a parked peer builds");
    echo_through(&mut stream, b"through a parked peer").await;

    // The guard extended, and did so by splicing rather than dialling.
    assert_eq!(guard.stats.extends(), 1);
    assert_eq!(guard.stats.splices(), 1, "the extend must have been a splice");
    assert_eq!(guard.stats.connects(), 0, "the guard never saw a destination");
    assert_eq!(guard.stats.dials(), 0);

    // The peer forwarded and never learned a destination.
    assert_eq!(peer.splices_seen(), 1, "exactly one circuit was spliced in");
    assert!(peer.was_contacted(), "the splice must reach the peer's engine");
    assert_eq!(peer.stats.layers(), 1, "the peer terminated one onion layer");
    assert_eq!(peer.stats.extends(), 1);
    assert_eq!(peer.stats.connects(), 0);
    assert_eq!(peer.stats.dials(), 0);

    // The first-party exit is the one that talked to the origin.
    assert_eq!(exit.stats.connects(), 1);
    assert_eq!(exit.stats.dials(), 1);
    assert_eq!(origin_conns.load(Ordering::Relaxed), 1);

    // The exit was reached from the PEER's engine, not from the client and not
    // from the guard — the splice did not collapse the path.
    let seen = exit.peers.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "the exit saw exactly one predecessor");
    assert_ne!(seen[0], guard.addr);
}

// ---- P2 --------------------------------------------------------------------

/// A parked peer whose leaf cert is revoked is refused, and nothing is spliced.
///
/// The selector is the **cert digest**, not the address: a parked peer has no
/// dialable address to revoke, which is precisely the per-peer identity case
/// phase C built that selector for. Delete the parked branch of the revocation
/// check in `serve_extend` and this test fails.
#[tokio::test]
async fn p2_a_revoked_parked_peer_is_refused() {
    let (origin, origin_conns) = spawn_echo().await;
    let exit = spawn_relay(&[ORIGIN_NAME], true, healthy_revocations());

    // Stand the peer up FIRST, so the guard's list can name the very cert this
    // peer parks under. (Same peer, same cert — the revocation has to bite on
    // the identity that is actually parked.)
    let mut peer = TestPeer::start();
    let revoked = format!(
        r#"{{"cert_sha256_b64":"{}","reason":"seized"}}"#,
        cert_selector(&peer.cert)
    );
    let guard = spawn_relay(&[], false, store_revoking(&revoked));
    peer.park(&guard).await;
    await_parked(&guard.parked, 1).await;

    let circuit = Circuit::build(
        vec![guard.hop(), peer.hop(), exit.hop()],
        client_identity(),
    )
    .unwrap();
    assert_extend_failed(
        circuit.connect(ORIGIN_NAME, origin.port()).await,
        "a revoked peer must not be spliced into",
    );

    assert_eq!(guard.stats.splices(), 0);
    assert_eq!(guard.stats.extends(), 0);
    assert!(guard.stats.rejected() >= 1, "the refusal must be recorded");
    // The peer was never handed anything — and, the part only the PRE-splice
    // check can deliver, was never even reached for: no stream was opened on its
    // parked connection. Give anything in flight time to land first, so this is
    // about the refusal happening early rather than about winning a race.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        peer.splices_seen(),
        0,
        "a revoked peer must be refused before the splice, not after"
    );
    assert!(!peer.was_contacted());
    assert_eq!(peer.stats.layers(), 0);
    assert_eq!(peer.stats.extends(), 0);
    assert_eq!(origin_conns.load(Ordering::Relaxed), 0);
    // It is still parked — revocation refuses a splice; it does not garble the
    // registry.
    assert_eq!(guard.parked.len(), 1);
}

// ---- P3 --------------------------------------------------------------------

/// A parked connection that dies is evicted, and the fingerprint stops being
/// splice-able — no leaked entry, and no splicing into a corpse.
///
/// The follow-up `Extend` proves the eviction is real rather than cosmetic: with
/// nothing parked, the relay falls back to dialling the address the client gave,
/// which is dead, so it refuses.
#[tokio::test]
async fn p3_a_dead_parked_connection_is_evicted() {
    let (origin, _origin_conns) = spawn_echo().await;
    let guard = spawn_relay(&[], false, healthy_revocations());
    let exit = spawn_relay(&[ORIGIN_NAME], true, healthy_revocations());

    let mut peer = TestPeer::start();
    peer.park(&guard).await;
    await_parked(&guard.parked, 1).await;
    assert_eq!(guard.parked.fingerprints().len(), 1);

    // The peer goes offline.
    peer.unpark();
    await_parked(&guard.parked, 0).await;
    assert!(
        guard.parked.fingerprints().is_empty(),
        "a dead parking must leave no entry behind"
    );

    let circuit = Circuit::build(
        vec![guard.hop(), peer.hop(), exit.hop()],
        client_identity(),
    )
    .unwrap();
    assert_extend_failed(
        circuit.connect(ORIGIN_NAME, origin.port()).await,
        "an evicted peer must not still be reachable",
    );
    assert_eq!(guard.stats.splices(), 0);
}

// ---- P4 --------------------------------------------------------------------

/// A v3-negotiated stream cannot park, even though the frame is hand-rolled to
/// carry a v3 version byte — the gate is at the relay's reader, not only at our
/// writer.
///
/// The v4 half of the same exchange is P1, so this is pinning the version gate
/// rather than a malformed frame.
#[tokio::test]
async fn p4_a_v3_stream_cannot_park() {
    use quinn::crypto::rustls::QuicClientConfig;
    use pollis_relay::tls::{self, PinnedServerCertVerifier};

    let guard = spawn_relay(&[], false, healthy_revocations());
    let leaf = guard.cert.as_ref().to_vec();

    tls::ensure_crypto_provider();
    let mut client_crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(PinnedServerCertVerifier::new(guard.cert.clone()))
        .with_no_client_auth();
    // Offer ONLY v3, so the connection really is a v3 one.
    client_crypto.alpn_protocols = vec![b"pollis-relay/3".to_vec()];
    let client_config =
        quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(client_crypto).unwrap()));
    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(client_config);
    let connection = endpoint
        .connect(guard.addr, tls::RELAY_SERVER_NAME)
        .unwrap()
        .await
        .unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();

    // A genuine v3 handshake, then a hand-rolled v3 park frame (our writer
    // refuses to emit one, which is the point).
    let identity = client_identity();
    let handshake = identity.fresh_handshake().unwrap();
    let mut frame = Vec::new();
    proto::write_handshake(&mut frame, &handshake, 3).await.unwrap();
    frame.push(3);
    // MSG_PARK
    frame.push(6);
    frame.extend_from_slice(&(leaf.len() as u16).to_be_bytes());
    frame.extend_from_slice(&leaf);
    send.write_all(&frame).await.unwrap();
    send.flush().await.unwrap();

    // The relay answers with a typed rejection and parks nothing.
    let err = proto::read_response(&mut recv).await.unwrap_err();
    assert!(
        format!("{err:?}").contains("BadRequest"),
        "a v3 park must be refused as malformed, got: {err:?}"
    );
    assert!(
        guard.parked.fingerprints().is_empty(),
        "a v3 stream must not be able to park"
    );
}

// ---- P5 --------------------------------------------------------------------

/// Parking twice under one fingerprint leaves exactly one entry, and the
/// incumbent keeps serving: the second attempt is refused rather than silently
/// displacing a peer that is carrying traffic.
#[tokio::test]
async fn p5_parking_twice_does_not_displace_the_incumbent() {
    let guard = spawn_relay(&[], false, healthy_revocations());
    let mut peer = TestPeer::start();
    peer.park(&guard).await;
    await_parked(&guard.parked, 1).await;
    let fingerprints = guard.parked.fingerprints();

    // A second connection claiming the SAME leaf.
    let mut squatter = TestPeer::start();
    squatter.cert = peer.cert.clone();
    assert!(
        squatter.try_park(&guard).await.is_err(),
        "a second park under a live one must be refused"
    );

    assert_eq!(guard.parked.len(), 1, "no duplicate entry");
    assert_eq!(
        guard.parked.fingerprints(),
        fingerprints,
        "the incumbent's registration must survive"
    );
}

// ---- P6 --------------------------------------------------------------------

/// The health port publishes the parked peers in the shape P3's reconciler
/// reads — and nothing else about them. Not an address, not a client count, not
/// a byte total.
#[tokio::test]
async fn p6_the_health_port_publishes_parked_peers_only() {
    use tokio::net::TcpStream;

    let guard = spawn_relay(&[], false, healthy_revocations());
    let mut peer = TestPeer::start();
    peer.park(&guard).await;
    await_parked(&guard.parked, 1).await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let health_addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(pollis_relay::health::serve(
        listener,
        async move {
            let _ = rx.await;
        },
        guard.parked.clone(),
    ));

    let mut sock = TcpStream::connect(health_addr).await.unwrap();
    sock.write_all(b"GET /peers HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (_head, body) = text.split_once("\r\n\r\n").unwrap();

    // Exactly the reconciler's contract: `{"peers":[{"cert_b64":"<DER>"}]}`.
    let expected = B64.encode(peer.cert.as_ref());
    assert_eq!(body, format!("{{\"peers\":[{{\"cert_b64\":\"{expected}\"}}]}}"));
    // Nothing that could identify a user, a client or a volume of traffic.
    for leaked in ["127.0.0.1", USER, DEVICE, "count", "bytes", "clients"] {
        assert!(!body.contains(leaked), "health body leaked {leaked}: {body}");
    }

    let _ = tx.send(());
    let _ = served.await;
}
