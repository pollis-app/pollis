//! The relay node a consenting device runs, plus the [`PeerLink`] ↔ QUIC bridge
//! that carries traffic to it.
//!
//! [`PeerEngine`] binds `127.0.0.1:0`; every byte arrives over a link the
//! device opened outbound, so turning the feature on opens no port, and TLS
//! terminates at the engine's own pinned leaf whatever carries the link.
//!
//! - **Never an exit**: the destination allowlist is empty, so every `Connect`
//!   is refused before a socket opens
//!   ([`tests::a_peer_cannot_be_a_circuits_exit`]).
//! - **Never a dialer**: `allow_extend` is on only with a configured
//!   [`NextHopDirectory`], and next hops must be public unicast, so `Extend`
//!   cannot make a volunteer's laptop dial into its own network.
//! - **Rate limits**: every source address is loopback, so per-IP limits
//!   discriminate nothing and per-account limits do the work. They stay finite
//!   (sized never to bind first) so they don't vanish if something other than
//!   the bridge ever reaches the engine.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pollis_relay::nexthop::NextHopDirectory;
use pollis_relay::server::{Allowlist, RelayConfig, RelayServer, RelayStats};
use pollis_relay::tls;
use pollis_relay::CertificateDer;
use pollis_relay::RateLimitConfig;
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::link::PeerLink;

/// Largest datagram the bridge will move; slack over QUIC's MTU so a jumbo
/// path cannot truncate a packet.
const MAX_DATAGRAM: usize = 4096;

/// Drop a link idle this long (no work runs on the deadline; any traffic
/// resets it), so a vanished far end doesn't hold a socket forever.
const LINK_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long in-flight circuits get to drain when relaying is turned off:
/// someone else's message is mid-flight.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Concurrent QUIC connections the engine will carry; bounds what consenting
/// costs a volunteer.
const MAX_CONCURRENT_CONNECTIONS: u32 = 32;

/// Concurrent circuits per source address. Effectively a second global cap
/// (all loopback), set above what [`MAX_CONCURRENT_CONNECTIONS`] can produce.
const MAX_CIRCUITS_PER_IP: u32 = 256;

/// New circuits per minute from one source address, on the same terms.
const NEW_CIRCUITS_PER_MIN_PER_IP: u32 = 600;

/// Live counters for the status surface: volume only, never who sent it
/// (§5.2, §11.7).
#[derive(Default)]
pub struct PeerCounters {
    active_links: AtomicU32,
    bytes_forwarded: AtomicU64,
    /// Invoked when the live-link count changes. Byte movement doesn't notify,
    /// to avoid a per-datagram event storm.
    on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl PeerCounters {
    pub fn new() -> Arc<PeerCounters> {
        Arc::new(PeerCounters::default())
    }

    /// Links currently carrying traffic, reported to the UI as
    /// `active_circuits`. One link may carry several circuits; `RelayStats` has
    /// no live circuit gauge.
    pub fn active_links(&self) -> u32 {
        self.active_links.load(Ordering::Relaxed)
    }

    pub fn bytes_forwarded(&self) -> u64 {
        self.bytes_forwarded.load(Ordering::Relaxed)
    }

    /// Register the change hook. Replaces any previous one.
    pub fn set_on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.on_change.lock().unwrap() = Some(hook);
    }

    fn add_bytes(&self, n: usize) {
        self.bytes_forwarded.fetch_add(n as u64, Ordering::Relaxed);
    }

    /// Count a link as live until the returned guard drops, so the count
    /// self-heals however a pump ends.
    fn link_guard(self: &Arc<Self>) -> LinkGuard {
        self.active_links.fetch_add(1, Ordering::Relaxed);
        self.notify();
        LinkGuard(self.clone())
    }

    fn notify(&self) {
        let hook = self.on_change.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

struct LinkGuard(Arc<PeerCounters>);

impl Drop for LinkGuard {
    fn drop(&mut self) {
        self.0.active_links.fetch_sub(1, Ordering::Relaxed);
        self.0.notify();
    }
}

/// A running peer relay node. Dropping it aborts everything immediately;
/// [`PeerEngine::shutdown`] lets in-flight circuits drain first.
pub struct PeerEngine {
    quic_addr: SocketAddr,
    cert_der: CertificateDer<'static>,
    stats: Arc<RelayStats>,
    counters: Arc<PeerCounters>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    pumps: Mutex<Vec<JoinHandle<()>>>,
}

impl PeerEngine {
    /// Stand up the node with a fresh identity per run (nothing persists a
    /// peer leaf yet).
    ///
    /// `revocations` and `next_hops` are required parameters: with the
    /// unconfigured form of either the node refuses every `Extend` (#813), and
    /// that should be a call-site decision, not a silent default.
    pub fn start(
        counters: Arc<PeerCounters>,
        revocations: pollis_relay::policy::RevocationStore,
        next_hops: NextHopDirectory,
    ) -> anyhow::Result<PeerEngine> {
        let identity = tls::generate_self_signed(tls::RELAY_SERVER_NAME)?;
        // Empty allowlist: a peer is never an exit.
        let mut config = RelayConfig::with_identity(
            "127.0.0.1:0".parse().expect("loopback literal parses"),
            Allowlist::default(),
            identity,
        );
        // Extend only to hops the signed directory names.
        config.allow_extend = next_hops.is_configured();
        config.next_hops = next_hops;
        // Next hops are public first-party relays; private addresses are
        // allowed only in this crate's test build.
        config.allow_private_next_hops = cfg!(test);
        config.revocations = revocations;
        config.max_concurrent_connections = MAX_CONCURRENT_CONNECTIONS;
        // Per-IP is effectively global here; matched to the global cap, since
        // `u32::MAX` is not a bound.
        config.max_connections_per_ip = MAX_CONCURRENT_CONNECTIONS;
        config.rate_limits = RateLimitConfig {
            // Per-account limits discriminate; these only bound.
            new_circuits_per_min_per_ip: NEW_CIRCUITS_PER_MIN_PER_IP,
            max_concurrent_per_ip: MAX_CIRCUITS_PER_IP,
            ..RateLimitConfig::default()
        };
        // Never record who connects (§5.2, §11.7).
        config.peer_observer = None;

        let cert_der = config.server_cert();
        let stats = config.stats.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (task, quic_addr) = RelayServer::spawn_with_shutdown(
            config,
            async move {
                let _ = shutdown_rx.await;
            },
            DRAIN_TIMEOUT,
        )?;

        Ok(PeerEngine {
            quic_addr,
            cert_der,
            stats,
            counters,
            shutdown: Some(shutdown_tx),
            task: Some(task),
            pumps: Mutex::new(Vec::new()),
        })
    }

    /// The loopback address the bridge feeds. Never reachable off-device.
    pub fn quic_addr(&self) -> SocketAddr {
        self.quic_addr
    }

    /// The leaf cert a client pins to reach this node.
    pub fn cert_der(&self) -> &CertificateDer<'static> {
        &self.cert_der
    }

    /// Relay counters. `connects > 0` with `dials == 0` is a peer's normal
    /// shape: asked to be an exit, it refused.
    pub fn stats(&self) -> &Arc<RelayStats> {
        &self.stats
    }

    /// Start carrying traffic for one link until it closes, goes idle, or the
    /// engine is dropped.
    pub fn serve_link(&self, link: PeerLink) {
        let counters = self.counters.clone();
        let quic = self.quic_addr;
        let handle = tokio::spawn(async move {
            if let Err(e) = pump_link_to_quic(link, quic, counters).await {
                crate::diag!("[peer-relay] link ended: {e}");
            }
        });
        let mut pumps = self.pumps.lock().unwrap();
        pumps.retain(|p| !p.is_finished());
        pumps.push(handle);
    }

    /// Stop accepting, let in-flight circuits drain, then stop.
    pub async fn shutdown(&mut self) {
        for pump in self.pumps.lock().unwrap().drain(..) {
            pump.abort();
        }
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for PeerEngine {
    fn drop(&mut self) {
        for pump in self.pumps.lock().unwrap().drain(..) {
            pump.abort();
        }
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Peer side of the bridge: shuttle datagrams between `link` and the loopback
/// QUIC endpoint. The socket is `connect`ed, so each link is its own QUIC peer.
/// A pure byte mover.
async fn pump_link_to_quic(
    mut link: PeerLink,
    quic: SocketAddr,
    counters: Arc<PeerCounters>,
) -> anyhow::Result<()> {
    let socket = UdpSocket::bind(("127.0.0.1", 0)).await?;
    socket.connect(quic).await?;
    let _guard = counters.link_guard();

    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        tokio::select! {
            incoming = tokio::time::timeout(LINK_IDLE_TIMEOUT, link.recv()) => {
                match incoming {
                    Ok(Some(datagram)) => {
                        counters.add_bytes(datagram.len());
                        let _ = socket.send(&datagram).await;
                    }
                    Ok(None) | Err(_) => return Ok(()),
                }
            }
            received = socket.recv(&mut buf) => {
                let n = received?;
                counters.add_bytes(n);
                // Drop rather than block the pipe; QUIC recovers.
                let _ = link.try_send(buf[..n].to_vec());
            }
        }
    }
}

/// Client side of the bridge: a loopback UDP address a QUIC client can dial,
/// shuttled over `link`. The hop's identity is still the peer's pinned leaf.
pub async fn bridge_to_link(
    link: PeerLink,
    counters: Arc<PeerCounters>,
) -> anyhow::Result<(SocketAddr, JoinHandle<()>)> {
    let socket = UdpSocket::bind(("127.0.0.1", 0)).await?;
    let addr = socket.local_addr()?;
    let handle = tokio::spawn(async move {
        if let Err(e) = pump_quic_to_link(socket, link, counters).await {
            crate::diag!("[peer-relay] client bridge ended: {e}");
        }
    });
    Ok((addr, handle))
}

async fn pump_quic_to_link(
    socket: UdpSocket,
    mut link: PeerLink,
    counters: Arc<PeerCounters>,
) -> anyhow::Result<()> {
    let _guard = counters.link_guard();
    let mut buf = vec![0u8; MAX_DATAGRAM];
    // The last sender: a QUIC client keeps one source port per connection.
    let mut client: Option<SocketAddr> = None;
    loop {
        tokio::select! {
            incoming = tokio::time::timeout(LINK_IDLE_TIMEOUT, link.recv()) => {
                match incoming {
                    Ok(Some(datagram)) => {
                        counters.add_bytes(datagram.len());
                        if let Some(peer) = client {
                            let _ = socket.send_to(&datagram, peer).await;
                        }
                    }
                    Ok(None) | Err(_) => return Ok(()),
                }
            }
            received = socket.recv_from(&mut buf) => {
                let (n, from) = received?;
                client = Some(from);
                counters.add_bytes(n);
                let _ = link.try_send(buf[..n].to_vec());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::AtomicUsize;

    use ml_dsa::{Keypair, MlDsa44, SigningKey};
    use pollis_relay::circuit::{Circuit, Hop};
    use pollis_relay::client::ClientIdentity;
    use pollis_relay::proto::DeviceCertMaterial;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crate::net::peer::link::loopback_pair;

    const USER: &str = "u_peer";
    const DEVICE: &str = "d_peer";
    const ORIGIN_NAME: &str = "origin.test";
    const ISSUED_AT: u64 = 1_700_000_000;
    const DEVICE_ED_PUB: [u8; 32] = [9u8; 32];

    fn client_identity() -> Arc<ClientIdentity> {
        let device = SigningKey::<MlDsa44>::from_seed(&[9u8; 32].into());
        let account = SigningKey::<MlDsa44>::from_seed(&[4u8; 32].into());
        let cert = DeviceCertMaterial::mint(
            &account,
            DEVICE,
            &DEVICE_ED_PUB,
            &device.verifying_key().encode(),
            1,
            ISSUED_AT,
        );
        Arc::new(ClientIdentity::new(
            USER,
            DEVICE,
            DEVICE_ED_PUB,
            device,
            cert,
        ))
    }

    /// A first-party relay: allowlists the origin and resolves it to loopback.
    fn spawn_first_party_relay() -> (
        SocketAddr,
        CertificateDer<'static>,
        Arc<RelayStats>,
        JoinHandle<()>,
    ) {
        let mut config = RelayConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            // The test origin listens on a random port.
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
        let stats = config.stats.clone();
        let (task, addr) = RelayServer::spawn(config).unwrap();
        // The peer extends only to addresses the signed directory names.
        crate::net::testing::publish_hop(addr);
        (addr, cert, stats, task)
    }

    /// Loopback TCP echo server.
    async fn spawn_echo() -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
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

    /// Hand a peer a `Connect` and it refuses, having dialled nothing. An open
    /// allowlist in [`PeerEngine::start`] fails this.
    #[tokio::test]
    async fn a_peer_cannot_be_a_circuits_exit() {
        let (origin, connections) = spawn_echo().await;
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters, crate::net::testing::healthy_revocations(), crate::net::testing::test_directory()).unwrap();

        let circuit = Circuit::build_single_hop(
            Hop::new(peer.quic_addr(), peer.cert_der().clone()),
            client_identity(),
        );
        // A live destination, so refusal is the only way this can fail.
        let result = circuit.connect("127.0.0.1", origin.port()).await;

        assert!(result.is_err(), "a peer must refuse to reach a destination");
        assert_eq!(peer.stats().connects(), 1);
        assert_eq!(peer.stats().dials(), 0);
        assert_eq!(connections.load(Ordering::Relaxed), 0);
    }

    /// Named first-party destinations are refused too.
    #[tokio::test]
    async fn a_peer_refuses_named_destinations_too() {
        let (origin, connections) = spawn_echo().await;
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters, crate::net::testing::healthy_revocations(), crate::net::testing::test_directory()).unwrap();

        let circuit = Circuit::build_single_hop(
            Hop::new(peer.quic_addr(), peer.cert_der().clone()),
            client_identity(),
        );
        assert!(circuit.connect(ORIGIN_NAME, origin.port()).await.is_err());
        assert_eq!(peer.stats().connects(), 1);
        assert_eq!(peer.stats().dials(), 0);
        assert_eq!(connections.load(Ordering::Relaxed), 0);
    }

    /// With no signed directory, an `Extend` to any address (even a real relay)
    /// is refused and nothing is dialled.
    #[tokio::test]
    async fn without_a_directory_a_peer_extends_to_nothing() {
        let (origin, connections) = spawn_echo().await;
        let (fp_addr, fp_cert, fp_stats, _fp_task) = spawn_first_party_relay();
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(
            counters.clone(),
            crate::net::testing::healthy_revocations(),
            pollis_relay::nexthop::NextHopDirectory::unconfigured(),
        )
        .unwrap();

        let circuit = Circuit::build(
            vec![
                Hop::new(peer.quic_addr(), peer.cert_der().clone()),
                Hop::new(fp_addr, fp_cert),
            ],
            client_identity(),
        )
        .unwrap();
        let err = match circuit.connect(ORIGIN_NAME, origin.port()).await {
            Ok(_) => panic!("a peer with no directory must not extend"),
            Err(e) => format!("{e:?}"),
        };
        assert!(err.contains("ExtendFailed"), "expected a typed refusal, got: {err}");

        assert_eq!(peer.stats().extends(), 0);
        assert_eq!(peer.stats().dials(), 0);
        assert_eq!(fp_stats.layers(), 0, "the next hop was never contacted");
        assert_eq!(connections.load(Ordering::Relaxed), 0);
    }

    /// The engine's per-IP bounds are finite.
    #[test]
    fn the_engines_per_ip_bounds_are_finite() {
        const {
            assert!(MAX_CIRCUITS_PER_IP < u32::MAX);
            assert!(NEW_CIRCUITS_PER_MIN_PER_IP < u32::MAX);
            assert!(MAX_CONCURRENT_CONNECTIONS < u32::MAX);
            // Const-evaluated, so `u32::MAX` fails the build.
            assert!(MAX_CIRCUITS_PER_IP >= MAX_CONCURRENT_CONNECTIONS);
        }
    }

    /// A peer forwards as a non-terminal hop without seeing the destination.
    #[tokio::test]
    async fn a_peer_forwards_as_a_non_terminal_hop() {
        let (origin, connections) = spawn_echo().await;
        let (fp_addr, fp_cert, fp_stats, _fp_task) = spawn_first_party_relay();
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters.clone(), crate::net::testing::healthy_revocations(), crate::net::testing::test_directory()).unwrap();

        let circuit = Circuit::build(
            vec![
                Hop::new(peer.quic_addr(), peer.cert_der().clone()),
                Hop::new(fp_addr, fp_cert),
            ],
            client_identity(),
        )
        .unwrap();
        let mut stream = circuit.connect(ORIGIN_NAME, origin.port()).await.unwrap();

        stream.write_all(b"through the peer").await.unwrap();
        let mut buf = [0u8; 16];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"through the peer");

        assert_eq!(peer.stats().extends(), 1);
        assert_eq!(peer.stats().connects(), 0);
        assert_eq!(peer.stats().dials(), 0);
        assert_eq!(fp_stats.connects(), 1);
        assert_eq!(fp_stats.dials(), 1);
        assert_eq!(connections.load(Ordering::Relaxed), 1);
    }

    /// The same circuit with the client reaching the peer only through a
    /// [`PeerLink`]; also checks the UI counters.
    #[tokio::test]
    async fn a_full_circuit_rides_a_peer_link() {
        let (origin, connections) = spawn_echo().await;
        let (fp_addr, fp_cert, _fp_stats, _fp_task) = spawn_first_party_relay();
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters.clone(), crate::net::testing::healthy_revocations(), crate::net::testing::test_directory()).unwrap();

        let (client_end, peer_end) = loopback_pair();
        peer.serve_link(peer_end);
        let (bridge_addr, _bridge) = bridge_to_link(client_end, counters.clone())
            .await
            .unwrap();

        let circuit = Circuit::build(
            vec![
                Hop::new(bridge_addr, peer.cert_der().clone()),
                Hop::new(fp_addr, fp_cert),
            ],
            client_identity(),
        )
        .unwrap();
        let mut stream = circuit.connect(ORIGIN_NAME, origin.port()).await.unwrap();

        stream.write_all(b"tunnelled").await.unwrap();
        let mut buf = [0u8; 9];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"tunnelled");

        assert_eq!(peer.stats().extends(), 1);
        assert_eq!(peer.stats().connects(), 0);
        assert_eq!(peer.stats().dials(), 0);
        assert_eq!(connections.load(Ordering::Relaxed), 1);
        assert!(
            counters.bytes_forwarded() > 0,
            "the bridge must account for what it moved"
        );
        // Both tunnel ends share one counter in-process; in production each
        // device counts one.
        assert_eq!(counters.active_links(), 2);
    }

    /// Reached over a link, the engine still refuses to exit.
    #[tokio::test]
    async fn a_peer_reached_over_a_link_still_refuses_to_exit() {
        let (origin, connections) = spawn_echo().await;
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters.clone(), crate::net::testing::healthy_revocations(), crate::net::testing::test_directory()).unwrap();

        let (client_end, peer_end) = loopback_pair();
        peer.serve_link(peer_end);
        let (bridge_addr, _bridge) = bridge_to_link(client_end, counters.clone())
            .await
            .unwrap();

        let circuit = Circuit::build_single_hop(
            Hop::new(bridge_addr, peer.cert_der().clone()),
            client_identity(),
        );
        assert!(circuit.connect("127.0.0.1", origin.port()).await.is_err());
        assert_eq!(peer.stats().dials(), 0);
        assert_eq!(connections.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn link_counters_return_to_zero_when_a_link_ends() {
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters.clone(), crate::net::testing::healthy_revocations(), crate::net::testing::test_directory()).unwrap();

        let (client_end, peer_end) = loopback_pair();
        peer.serve_link(peer_end);
        for _ in 0..50 {
            if counters.active_links() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(counters.active_links(), 1);

        drop(client_end);
        for _ in 0..100 {
            if counters.active_links() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(counters.active_links(), 0);
    }

    #[tokio::test]
    async fn the_change_hook_fires_when_links_come_and_go() {
        let counters = PeerCounters::new();
        let fired = Arc::new(AtomicUsize::new(0));
        let f = fired.clone();
        counters.set_on_change(Arc::new(move || {
            f.fetch_add(1, Ordering::Relaxed);
        }));

        let peer = PeerEngine::start(counters.clone(), crate::net::testing::healthy_revocations(), crate::net::testing::test_directory()).unwrap();
        let (client_end, peer_end) = loopback_pair();
        peer.serve_link(peer_end);
        for _ in 0..50 {
            if fired.load(Ordering::Relaxed) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(fired.load(Ordering::Relaxed) > 0);
        drop(client_end);
    }

    #[tokio::test]
    async fn shutdown_stops_the_node() {
        let counters = PeerCounters::new();
        let mut peer = PeerEngine::start(counters, crate::net::testing::healthy_revocations(), crate::net::testing::test_directory()).unwrap();
        let addr = peer.quic_addr();
        peer.shutdown().await;

        // Nothing answers: an error or a hang both mean "gone".
        let circuit = Circuit::build_single_hop(
            Hop::new(addr, peer.cert_der().clone()),
            client_identity(),
        );
        let result =
            tokio::time::timeout(Duration::from_secs(5), circuit.connect(ORIGIN_NAME, 443)).await;
        assert!(!matches!(result, Ok(Ok(_))));
    }
}
