//! **Parking** — the outbound connections that make a consenting device
//! reachable (#813 wave 3, contract §1/§2/§6).
//!
//! # The one connection that can exist
//!
//! A peer is a **middle** hop, so it is reached by the *previous relay*, which is
//! always first-party. A first-party relay cannot hole-punch to a NATed consumer
//! device and is not an ICE agent, so the only connection that can ever exist
//! between them is the one the **peer opens outbound**. This module opens it and
//! keeps it open.
//!
//! Concretely, per first-party relay in the signed directory:
//!
//! 1. dial it over QUIC, pinning its directory-published leaf cert;
//! 2. run the **normal device handshake** on a bi-stream ([`ClientIdentity::fresh_handshake`],
//!    verified relay-side by `proto::verify_handshake` — no new authentication
//!    mechanism, no second trust root);
//! 3. write one [`proto::Park`] frame carrying **this device's relay leaf DER**,
//!    the identity a client pins when it selects this peer as a hop;
//! 4. read the acknowledgement, then leave the connection parked and accept the
//!    streams the relay opens on it.
//!
//! Every stream the relay subsequently opens on a parked connection **is** a
//! splice: the relay is carrying out an `Extend` whose next hop is this device,
//! and rather than dialling (it cannot), it tunnels the next-hop QUIC through
//! that stream. So an accepted stream needs no preamble to be recognised — it is
//! wrapped straight into a [`PeerLink`] and handed to the running engine, which
//! is exactly the transport [`super::engine::PeerEngine::serve_link`] was
//! written against. **Nothing about the onion wire changes; only who dialled
//! whom.**
//!
//! # One codec, and it lives in `pollis-relay`
//!
//! The `Park` frame and the tunnel's length-delimited datagram framing are
//! defined **once**, on the relay side — [`proto::Park`] / [`proto::write_park`]
//! and [`pollis_relay::park::write_tunnel_datagram`] — and this module calls
//! them rather than restating the bytes. Both ends of a wire in one crate is
//! what stops the relay half and the device half drifting into two protocols
//! that no longer talk to each other.
//!
//! # Park at every relay (contract §2)
//!
//! v1 parks at *every* first-party relay in the directory rather than a subset.
//! The hosted pool is two nodes, so this is two idle QUIC connections. The
//! payoff is that any guard can reach any peer with no rendezvous lookup, no
//! directory of "which relay is this peer behind", and no failure mode where the
//! one relay a peer chose is the one relay the path did not select. The scaling
//! path (park at a subset; path selection targets the right relay) is already
//! representable — the directory's peer entry carries `parked_at` — so moving to
//! it later needs no schema change.
//!
//! # What this module still refuses to do
//!
//! Parking makes a device **reachable**; it does not widen what the device will
//! do once reached. The engine still binds loopback, still runs an empty
//! destination allowlist (so a `Connect` is refused before any socket opens),
//! and still cannot extend unless it can evaluate revocation. A parked
//! connection carries opaque bytes and this module never inspects, buffers by
//! content, or records anything about them — there is deliberately no per-link
//! identity, no counter keyed on who connected, and nothing persisted.
//!
//! # On keep-alives, and CLAUDE.md's polling ban
//!
//! Reconnection is driven by the connection **actually dropping** — a per-relay
//! task blocks on the live connection and only re-dials once it is gone, with
//! exponential backoff and jitter. There is no timer sweeping a table of
//! connections asking whether they are still up.
//!
//! The one periodic thing here is QUIC's own [`KEEP_ALIVE_INTERVAL`], which is
//! not app-level polling: a parked connection is idle by design, and a NAT/CGNAT
//! binding for an idle UDP flow is reclaimed in tens of seconds. Without a
//! transport keep-alive the connection silently stops being reachable while both
//! ends still believe it is live — the failure this whole module exists to
//! prevent. It is one QUIC PING frame on an otherwise idle path, and it stops
//! the moment the user withdraws consent.

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

/// Hold the parked connection's NAT binding open. See the module docs for why
/// this is not the polling CLAUDE.md bans.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// Give up on a parked connection that has heard nothing for this long. Four
/// missed keep-alives — long enough to ride out a suspend/resume or a Wi-Fi
/// roam, short enough that a dead path is noticed and re-dialled.
const PARK_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the handshake + `Park` + acknowledgement exchange gets before the
/// attempt is abandoned and retried. Without it a relay that accepts the QUIC
/// connection and then says nothing would hold a park slot forever.
const PARK_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// First re-dial delay after a parked connection is lost.
const RECONNECT_MIN: Duration = Duration::from_secs(1);

/// Ceiling on the re-dial delay. A device whose relay is down retries once a
/// minute forever rather than giving up — consent stays honoured, and the status
/// line keeps saying `Waiting` until it succeeds.
const RECONNECT_MAX: Duration = Duration::from_secs(60);

/// One first-party relay this device parks a connection with.
#[derive(Debug, Clone)]
pub struct ParkedRelay {
    pub addr: SocketAddr,
    /// The leaf the peer pins for this relay — from the **signed** directory,
    /// never from an unauthenticated source.
    pub cert: CertificateDer<'static>,
}

/// Where an accepted splice goes. Implemented by the serving manager (hand the
/// link to the running engine); a test supplies its own.
#[async_trait::async_trait]
pub trait LinkAcceptor: Send + Sync + 'static {
    /// Take one link. `false` means nothing is running to take it, in which case
    /// the tunnel is closed rather than queued.
    async fn accept(&self, link: PeerLink) -> bool;
}

/// Shared state for one supervisor's worth of park tasks.
struct ParkingState {
    /// Parked connections currently live. `> 0` is what resolves
    /// `Waiting{NoInboundPath}`.
    live: AtomicUsize,
    /// Pushed whenever [`ParkingState::live`] changes, so the status surface
    /// updates without anything polling it.
    on_change: Option<Arc<dyn Fn(usize) + Send + Sync>>,
    acceptor: Arc<dyn LinkAcceptor>,
    identity: Arc<ClientIdentity>,
    /// This device's relay leaf DER — the `Park` frame's whole payload.
    leaf_der: CertificateDer<'static>,
}

impl ParkingState {
    fn notify(&self) {
        if let Some(hook) = self.on_change.as_ref() {
            hook(self.live.load(Ordering::Relaxed));
        }
    }

    /// Count one parked connection as live until the guard drops. A guard rather
    /// than paired calls so the gauge self-heals however the task ends.
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
/// Dropping it aborts every park task and closes every parked connection, which
/// is how withdrawing consent stops the device being reachable immediately
/// rather than at the next tick.
pub struct ParkingSupervisor {
    state: Arc<ParkingState>,
    tasks: Vec<JoinHandle<()>>,
    /// The relay set this supervisor was built for, so a directory refresh can
    /// tell whether anything actually changed.
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

    /// Parked connections currently live.
    pub fn live(&self) -> usize {
        self.state.live.load(Ordering::Relaxed)
    }

    /// Whether anything can currently reach this device. This is the whole
    /// answer to contract §6: once one parked connection is up, a first-party
    /// relay has a path to this peer and `NoInboundPath` is no longer true.
    pub fn reachable(&self) -> bool {
        self.live() > 0
    }

    /// The relay addresses this supervisor is parking at, sorted — used to tell
    /// a directory refresh that changed nothing from one that did.
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
/// The delay is driven by the connection **actually dropping**, never by a timer
/// polling its state — the task spends its whole life blocked on the live
/// connection. A connection that was parked successfully resets the backoff, so
/// a transient drop re-dials in a second while a relay that is genuinely down is
/// retried at [`RECONNECT_MAX`].
async fn park_forever(relay: ParkedRelay, state: Arc<ParkingState>) {
    let mut backoff = Backoff::new();
    loop {
        match park_once(&relay, &state).await {
            Ok(()) => {
                // We were parked and then lost it: come back quickly.
                backoff.reset();
            }
            Err(e) => {
                eprintln!("[peer-relay] parking at {} failed: {e}", relay.addr);
            }
        }
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

/// One parked-connection lifetime. Returns `Ok(())` only if the connection was
/// genuinely parked (acknowledged) and later ended.
async fn park_once(relay: &ParkedRelay, state: &Arc<ParkingState>) -> anyhow::Result<()> {
    // `_endpoint` is held for the whole function: dropping a quinn endpoint tears
    // down every connection on it, and this function's body IS the parked
    // connection's lifetime.
    let (_endpoint, connection) = dial(relay).await?;
    let version = negotiated_version(&connection)?;
    if version < proto::PARK_MIN_VERSION {
        anyhow::bail!(
            "relay {} negotiated wire v{version}; parking needs v{}",
            relay.addr,
            proto::PARK_MIN_VERSION
        );
    }

    // The park stream is held open for the connection's whole life: it is the
    // registration, and the relay drops the parked entry when it closes.
    let (mut send, mut recv) = connection.open_bi().await?;
    tokio::time::timeout(PARK_HANDSHAKE_TIMEOUT, async {
        let handshake = state.identity.fresh_handshake()?;
        proto::write_handshake(&mut send, &handshake, version).await?;
        // The relay crate owns the codec; the DER is copied into the frame once
        // per parked connection, which is not a path that runs often.
        let park = Park {
            relay_leaf_der: state.leaf_der.as_ref().to_vec(),
        };
        proto::write_park(&mut send, &park, version).await?;
        // The acknowledgement is the existing Response frame: `Ok` means parked,
        // and a rejection carries the reason the relay refused.
        proto::read_response(&mut recv).await?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("relay {} did not acknowledge the park", relay.addr))??;

    let _guard = state.live_guard();

    // Every stream the relay opens from here IS a splice — see the module docs.
    loop {
        let (send, recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            // The connection ended: that is the event the reconnect is driven by.
            Err(_) => return Ok(()),
        };
        let state = state.clone();
        tokio::spawn(async move {
            serve_splice(send, recv, state).await;
        });
    }
}

/// Open the outbound QUIC connection, pinning `relay.cert` as the relay's
/// identity. Offers both supported ALPNs so a relay that has not yet rolled to
/// v4 fails the *version* check above with a clear message rather than failing
/// the QUIC handshake with an opaque one.
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
/// The two directions run as separate tasks rather than one `select!` loop
/// because the framed read is **not** cancel-safe: a dropped `read_exact` mid
/// length-prefix would resynchronise the tunnel onto garbage. Splitting the link
/// into its two channel halves is what makes that possible.
async fn serve_splice(
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    state: Arc<ParkingState>,
) {
    // relay → engine, and engine → relay.
    let (to_engine, from_relay) = mpsc::channel(LINK_QUEUE_DEPTH);
    let (to_relay, from_engine) = mpsc::channel(LINK_QUEUE_DEPTH);

    if !state.acceptor.accept(PeerLink::new(to_relay, from_relay)).await {
        // Nothing is running to carry it. Refuse rather than queue: a link the
        // device cannot serve must not look served.
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
                // A dropped datagram is a datagram; the tunnelled QUIC recovers.
                // Blocking the pipe on one slow reader would not.
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

// ── Backoff ─────────────────────────────────────────────────────────────────

/// Exponential backoff with full jitter, reset by a connection that actually
/// parked. Jitter matters with a two-node pool: without it every device that
/// loses a relay re-dials the replacement in lockstep.
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
        // Full jitter: anywhere in [MIN, ceiling].
        let span = ceiling.saturating_sub(RECONNECT_MIN);
        if span.is_zero() {
            return ceiling;
        }
        RECONNECT_MIN + Duration::from_nanos(jitter_nanos() % (span.as_nanos() as u64 + 1))
    }
}

/// A cheap, dependency-free jitter source. Nothing here is security-relevant —
/// it only has to stop a fleet re-dialling in lockstep.
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

    /// A stand-in first-party relay that speaks **only** the parking half of the
    /// wire: device handshake, `Park`, acknowledge, then splice on demand.
    ///
    /// It exists so this side can be exercised without standing up a real
    /// `RelayServer`. It reads the park with `proto::read_client_frame` — the
    /// same dispatcher the real relay uses — so what these tests check is that
    /// the device drives the shared codec correctly, not that two hand-written
    /// encodings happen to agree. (They used to: this module carried its own
    /// copy of the `Park` and tunnel codecs until the two were unified into
    /// `pollis-relay`, which is where the wire is now defined once. The
    /// cross-implementation check that copy provided is not lost — the relay's
    /// own `tests/park.rs` drives the real server with a stand-in *device*.)
    ///
    /// `splice` is the `serve_extend` branch in miniature: open a stream on the
    /// parked connection and tunnel the next-hop QUIC through it, instead of
    /// dialling an address the peer does not have.
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
                        // The normal device handshake — no new auth (contract §1).
                        let Ok(handshake) = proto::read_handshake(&mut recv).await else {
                            return;
                        };
                        if proto::verify_handshake(&handshake, proto::now_unix()).is_err() {
                            return;
                        }
                        // The same reader the real relay dispatches on, so the
                        // v4 gate is the crate's own: a `Park` on a
                        // v3-negotiated stream is refused as an unknown type.
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
                        // Hold the park stream open for the connection's life.
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

        /// The `serve_extend` splice: open a stream on the parked connection and
        /// bridge it to a loopback UDP address a QUIC client can dial. That
        /// address is what goes in `Hop::addr`; the peer's identity is still its
        /// pinned leaf, verified end to end inside the tunnel.
        async fn splice(&self, fingerprint: &[u8; 32]) -> Option<SocketAddr> {
            let connection = self.parked.lock().unwrap().get(fingerprint).cloned()?;
            let (send, recv) = connection.open_bi().await.ok()?;
            // The stream only reaches the peer once something is written on it,
            // which the client's very first QUIC packet does.
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

    /// A real first-party relay: allowlists the origin and resolves it to
    /// loopback. This is the hop a peer legitimately extends to.
    fn spawn_first_party_relay() -> (SocketAddr, CertificateDer<'static>, JoinHandle<()>) {
        let mut config = RelayConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            Allowlist::from_patterns([ORIGIN_NAME]),
        )
        .unwrap();
        config.revocations = crate::net::testing::healthy_revocations();
        config
            .resolve_overrides
            .insert(ORIGIN_NAME.to_string(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        let cert = config.server_cert();
        let (task, addr) = RelayServer::spawn(config).unwrap();
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

    /// Poll a condition with a wall-clock bound. Used only to wait for the
    /// network to settle; every assertion after it is exact.
    async fn wait_for(label: &str, mut cond: impl FnMut() -> bool) {
        for _ in 0..600 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {label}");
    }

    /// **Contract §6, the whole point of this phase.** A consenting device parks
    /// an outbound connection at a first-party relay, that relay splices an
    /// `Extend` into it, and a real circuit
    /// `client → parked peer → first-party exit → origin` carries bytes.
    ///
    /// The peer's allowlist is empty throughout, so had it been the hop that
    /// parsed the `Connect` the circuit would have failed — the assertions on
    /// `connects`/`dials` are what prove it stayed a middle hop.
    #[tokio::test]
    async fn a_parked_device_is_reachable_and_forwards() {
        let (origin, connections) = spawn_echo().await;
        let (exit_addr, exit_cert, _exit) = spawn_first_party_relay();
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
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

        // The relay registered the peer under its LEAF fingerprint and nothing
        // else — the parked map is keyed on infrastructure identity, never on a
        // user (contract's design ruling).
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

        // It extended, and it never learned a destination or opened a socket.
        assert_eq!(peer.stats().extends(), 1);
        assert_eq!(peer.stats().connects(), 0);
        assert_eq!(peer.stats().dials(), 0);
        assert_eq!(connections.load(StdOrdering::Relaxed), 1);
    }

    /// Losing a parked connection recovers, and the recovery is driven by the
    /// connection actually dropping rather than by a sweep.
    #[tokio::test]
    async fn losing_a_parked_connection_reconnects() {
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
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

        // Kill it from the relay's side, exactly as a node rotating out would.
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

    /// Contract §2: one parked connection per first-party relay, so any guard can
    /// reach any peer with no rendezvous lookup.
    #[tokio::test]
    async fn the_device_parks_at_every_first_party_relay() {
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
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

    /// Withdrawing consent drops the supervisor, which must close every parked
    /// connection — the device stops being reachable at the moment it stops
    /// being willing, not at some later tick.
    #[tokio::test]
    async fn dropping_the_supervisor_stops_the_device_being_reachable() {
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
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

    /// The live gauge is what resolves `NoInboundPath`, so it must be pushed
    /// rather than polled for.
    #[tokio::test]
    async fn parking_pushes_a_reachability_change() {
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
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

    /// A relay that presents a cert the device did not pin is refused by the QUIC
    /// handshake, so nothing is ever parked with it. Parking inherits the
    /// directory's pinning; it does not add a weaker path to the same relays.
    #[tokio::test]
    async fn parking_refuses_a_relay_that_is_not_the_pinned_one() {
        let peer = Arc::new(
            PeerEngine::start(
                PeerCounters::new(),
                crate::net::testing::healthy_revocations(),
            )
            .unwrap(),
        );
        let real = StandInRelay::start();
        let impostor = StandInRelay::start();

        let supervisor = ParkingSupervisor::start(
            // The impostor's address with the real relay's cert.
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

    /// Backoff grows, is bounded, and a successful park resets it — so a
    /// transient drop reconnects in about a second rather than a minute.
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
