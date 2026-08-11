//! The relay node a consenting device runs, plus the [`PeerLink`] ↔ QUIC bridge
//! that carries traffic to it.
//!
//! # Why it listens on loopback
//!
//! [`PeerEngine`] binds `127.0.0.1:0`. Nothing on the network can reach it: a
//! peer is reached over a link the device itself opened outbound (see the module
//! docs), and the bridge relays that link's datagrams to the loopback endpoint.
//! So a volunteer gains no externally-reachable port, no firewall prompt, and no
//! new attack surface from turning the feature on — and the engine's own
//! security properties (below) hold regardless of what is on the other end of
//! the link, because the QUIC/TLS terminates at the engine's own pinned leaf.
//!
//! # A peer cannot be an exit — structurally, not by policy
//!
//! The engine is configured with an **empty destination allowlist**.
//! `Allowlist::permits` returns false for every host when the pattern list is
//! empty, and the allowlist is checked on the `Connect` frame — the only frame
//! that names a destination — *before* any socket is opened. So a peer that is
//! handed a `Connect` refuses it with `NotAllowed` and never dials anything, no
//! matter who sent it or what path selection intended. `allow_extend` stays
//! `true`: extending to another **relay** is the peer's whole job, and `Extend`
//! carries a relay address plus a pinned cert fingerprint, never a destination.
//! `o1_*` in `pollis-relay/tests/onion.rs` proves the same property for
//! first-party middles; [`tests::a_peer_cannot_be_a_circuits_exit`] proves it for
//! this configuration.
//!
//! # Rate limiting
//!
//! Per-IP limits are disabled here and per-account limits do the work. Every
//! stream arrives through the bridge, so its source address is loopback and
//! identifies nothing; keying limits on it would lump every account behind one
//! bucket. This is the same reasoning (and the same outcome) as Phase A's
//! `admit_account_only` for streams that arrive via `Extend`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pollis_relay::server::{Allowlist, RelayConfig, RelayServer, RelayStats};
use pollis_relay::tls;
use pollis_relay::CertificateDer;
use pollis_relay::RateLimitConfig;
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::link::PeerLink;

/// Largest datagram the bridge will move. QUIC's own path-MTU ceiling is well
/// under this; the slack is there so a jumbo-frame path cannot silently truncate
/// a packet into a corrupt one.
const MAX_DATAGRAM: usize = 4096;

/// Drop a link that has carried nothing for this long. Not a poll — no work
/// happens on the deadline, and any traffic in either direction resets it. It
/// exists so a link whose far end vanished without closing cleanly does not sit
/// on a socket forever.
const LINK_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long in-flight circuits get to drain when the user turns relaying off.
/// Someone else's message is mid-flight; cutting it dead is the one failure this
/// feature must not cause.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Concurrent QUIC connections the engine will carry. A volunteer's laptop is
/// not a datacentre; the cap bounds what consenting costs.
const MAX_CONCURRENT_CONNECTIONS: u32 = 32;

/// Live counters for the status surface.
///
/// Both numbers describe traffic *volume*, never who sent it — a relay has no
/// way to know that, and recording it is exactly what §5.2/§11.7 forbid.
#[derive(Default)]
pub struct PeerCounters {
    active_links: AtomicU32,
    bytes_forwarded: AtomicU64,
    /// Invoked when the live-link count changes, so the manager can push a
    /// status event. Byte movement deliberately does NOT notify: that would be a
    /// per-datagram event storm for a number the UI renders in kB.
    on_change: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl PeerCounters {
    pub fn new() -> Arc<PeerCounters> {
        Arc::new(PeerCounters::default())
    }

    /// Links currently carrying traffic.
    ///
    /// Reported to the UI as `active_circuits`. One link is one QUIC connection
    /// from a first-party relay, which may carry more than one circuit —
    /// `pollis-relay`'s `RelayStats` exposes cumulative counters only, with no
    /// live gauge to read. A true circuit gauge is a one-line addition to
    /// `serve_authenticated` in `pollis-relay/src/server.rs`, which phase D1 does
    /// not own; it is filed as a cross-cutting request.
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

    /// Count a link as live until the returned guard drops. A guard rather than
    /// paired calls so the count self-heals when a pump ends for any reason,
    /// including a panic.
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

/// A running peer relay node.
///
/// Dropping it aborts the accept loop and every link pump immediately; call
/// [`PeerEngine::shutdown`] instead to let in-flight circuits drain first.
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
    /// Stand up the node. The identity is generated fresh per run: a peer's leaf
    /// cert only means anything once something has published it for clients to
    /// pin, and nothing publishes it yet, so persisting one would be storing a
    /// credential with no reader.
    ///
    /// `revocations` is not optional by accident. A peer's whole job is to
    /// `Extend` to the next hop, and a node that cannot evaluate revocation
    /// refuses to extend (#813 phase C, enforced in `serve_extend`) — so an
    /// engine handed [`RevocationStore::unconfigured`] forwards nothing at all.
    /// Taking it as a parameter makes that a decision at the call site instead
    /// of a silent default that looks like it works.
    pub fn start(
        counters: Arc<PeerCounters>,
        revocations: pollis_relay::policy::RevocationStore,
    ) -> anyhow::Result<PeerEngine> {
        let identity = tls::generate_self_signed(tls::RELAY_SERVER_NAME)?;
        // Empty allowlist — permits nothing. See the module docs: this is what
        // makes "a peer is never an exit" structural.
        let mut config = RelayConfig::with_identity(
            "127.0.0.1:0".parse().expect("loopback literal parses"),
            Allowlist::default(),
            identity,
        );
        // Forwarding to the next relay is the entire job.
        config.allow_extend = true;
        config.revocations = revocations;
        config.max_concurrent_connections = MAX_CONCURRENT_CONNECTIONS;
        config.rate_limits = RateLimitConfig {
            // Loopback source addresses identify nothing behind the bridge; the
            // per-account limits below are the real bound (see the module docs).
            new_circuits_per_min_per_ip: u32::MAX,
            max_concurrent_per_ip: u32::MAX,
            ..RateLimitConfig::default()
        };
        // Production nodes never record who connects to them (§5.2, §11.7), and
        // a volunteer's device least of all.
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

    /// The leaf cert a client pins to reach this node. Published through the
    /// signed directory once peer registration exists ([`super::discovery`]).
    pub fn cert_der(&self) -> &CertificateDer<'static> {
        &self.cert_der
    }

    /// Relay counters (`connects`, `extends`, `dials`, …). `connects` above zero
    /// with `dials` at zero is the shape a peer should always have: something
    /// asked it to be an exit and it refused.
    pub fn stats(&self) -> &Arc<RelayStats> {
        &self.stats
    }

    /// Start carrying traffic for one link. Runs until the link closes, goes
    /// idle, or the engine is dropped.
    pub fn serve_link(&self, link: PeerLink) {
        let counters = self.counters.clone();
        let quic = self.quic_addr;
        let handle = tokio::spawn(async move {
            if let Err(e) = pump_link_to_quic(link, quic, counters).await {
                eprintln!("[peer-relay] link ended: {e}");
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
/// QUIC endpoint at `quic`.
///
/// The socket is `connect`ed, so each link gets its own source port and the
/// engine sees one QUIC peer per link. Nothing here parses, buffers, or reorders
/// a datagram — it is a byte mover, which is the whole promise the consent
/// screen makes.
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
                    // Link closed, or idle for the whole timeout.
                    Ok(None) | Err(_) => return Ok(()),
                }
            }
            received = socket.recv(&mut buf) => {
                let n = received?;
                counters.add_bytes(n);
                // A dropped datagram is a datagram; QUIC recovers. Blocking the
                // whole pipe on one slow reader would not.
                let _ = link.try_send(buf[..n].to_vec());
            }
        }
    }
}

/// Client side of the bridge: bind a loopback UDP address that a QUIC client can
/// dial, and shuttle its datagrams over `link`.
///
/// The returned address is what goes in `Hop::addr`; the hop's *identity* is
/// still the peer's pinned leaf cert, verified end to end inside the tunnel, so
/// nothing carrying the link can impersonate the peer.
pub async fn bridge_to_link(
    link: PeerLink,
    counters: Arc<PeerCounters>,
) -> anyhow::Result<(SocketAddr, JoinHandle<()>)> {
    let socket = UdpSocket::bind(("127.0.0.1", 0)).await?;
    let addr = socket.local_addr()?;
    let handle = tokio::spawn(async move {
        if let Err(e) = pump_quic_to_link(socket, link, counters).await {
            eprintln!("[peer-relay] client bridge ended: {e}");
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
    // Whoever dialled us last. A QUIC client keeps one source port per
    // connection, so this is that connection's return path.
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

    /// A revocation store holding a freshly-signed list that revokes nobody.
    ///
    /// A node that cannot evaluate revocation refuses to extend circuits (#813
    /// phase C, wired into `serve_extend` by phase E2), so "keyed, with a current
    /// list" is the production shape of a middle hop and is what these tests must
    /// run. Without this every `Extend` here fails `ExtendFailed`. Mirrors
    /// `healthy_revocations()` in `pollis-relay/tests/onion.rs`; the unkeyed and
    /// revoked cases are covered directly in `pollis-relay/tests/revocation.rs`.
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
        let store = pollis_relay::policy::RevocationStore::enforcing(
            b64.encode(sk.verifying_key().to_bytes()),
        );
        store
            .install(&serde_json::to_vec(&envelope).unwrap(), now, 0)
            .expect("a freshly-signed empty list installs");
        store
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
            Allowlist::from_patterns([ORIGIN_NAME]),
        )
        .unwrap();
        config.revocations = healthy_revocations();
        config
            .resolve_overrides
            .insert(ORIGIN_NAME.to_string(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        let cert = config.server_cert();
        let stats = config.stats.clone();
        let (task, addr) = RelayServer::spawn(config).unwrap();
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

    /// The property the whole feature rests on: hand a peer a `Connect` and it
    /// refuses, having dialled nothing. Flip `Allowlist::default()` in
    /// [`PeerEngine::start`] to `["*"]` and this test fails.
    #[tokio::test]
    async fn a_peer_cannot_be_a_circuits_exit() {
        let (origin, connections) = spawn_echo().await;
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters, healthy_revocations()).unwrap();

        let circuit = Circuit::build_single_hop(
            Hop::new(peer.quic_addr(), peer.cert_der().clone()),
            client_identity(),
        );
        // A live, trivially reachable destination, so refusal is the ONLY reason
        // this can fail — an open allowlist here would succeed.
        let result = circuit.connect("127.0.0.1", origin.port()).await;

        assert!(result.is_err(), "a peer must refuse to reach a destination");
        // It parsed the Connect — and refused before opening any socket.
        assert_eq!(peer.stats().connects(), 1);
        assert_eq!(peer.stats().dials(), 0);
        assert_eq!(connections.load(Ordering::Relaxed), 0);
    }

    /// A first-party host name is not special-cased either: the allowlist is
    /// empty, so nothing is permitted, whatever the destination looks like.
    #[tokio::test]
    async fn a_peer_refuses_named_destinations_too() {
        let (origin, connections) = spawn_echo().await;
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters, healthy_revocations()).unwrap();

        let circuit = Circuit::build_single_hop(
            Hop::new(peer.quic_addr(), peer.cert_der().clone()),
            client_identity(),
        );
        assert!(circuit.connect(ORIGIN_NAME, origin.port()).await.is_err());
        assert_eq!(peer.stats().connects(), 1);
        assert_eq!(peer.stats().dials(), 0);
        assert_eq!(connections.load(Ordering::Relaxed), 0);
    }

    /// The job a peer *does* do: forward to the next relay without ever seeing
    /// the destination. The peer's allowlist is empty throughout, so if it had
    /// been the node that parsed the `Connect`, the circuit would have failed.
    #[tokio::test]
    async fn a_peer_forwards_as_a_non_terminal_hop() {
        let (origin, connections) = spawn_echo().await;
        let (fp_addr, fp_cert, fp_stats, _fp_task) = spawn_first_party_relay();
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters.clone(), healthy_revocations()).unwrap();

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

        // The peer extended and never learned a destination.
        assert_eq!(peer.stats().extends(), 1);
        assert_eq!(peer.stats().connects(), 0);
        assert_eq!(peer.stats().dials(), 0);
        // The first-party hop is the one that talked to the origin.
        assert_eq!(fp_stats.connects(), 1);
        assert_eq!(fp_stats.dials(), 1);
        assert_eq!(connections.load(Ordering::Relaxed), 1);
    }

    /// The same circuit, but the client reaches the peer only through a
    /// [`PeerLink`] datagram tunnel — the transport a peer is actually reached
    /// over. Proves the bridge carries a full onion circuit, and that the
    /// counters the UI shows are real.
    #[tokio::test]
    async fn a_full_circuit_rides_a_peer_link() {
        let (origin, connections) = spawn_echo().await;
        let (fp_addr, fp_cert, _fp_stats, _fp_task) = spawn_first_party_relay();
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters.clone(), healthy_revocations()).unwrap();

        // Tunnel: client bridge ⇄ link ⇄ peer engine. Nothing dials the peer's
        // loopback endpoint from outside this process.
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
        // Both ends of the tunnel share one counter here because both run in
        // this process; in production the client bridge and the peer pump are on
        // different devices and each counts one.
        assert_eq!(counters.active_links(), 2);
    }

    /// The engine still refuses to be an exit when reached over a link — the
    /// transport cannot launder a `Connect`.
    #[tokio::test]
    async fn a_peer_reached_over_a_link_still_refuses_to_exit() {
        let (origin, connections) = spawn_echo().await;
        let counters = PeerCounters::new();
        let peer = PeerEngine::start(counters.clone(), healthy_revocations()).unwrap();

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
        let peer = PeerEngine::start(counters.clone(), healthy_revocations()).unwrap();

        let (client_end, peer_end) = loopback_pair();
        peer.serve_link(peer_end);
        // Give the pump a moment to register.
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

        let peer = PeerEngine::start(counters.clone(), healthy_revocations()).unwrap();
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
        let mut peer = PeerEngine::start(counters, healthy_revocations()).unwrap();
        let addr = peer.quic_addr();
        peer.shutdown().await;

        // Nothing answers there any more: the dial either errors or never
        // completes, and both are "the node is gone".
        let circuit = Circuit::build_single_hop(
            Hop::new(addr, peer.cert_der().clone()),
            client_identity(),
        );
        let result =
            tokio::time::timeout(Duration::from_secs(5), circuit.connect(ORIGIN_NAME, 443)).await;
        assert!(!matches!(result, Ok(Ok(_))));
    }
}
