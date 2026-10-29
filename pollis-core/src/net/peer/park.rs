//! **Parking** — the outbound connections that make a consenting device
//! reachable (#813 wave 3, contract §1/§2/§6).
//!
//! # The one connection that can exist
//!
//! A middle hop is reached by a first-party relay that can't hole-punch, so the **peer opens the connection outbound** and keeps it open.
//!
//! Concretely, per first-party relay in the signed directory:
//!
//! 1. dial it over QUIC, pinning its directory-published leaf cert;
//! 2. run the **normal device handshake** ([`ClientIdentity::fresh_handshake`]); no new auth;
//! 3. write one [`proto::Park`] frame carrying **this device's relay leaf DER**;
//! 4. read the acknowledgement, then accept the streams the relay opens.
//!
//! Every stream the relay then opens **is** a splice of an `Extend`, wrapped straight into a [`PeerLink`]. **Only who dialled whom changes.**
//!
//! # One codec, and it lives in `pollis-relay`
//!
//! The `Park` frame and tunnel framing are defined once in `pollis-relay`, so the two ends can't drift.
//!
//! # Park at every relay (contract §2)
//!
//! v1 parks at *every* first-party relay, so any guard reaches any peer; `parked_at` already allows subsets later.
//!
//! # What this module still refuses to do
//!
//! Parking makes a device **reachable**, nothing more: loopback, empty allowlist, revocation-gated, nothing inspected or recorded.
//!
//! # On keep-alives, and CLAUDE.md's polling ban
//!
//! Reconnection is driven by the connection **actually dropping**, with backoff and jitter; no sweeping timer.
//!
//! [`KEEP_ALIVE_INTERVAL`] is a QUIC PING, not app polling: without it an idle NAT binding silently dies.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pollis_relay::client::ClientIdentity;
use pollis_relay::park::{read_tunnel_datagram, write_tunnel_datagram};
use pollis_relay::proto::{self, Park};
use pollis_relay::tls::{self, PinnedServerCertVerifier, RELAY_SERVER_NAME};
use pollis_relay::CertificateDer;
use quinn::crypto::rustls::QuicClientConfig;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::link::{PeerLink, LINK_QUEUE_DEPTH};

/// Keep the NAT binding open (see the module docs).
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Four missed keep-alives: survives suspend or a Wi-Fi roam.
const PARK_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Bound on handshake + `Park` + ack, so a silent relay can't hold a slot.
const PARK_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// First re-dial delay after a parked connection is lost.
const RECONNECT_MIN: Duration = Duration::from_secs(1);

/// Re-dial ceiling; retries forever while consent stands.
const RECONNECT_MAX: Duration = Duration::from_secs(60);

/// One first-party relay this device parks a connection with.
#[derive(Debug, Clone)]
pub struct ParkedRelay {
    pub addr: SocketAddr,
    /// The relay's pinned leaf, from the **signed** directory.
    pub cert: CertificateDer<'static>,
}

/// Where an accepted splice goes (the serving manager, or a test).
#[async_trait::async_trait]
pub trait LinkAcceptor: Send + Sync + 'static {
    /// `false` when nothing runs; the tunnel is closed, not queued.
    async fn accept(&self, link: PeerLink) -> bool;
}

struct ParkingState {
    /// Live parked connections; `> 0` resolves `Waiting{NoInboundPath}`.
    live: AtomicUsize,
    /// Pushed whenever `live` changes.
    on_change: Option<Arc<dyn Fn(usize) + Send + Sync>>,
    acceptor: Arc<dyn LinkAcceptor>,
    identity: Arc<ClientIdentity>,
    leaf_der: CertificateDer<'static>,
}

impl ParkingState {
    fn notify(&self) {
        if let Some(hook) = self.on_change.as_ref() {
            hook(self.live.load(Ordering::Relaxed));
        }
    }

    /// A guard so the gauge self-heals however the task ends.
    fn live_guard(self: &Arc<Self>) -> LiveGuard {
        self.live.fetch_add(1, Ordering::Relaxed);
        self.notify();
        LiveGuard(self.clone())
    }
}

struct LiveGuard(Arc<ParkingState>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::Relaxed);
        self.0.notify();
    }
}

/// Keeps one parked connection per relay alive for as long as it exists.
///
/// Dropping it closes every parked connection immediately.
pub struct ParkingSupervisor {
    state: Arc<ParkingState>,
    tasks: Vec<JoinHandle<()>>,
    /// The relay set, so a refresh can tell whether anything changed.
    relays: Vec<SocketAddr>,
}

impl ParkingSupervisor {
    /// Start parking at every relay in `relays`.
    pub fn start(
        relays: Vec<ParkedRelay>,
        identity: Arc<ClientIdentity>,
        leaf_der: CertificateDer<'static>,
        acceptor: Arc<dyn LinkAcceptor>,
        on_change: Option<Arc<dyn Fn(usize) + Send + Sync>>,
    ) -> ParkingSupervisor {
        let state = Arc::new(ParkingState {
            live: AtomicUsize::new(0),
            on_change,
            acceptor,
            identity,
            leaf_der,
        });
        let addrs = relays.iter().map(|r| r.addr).collect();
        let tasks = relays
            .into_iter()
            .map(|relay| {
                let state = state.clone();
                tokio::spawn(async move {
                    park_forever(relay, state).await;
                })
            })
            .collect();
        ParkingSupervisor {
            state,
            tasks,
            relays: addrs,
        }
    }

    pub fn live(&self) -> usize {
        self.state.live.load(Ordering::Relaxed)
    }

    /// Contract §6: one live parked connection clears `NoInboundPath`.
    pub fn reachable(&self) -> bool {
        self.live() > 0
    }

    /// Parked relay addresses, sorted, for refresh comparison.
    pub fn relay_set(&self) -> Vec<SocketAddr> {
        let mut set = self.relays.clone();
        set.sort();
        set
    }
}

impl Drop for ParkingSupervisor {
    fn drop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}

/// Keep one relay parked forever: dial, park, serve splices until the connection
/// dies, back off, repeat.
///
/// Driven by the connection dropping; a successful park resets the backoff.
async fn park_forever(relay: ParkedRelay, state: Arc<ParkingState>) {
    let mut backoff = Backoff::new();
    loop {
        match park_once(&relay, &state).await {
            Ok(()) => {
                backoff.reset();
            }
            Err(e) => {
                crate::diag!("[peer-relay] parking at {} failed: {e}", relay.addr);
            }
        }
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

/// One parked-connection lifetime. Returns `Ok(())` only if the connection was
/// genuinely parked (acknowledged) and later ended.
async fn park_once(relay: &ParkedRelay, state: &Arc<ParkingState>) -> anyhow::Result<()> {
    // Held: dropping the endpoint tears down the connection.
    let (_endpoint, connection) = dial(relay).await?;
    let version = negotiated_version(&connection)?;
    if version < proto::PARK_MIN_VERSION {
        anyhow::bail!(
            "relay {} negotiated wire v{version}; parking needs v{}",
            relay.addr,
            proto::PARK_MIN_VERSION
        );
    }

    // Held open: it is the registration.
    let (mut send, mut recv) = connection.open_bi().await?;
    tokio::time::timeout(PARK_HANDSHAKE_TIMEOUT, async {
        let handshake = state.identity.fresh_handshake()?;
        proto::write_handshake(&mut send, &handshake, version).await?;
        let park = Park {
            relay_leaf_der: state.leaf_der.as_ref().to_vec(),
        };
        proto::write_park(&mut send, &park, version).await?;
        // `Ok` means parked; a rejection carries the reason.
        proto::read_response(&mut recv).await?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("relay {} did not acknowledge the park", relay.addr))??;

    let _guard = state.live_guard();

    loop {
        let (send, recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        let state = state.clone();
        tokio::spawn(async move {
            serve_splice(send, recv, state).await;
        });
    }
}

/// Open the outbound QUIC connection, pinning `relay.cert` as the relay's
/// Offers both ALPNs so an older relay fails the version check clearly.
async fn dial(relay: &ParkedRelay) -> anyhow::Result<(quinn::Endpoint, quinn::Connection)> {
    tls::ensure_crypto_provider();

    let mut client_crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(PinnedServerCertVerifier::new(relay.cert.clone()))
        .with_no_client_auth();
    client_crypto.alpn_protocols = proto::SUPPORTED_ALPNS.iter().map(|a| a.to_vec()).collect();

    let quic_crypto = QuicClientConfig::try_from(client_crypto)?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));

    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));
    transport.max_idle_timeout(Some(PARK_IDLE_TIMEOUT.try_into()?));
    client_config.transport_config(Arc::new(transport));

    let bind: SocketAddr = if relay.addr.is_ipv6() {
        "[::]:0".parse().expect("literal parses")
    } else {
        "0.0.0.0:0".parse().expect("literal parses")
    };
    let mut endpoint = quinn::Endpoint::client(bind)?;
    endpoint.set_default_client_config(client_config);
    let connection = endpoint.connect(relay.addr, RELAY_SERVER_NAME)?.await?;
    Ok((endpoint, connection))
}

/// Read the ALPN the QUIC handshake settled on and map it to a wire generation.
fn negotiated_version(connection: &quinn::Connection) -> anyhow::Result<u8> {
    let alpn = connection
        .handshake_data()
        .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
        .and_then(|data| data.protocol)
        .ok_or_else(|| anyhow::anyhow!("relay negotiated no ALPN"))?;
    proto::version_from_alpn(&alpn).ok_or_else(|| {
        anyhow::anyhow!(
            "relay negotiated an unsupported protocol: {}",
            String::from_utf8_lossy(&alpn)
        )
    })
}

/// Turn one spliced stream into a [`PeerLink`] and hand it to the engine.
///
/// Two tasks, not `select!`: the framed read isn't cancel-safe.
async fn serve_splice(
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    state: Arc<ParkingState>,
) {
    let (to_engine, from_relay) = mpsc::channel(LINK_QUEUE_DEPTH);
    let (to_relay, from_engine) = mpsc::channel(LINK_QUEUE_DEPTH);

    if !state.acceptor.accept(PeerLink::new(to_relay, from_relay)).await {
        // Refuse rather than queue.
        return;
    }

    let reader = tokio::spawn(async move {
        pump_stream_to_link(recv, to_engine).await;
    });
    pump_link_to_stream(send, from_engine).await;
    reader.abort();
}

/// Framed datagrams off the spliced stream, into the link.
async fn pump_stream_to_link(mut recv: quinn::RecvStream, to_engine: mpsc::Sender<Vec<u8>>) {
    loop {
        match read_tunnel_datagram(&mut recv).await {
            Ok(Some(datagram)) => {
                // Drop rather than block; QUIC recovers.
                let _ = to_engine.try_send(datagram);
            }
            Ok(None) | Err(_) => return,
        }
    }
}

/// Datagrams out of the link, framed onto the spliced stream.
async fn pump_link_to_stream(mut send: quinn::SendStream, mut from_engine: mpsc::Receiver<Vec<u8>>) {
    while let Some(datagram) = from_engine.recv().await {
        if write_tunnel_datagram(&mut send, &datagram).await.is_err() {
            return;
        }
    }
    let _ = send.finish();
}

/// Exponential backoff with full jitter, reset by a connection that actually
/// Jitter stops a fleet re-dialling in lockstep.
struct Backoff {
    next: Duration,
}

impl Backoff {
    fn new() -> Backoff {
        Backoff { next: RECONNECT_MIN }
    }

    fn reset(&mut self) {
        self.next = RECONNECT_MIN;
    }

    fn next_delay(&mut self) -> Duration {
        let ceiling = self.next;
        self.next = (self.next * 2).min(RECONNECT_MAX);
        let span = ceiling.saturating_sub(RECONNECT_MIN);
        if span.is_zero() {
            return ceiling;
        }
        RECONNECT_MIN + Duration::from_nanos(jitter_nanos() % (span.as_nanos() as u64 + 1))
    }
}

/// Cheap non-security jitter source.
fn jitter_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};
    use std::sync::Mutex as StdMutex;

    use ml_dsa::{Keypair, MlDsa44, SigningKey};
    use pollis_relay::circuit::{Circuit, Hop};
    use pollis_relay::proto::DeviceCertMaterial;
    use pollis_relay::server::{Allowlist, RelayConfig, RelayServer};
    use quinn::crypto::rustls::QuicServerConfig;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crate::net::peer::engine::{bridge_to_link, PeerCounters, PeerEngine};

    const USER: &str = "u_park";
    const DEVICE: &str = "d_park";
    const ORIGIN_NAME: &str = "origin.test";
    const DEVICE_ED_PUB: [u8; 32] = [7u8; 32];

    fn client_identity() -> Arc<ClientIdentity> {
        let device = SigningKey::<MlDsa44>::from_seed(&[7u8; 32].into());
        let account = SigningKey::<MlDsa44>::from_seed(&[5u8; 32].into());
        let cert = DeviceCertMaterial::mint(
            &account,
            DEVICE,
            &DEVICE_ED_PUB,
            &device.verifying_key().encode(),
            1,
            proto::now_unix() as u64,
        );
        Arc::new(ClientIdentity::new(
            USER,
            DEVICE,
            DEVICE_ED_PUB,
            device,
            cert,
        ))
    }

    /// Hands every accepted splice to one running peer node.
    struct EngineAcceptor(Arc<PeerEngine>);

    #[async_trait::async_trait]
    impl LinkAcceptor for EngineAcceptor {
        async fn accept(&self, link: PeerLink) -> bool {
            self.0.serve_link(link);
            true
        }
    }

    /// A stand-in first-party relay speaking only the parking wire.
    ///
    /// Uses `proto::read_client_frame`, so tests check the shared codec; the real server is tested in `tests/park.rs`.
    ///
    /// `splice` is `serve_extend` in miniature.
    struct StandInRelay {
        addr: SocketAddr,
        cert: CertificateDer<'static>,
        parked: Arc<StdMutex<HashMap<[u8; 32], quinn::Connection>>>,
        parks_seen: Arc<StdAtomicUsize>,
        _endpoint: quinn::Endpoint,
    }

    impl StandInRelay {
        fn start() -> StandInRelay {
            tls::ensure_crypto_provider();
            let identity = tls::generate_self_signed(RELAY_SERVER_NAME).unwrap();
            let cert = identity.cert_der.clone();

            let mut server_crypto = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![identity.cert_der], identity.key_der)
                .unwrap();
            server_crypto.alpn_protocols =
                proto::SUPPORTED_ALPNS.iter().map(|a| a.to_vec()).collect();
            let quic_crypto = QuicServerConfig::try_from(server_crypto).unwrap();
            let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
            let mut transport = quinn::TransportConfig::default();
            transport.max_idle_timeout(Some(PARK_IDLE_TIMEOUT.try_into().unwrap()));
            server_config.transport_config(Arc::new(transport));

            let endpoint =
                quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = endpoint.local_addr().unwrap();

            let parked = Arc::new(StdMutex::new(HashMap::new()));
            let parks_seen = Arc::new(StdAtomicUsize::new(0));

            let accept_endpoint = endpoint.clone();
            let accept_parked = parked.clone();
            let accept_seen = parks_seen.clone();
            tokio::spawn(async move {
                while let Some(incoming) = accept_endpoint.accept().await {
                    let parked = accept_parked.clone();
                    let seen = accept_seen.clone();
                    tokio::spawn(async move {
                        let Ok(connection) = incoming.await else {
                            return;
                        };
                        let Ok(version) = negotiated_version(&connection) else {
                            return;
                        };
                        let Ok((mut send, mut recv)) = connection.accept_bi().await else {
                            return;
                        };
                        let Ok(handshake) = proto::read_handshake(&mut recv).await else {
                            return;
                        };
                        if proto::verify_handshake(&handshake, proto::now_unix()).is_err() {
                            return;
                        }
                        // The real relay's reader, so a v3 `Park` is refused as unknown.
                        let Ok(proto::ClientFrame::Park(park)) =
                            proto::read_client_frame(&mut recv).await
                        else {
                            return;
                        };
                        parked.lock().unwrap().insert(
                            proto::cert_fingerprint(&park.relay_leaf_der),
                            connection.clone(),
                        );
                        seen.fetch_add(1, StdOrdering::Relaxed);
                        if proto::write_response(&mut send, Ok(()), version).await.is_err() {
                            return;
                        }
                        connection.closed().await;
                        parked
                            .lock()
                            .unwrap()
                            .retain(|_, c| c.stable_id() != connection.stable_id());
                        drop(send);
                    });
                }
            });

            StandInRelay {
                addr,
                cert,
                parked,
                parks_seen,
                _endpoint: endpoint,
            }
        }

        fn parked_fingerprints(&self) -> Vec<[u8; 32]> {
            self.parked.lock().unwrap().keys().copied().collect()
        }

        /// The `serve_extend` splice, bridged to a loopback UDP address for `Hop::addr`.
        async fn splice(&self, fingerprint: &[u8; 32]) -> Option<SocketAddr> {
            let connection = self.parked.lock().unwrap().get(fingerprint).cloned()?;
            let (send, recv) = connection.open_bi().await.ok()?;
            // The stream reaches the peer on the client's first packet.
            let (to_client, from_relay) = mpsc::channel(LINK_QUEUE_DEPTH);
            let (to_relay, from_client) = mpsc::channel(LINK_QUEUE_DEPTH);
            tokio::spawn(pump_stream_to_link(recv, to_client));
            tokio::spawn(pump_link_to_stream(send, from_client));
            let (addr, _handle) =
                bridge_to_link(PeerLink::new(to_relay, from_relay), PeerCounters::new())
                    .await
                    .ok()?;
            Some(addr)
        }
    }

    /// A real first-party relay the peer extends to.
    fn spawn_first_party_relay() -> (SocketAddr, CertificateDer<'static>, JoinHandle<()>) {
        let mut config = RelayConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            Allowlist::from_patterns([format!("{ORIGIN_NAME}:*")]),
        )
        .unwrap();
        config.revocations = crate::net::testing::healthy_revocations();
        config.next_hops = crate::net::testing::test_directory();
        config.allow_private_next_hops = true;
        config
            .resolve_overrides
            .insert(ORIGIN_NAME.to_string(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        let cert = config.server_cert();
        let (task, addr) = RelayServer::spawn(config).unwrap();
        crate::net::testing::publish_hop(addr);
        (addr, cert, task)
    }

    async fn spawn_echo() -> (SocketAddr, Arc<StdAtomicUsize>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(StdAtomicUsize::new(0));
        let c = count.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                c.fetch_add(1, StdOrdering::Relaxed);
                tokio::spawn(async move {
                    let (mut r, mut w) = sock.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        (addr, count)
    }

    /// Wait (bounded) for the network to settle; assertions are exact.
    async fn wait_for(label: &str, mut cond: impl FnMut() -> bool) {
        for _ in 0..600 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {label}");
    }

    /// **Contract §6.** Park, splice an `Extend`, and carry `client → parked peer → exit → origin`.
    ///
    /// `connects`/`dials` prove the peer stayed a middle hop.
    #[tokio::test]
    async fn a_parked_device_is_reachable_and_forwards() {
        let (origin, connections) = spawn_echo().await;
        let (exit_addr, exit_cert, _exit) = spawn_first_party_relay();
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
                crate::net::testing::test_directory(),
            )
            .unwrap(),
        );
        let relay = StandInRelay::start();

        let supervisor = ParkingSupervisor::start(
            vec![ParkedRelay {
                addr: relay.addr,
                cert: relay.cert.clone(),
            }],
            client_identity(),
            peer.cert_der().clone(),
            Arc::new(EngineAcceptor(peer.clone())),
            None,
        );

        wait_for("the device to park", || supervisor.reachable()).await;
        assert_eq!(supervisor.live(), 1);

        // Keyed on the LEAF fingerprint, never a user.
        let fingerprints = relay.parked_fingerprints();
        assert_eq!(fingerprints.len(), 1);
        assert_eq!(fingerprints[0], proto::cert_fingerprint(peer.cert_der()));

        let bridge = relay
            .splice(&fingerprints[0])
            .await
            .expect("the relay must be able to splice into a parked peer");

        let circuit = Circuit::build(
            vec![
                Hop::new(bridge, peer.cert_der().clone()),
                Hop::new(exit_addr, exit_cert),
            ],
            client_identity(),
        )
        .unwrap();
        let mut stream = circuit.connect(ORIGIN_NAME, origin.port()).await.unwrap();

        stream.write_all(b"through a parked peer").await.unwrap();
        let mut buf = [0u8; 21];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"through a parked peer");

        assert_eq!(peer.stats().extends(), 1);
        assert_eq!(peer.stats().connects(), 0);
        assert_eq!(peer.stats().dials(), 0);
        assert_eq!(connections.load(StdOrdering::Relaxed), 1);
    }

    /// A lost park recovers, driven by the drop.
    #[tokio::test]
    async fn losing_a_parked_connection_reconnects() {
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
                crate::net::testing::test_directory(),
            )
            .unwrap(),
        );
        let relay = StandInRelay::start();

        let supervisor = ParkingSupervisor::start(
            vec![ParkedRelay {
                addr: relay.addr,
                cert: relay.cert.clone(),
            }],
            client_identity(),
            peer.cert_der().clone(),
            Arc::new(EngineAcceptor(peer.clone())),
            None,
        );

        wait_for("the first park", || supervisor.reachable()).await;
        assert_eq!(relay.parks_seen.load(StdOrdering::Relaxed), 1);

        for connection in relay.parked.lock().unwrap().values() {
            connection.close(0u32.into(), b"rotated out");
        }
        wait_for("the device to notice it is unreachable", || {
            !supervisor.reachable()
        })
        .await;

        wait_for("the device to re-park", || {
            relay.parks_seen.load(StdOrdering::Relaxed) >= 2
        })
        .await;
        wait_for("reachability to come back", || supervisor.reachable()).await;
    }

    /// Contract §2: one parked connection per first-party relay.
    #[tokio::test]
    async fn the_device_parks_at_every_first_party_relay() {
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
                crate::net::testing::test_directory(),
            )
            .unwrap(),
        );
        let a = StandInRelay::start();
        let b = StandInRelay::start();

        let supervisor = ParkingSupervisor::start(
            vec![
                ParkedRelay {
                    addr: a.addr,
                    cert: a.cert.clone(),
                },
                ParkedRelay {
                    addr: b.addr,
                    cert: b.cert.clone(),
                },
            ],
            client_identity(),
            peer.cert_der().clone(),
            Arc::new(EngineAcceptor(peer.clone())),
            None,
        );

        wait_for("both relays to hold a park", || supervisor.live() == 2).await;
        assert_eq!(a.parked_fingerprints().len(), 1);
        assert_eq!(b.parked_fingerprints().len(), 1);
        assert_eq!(supervisor.relay_set().len(), 2);
    }

    /// Dropping the supervisor closes every parked connection.
    #[tokio::test]
    async fn dropping_the_supervisor_stops_the_device_being_reachable() {
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
                crate::net::testing::test_directory(),
            )
            .unwrap(),
        );
        let relay = StandInRelay::start();

        let supervisor = ParkingSupervisor::start(
            vec![ParkedRelay {
                addr: relay.addr,
                cert: relay.cert.clone(),
            }],
            client_identity(),
            peer.cert_der().clone(),
            Arc::new(EngineAcceptor(peer.clone())),
            None,
        );
        wait_for("the park", || supervisor.reachable()).await;

        drop(supervisor);
        wait_for("the relay to lose the parked entry", || {
            relay.parked_fingerprints().is_empty()
        })
        .await;
    }

    /// The live gauge is pushed, not polled.
    #[tokio::test]
    async fn parking_pushes_a_reachability_change() {
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
                crate::net::testing::test_directory(),
            )
            .unwrap(),
        );
        let relay = StandInRelay::start();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let recorder = seen.clone();

        let supervisor = ParkingSupervisor::start(
            vec![ParkedRelay {
                addr: relay.addr,
                cert: relay.cert.clone(),
            }],
            client_identity(),
            peer.cert_der().clone(),
            Arc::new(EngineAcceptor(peer.clone())),
            Some(Arc::new(move |live: usize| {
                recorder.lock().unwrap().push(live);
            })),
        );

        wait_for("the park", || supervisor.reachable()).await;
        assert_eq!(seen.lock().unwrap().as_slice(), &[1usize]);

        drop(supervisor);
        wait_for("the drop to be reported", || {
            seen.lock().unwrap().last() == Some(&0)
        })
        .await;
    }

    /// A relay with an unpinned cert is refused by the QUIC handshake.
    #[tokio::test]
    async fn parking_refuses_a_relay_that_is_not_the_pinned_one() {
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
                crate::net::testing::test_directory(),
            )
            .unwrap(),
        );
        let real = StandInRelay::start();
        let impostor = StandInRelay::start();

        let supervisor = ParkingSupervisor::start(
            vec![ParkedRelay {
                addr: impostor.addr,
                cert: real.cert.clone(),
            }],
            client_identity(),
            peer.cert_der().clone(),
            Arc::new(EngineAcceptor(peer.clone())),
            None,
        );

        tokio::time::sleep(Duration::from_millis(750)).await;
        assert!(!supervisor.reachable(), "parked at an unpinned relay");
        assert!(impostor.parked_fingerprints().is_empty());
    }

    /// Backoff grows, is bounded, and resets on a successful park.
    #[test]
    fn backoff_grows_stays_bounded_and_resets() {
        let mut backoff = Backoff::new();
        let first = backoff.next_delay();
        assert!(first >= RECONNECT_MIN && first <= RECONNECT_MIN * 2);

        for _ in 0..20 {
            let delay = backoff.next_delay();
            assert!(delay >= RECONNECT_MIN, "backoff dipped below the floor");
            assert!(delay <= RECONNECT_MAX, "backoff exceeded the ceiling");
        }

        backoff.reset();
        let after = backoff.next_delay();
        assert!(after <= RECONNECT_MIN * 2, "a reset backoff must re-dial fast");
    }
}
