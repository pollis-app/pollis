//! Parked peer relays: the reverse hop (#813 Phase D1/P1, design §7.1).
//!
//! # Why a peer connection has to be parked
//!
//! A peer-hosted relay is a consumer device behind NAT or CGNAT. A first-party
//! relay cannot dial it, and neither end is an ICE agent, so hole-punching was
//! never on the table between them. The one connection that always works is the
//! one the **peer opens outbound**. So a peer dials each first-party relay it
//! knows from the signed directory, runs the ordinary device handshake, sends a
//! [`crate::proto::Park`] naming the relay leaf cert it serves under, and leaves
//! the connection open. A later `Extend` naming that leaf's fingerprint is
//! **spliced into that connection** instead of dialling. Nothing about the onion
//! wire changes — only who dialled whom.
//!
//! # What is in the map, and what is deliberately not
//!
//! `sha256(relay_leaf_der) -> { the live connection, that DER }`.
//! Nothing else. No client identity, no addresses, no counts, no traffic
//! metadata, no persistence, and no logging of who connected. §5.2/§11.7 forbid
//! a relay recording **who its users are**; a parked peer is *infrastructure*
//! whose identity is already public in the signed directory (it has to be, or
//! clients could not pin its cert). An open socket cannot live in S3, so the map
//! itself is irreducible — but it holds only what splicing into that socket
//! needs:
//!
//! - the **connection**, because that IS the parked socket;
//! - the **DER leaf**, because live revocation's `cert_sha256_b64` selector takes
//!   the preimage, not the digest, and checking only the address would fail
//!   *open* for exactly the per-peer identities that selector exists for. It is
//!   the fingerprint's preimage and is public directory data either way.
//!
//! Entries are dropped the moment the parking stream ends — which a dead
//! connection also ends — so nothing accumulates.
//!
//! # Splicing: a QUIC connection inside a QUIC connection
//!
//! The peer runs its relay engine on **loopback** and bridges it to the parked
//! connection (`pollis-core`'s `net::peer::park`). This module is the mirror
//! image on the relay side: [`ParkedPeer::splice`] opens **one bi-stream on the
//! parked connection per circuit** and bridges it to a loopback UDP socket, so
//! the relay's ordinary `Extend` machinery can dial *that* address with the same
//! pinned-fingerprint client config it would have used on a real dial.
//!
//! Every stream the relay opens on a parked connection **is** a splice — no
//! preamble, no discriminator, because that connection has no other use. Each
//! carries the next hop's QUIC packets length-delimited
//! ([`write_tunnel_datagram`]): `u16` big-endian length, then that many bytes,
//! ceiling [`MAX_TUNNEL_DATAGRAM`]. `pollis-core`'s device side reads and writes
//! the identical framing.
//!
//! Three things fall out of dialling a loopback socket, all load-bearing:
//!
//! 1. **The fingerprint pin still binds.** The tunnelled TLS terminates at the
//!    peer's own leaf, so a device that parked under a fingerprint it does not
//!    hold the private key for fails the handshake and carries nothing. That is
//!    what makes `Park` safe without a new credential: possession is proven by
//!    use, not by the frame.
//! 2. **Revocation still binds** — checked before the splice on the full
//!    identity, and again on the leaf actually presented.
//! 3. **The onion wire is unchanged.** The peer sees a `Layer` frame arriving on
//!    a QUIC connection, exactly as a dialled relay does.
//!
//! One stream per circuit, rather than one shared pipe: head-of-line blocking
//! then reaches no further than the circuit that caused it, and a circuit ending
//! closes exactly its own stream.
//!
//! The two directions of a splice run as **separate tasks, never one `select!`**.
//! A framed read is not cancel-safe — dropping a `read_exact` midway through a
//! length prefix resynchronises the tunnel onto garbage — and `select!` cancels
//! the loser of every iteration.
//!
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use crate::proto::cert_fingerprint;

/// SHA-256 of a parked peer's DER relay leaf — the map key, and the same value
/// an `Extend` carries in `next_cert_sha256`.
pub type PeerFingerprint = [u8; 32];

/// Ceiling on one tunnelled packet. Well above any QUIC packet a tunnelled
/// connection will emit (quinn probes no higher than 1452), so the bound exists
/// to stop a peer forcing an allocation, not to constrain the tunnel. Matches
/// `pollis-core`'s `net::peer::park::MAX_TUNNEL_DATAGRAM` — the two sides read
/// and write the same frames, so the ceiling has to agree.
pub const MAX_TUNNEL_DATAGRAM: usize = 4096;

/// Every peer currently parked at this node.
///
/// Cloneable behind an `Arc`, like [`crate::server::RelayStats`]: the server
/// holds one, and the health endpoint reads the fingerprints out of the same
/// one so what an operator (and the directory reconciler) sees is what the relay
/// would actually splice into.
#[derive(Default)]
pub struct ParkedPeers {
    map: Mutex<HashMap<PeerFingerprint, ParkedEntry>>,
}

/// One parked peer. See the module docs for why each field is here and why there
/// is not a fourth.
struct ParkedEntry {
    connection: quinn::Connection,
    leaf_der: Arc<Vec<u8>>,
}

impl ParkedPeers {
    pub fn new() -> Arc<ParkedPeers> {
        Arc::new(ParkedPeers::default())
    }

    /// Register `connection` as the way to reach the relay identity
    /// `leaf_der`, returning a registration that unparks it when dropped.
    ///
    /// `None` means the fingerprint is already parked here by a **live**
    /// connection, and the caller must refuse. First registration wins for as
    /// long as it lasts: last-wins would let any authenticated device evict a
    /// genuine peer at will, whereas an incumbent that is actually serving keeps
    /// serving. Neither ordering can be gamed into *intercepting* traffic — a
    /// squatter cannot complete the pinned handshake (module docs) — so the
    /// choice is only about which denial is easier, and this is the harder one.
    pub(crate) fn park(
        self: &Arc<Self>,
        leaf_der: Vec<u8>,
        connection: quinn::Connection,
    ) -> Option<ParkRegistration> {
        let fingerprint = cert_fingerprint(&leaf_der);
        let mut map = self.lock();
        // A live parked connection for this identity wins: the newcomer is
        // refused rather than allowed to displace it.
        let still_live = map
            .get(&fingerprint)
            .is_some_and(|existing| existing.connection.close_reason().is_none());
        if still_live {
            return None;
        }
        let registration = ParkRegistration {
            peers: self.clone(),
            fingerprint,
            connection_id: connection.stable_id(),
        };
        map.insert(
            fingerprint,
            ParkedEntry {
                connection,
                leaf_der: Arc::new(leaf_der),
            },
        );
        Some(registration)
    }

    /// The parked peer serving `fingerprint`, if one is here and still live.
    pub(crate) fn lookup(&self, fingerprint: &PeerFingerprint) -> Option<ParkedPeer> {
        let map = self.lock();
        let entry = map.get(fingerprint)?;
        // A connection that has closed but whose registration has not been
        // reaped yet is not a next hop. Splicing into it would fail anyway; this
        // just makes the answer immediate.
        if entry.connection.close_reason().is_some() {
            return None;
        }
        Some(ParkedPeer {
            connection: entry.connection.clone(),
            leaf_der: entry.leaf_der.clone(),
        })
    }

    /// The fingerprints parked right now, sorted.
    pub fn fingerprints(&self) -> Vec<PeerFingerprint> {
        let mut out: Vec<PeerFingerprint> = self.lock().keys().copied().collect();
        out.sort_unstable();
        out
    }

    /// The parked peers' own DER leaf certs, ordered by fingerprint.
    ///
    /// This — plus the fingerprints above, which are just its digest — is the
    /// **entire** external view of the registry, and it is what the health
    /// endpoint publishes for the directory reconciler. A peer's leaf is public
    /// by construction: the signed directory has to carry it or a client could
    /// not pin the peer's cert, so surfacing it here is not a new disclosure.
    ///
    /// Nothing else may follow it out: not who is connected, not how many
    /// clients a peer is serving, not a byte count, not an address.
    pub fn peer_leaves(&self) -> Vec<Vec<u8>> {
        let map = self.lock();
        let mut entries: Vec<(&PeerFingerprint, &ParkedEntry)> = map.iter().collect();
        entries.sort_unstable_by_key(|(fingerprint, _)| *fingerprint);
        entries
            .into_iter()
            .map(|(_, entry)| entry.leaf_der.as_ref().clone())
            .collect()
    }

    /// How many peers are parked. Infrastructure, not clientele.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PeerFingerprint, ParkedEntry>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for ParkedPeers {
    /// Deliberately only a count: a `Debug` that printed the map would be a
    /// logging path for exactly what this registry must not log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParkedPeers")
            .field("parked", &self.len())
            .finish()
    }
}

/// Holds a peer's registration for as long as its parking stream lives. Dropping
/// it unparks — so a closed stream, a dead connection, an error path and a
/// cancelled task all reap the entry identically, and none of them can leak one.
pub(crate) struct ParkRegistration {
    peers: Arc<ParkedPeers>,
    fingerprint: PeerFingerprint,
    connection_id: usize,
}

impl Drop for ParkRegistration {
    fn drop(&mut self) {
        let mut map = self.peers.lock();
        // Only reap our OWN registration: if this fingerprint has since been
        // re-parked by a different connection, that one is live and is not ours
        // to remove.
        if let Some(entry) = map.get(&self.fingerprint) {
            if entry.connection.stable_id() == self.connection_id {
                map.remove(&self.fingerprint);
            }
        }
    }
}

/// A live parked peer, resolved from the registry.
pub(crate) struct ParkedPeer {
    connection: quinn::Connection,
    leaf_der: Arc<Vec<u8>>,
}

impl ParkedPeer {
    /// The DER leaf this peer parked under — the identity live revocation is
    /// evaluated against before anything is spliced.
    pub(crate) fn leaf_der(&self) -> &[u8] {
        &self.leaf_der
    }

    /// Splice one circuit into this peer: open a stream on the parked connection
    /// and bridge it to a loopback UDP address the relay's `Extend` machinery can
    /// dial as if it were the peer's own.
    ///
    /// The stream does not reach the peer until something is written on it —
    /// QUIC creates a stream lazily — which the tunnelled connection's first
    /// packet does.
    pub(crate) async fn splice(&self) -> anyhow::Result<Tunnel> {
        let (send, recv) = self.connection.open_bi().await?;
        let socket = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?);
        let addr = socket.local_addr()?;

        // Where to send what comes back. The relay dials this socket from its own
        // extend endpoint, which keeps one source port per connection, and it
        // always speaks first — so the return path is known before any reply.
        let hop: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));

        let to_peer = tokio::spawn(pump_socket_to_stream(
            socket.clone(),
            send,
            hop.clone(),
        ));
        let from_peer = tokio::spawn(pump_stream_to_socket(recv, socket, hop));

        Ok(Tunnel {
            addr,
            pumps: [to_peer, from_peer],
        })
    }
}

/// One spliced circuit: a loopback UDP socket whose traffic is carried, framed,
/// on one stream of a parked connection.
pub(crate) struct Tunnel {
    addr: SocketAddr,
    pumps: [JoinHandle<()>; 2],
}

impl Tunnel {
    /// The address the relay's `Extend` machinery dials instead of the peer's
    /// (unreachable) real one.
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        for pump in &self.pumps {
            pump.abort();
        }
    }
}

/// Loopback socket → spliced stream. Also learns the return path.
async fn pump_socket_to_stream(
    socket: Arc<UdpSocket>,
    mut send: quinn::SendStream,
    hop: Arc<Mutex<Option<SocketAddr>>>,
) {
    let mut buf = vec![0u8; MAX_TUNNEL_DATAGRAM];
    loop {
        let Ok((n, from)) = socket.recv_from(&mut buf).await else {
            return;
        };
        *hop.lock().unwrap_or_else(|e| e.into_inner()) = Some(from);
        if write_tunnel_datagram(&mut send, &buf[..n]).await.is_err() {
            return;
        }
    }
}

/// Spliced stream → loopback socket.
async fn pump_stream_to_socket(
    mut recv: quinn::RecvStream,
    socket: Arc<UdpSocket>,
    hop: Arc<Mutex<Option<SocketAddr>>>,
) {
    loop {
        match read_tunnel_datagram(&mut recv).await {
            Ok(Some(datagram)) => {
                let target = *hop.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(target) = target {
                    let _ = socket.send_to(&datagram, target).await;
                }
            }
            // Clean end of stream, or a frame we cannot trust to resynchronise
            // from. Either way this splice is over; the circuit on it fails and
            // the client rebuilds.
            Ok(None) | Err(_) => {
                return;
            }
        }
    }
}

/// Write one tunnelled packet: `u16` big-endian length, then that many bytes.
///
/// `pollis-core`'s device side reads exactly this, so the encoding is shared
/// wire, not an internal detail. `len == 0` is legal and means an empty packet,
/// not an end of stream.
pub async fn write_tunnel_datagram<W: AsyncWrite + Unpin>(
    w: &mut W,
    datagram: &[u8],
) -> std::io::Result<()> {
    if datagram.len() > MAX_TUNNEL_DATAGRAM {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "tunnelled datagram exceeds the ceiling",
        ));
    }
    w.write_u16(datagram.len() as u16).await?;
    w.write_all(datagram).await?;
    w.flush().await?;
    Ok(())
}

/// Read one tunnelled packet. `Ok(None)` at a clean end of stream.
pub async fn read_tunnel_datagram<R: AsyncRead + Unpin>(
    r: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    let len = match r.read_u16().await {
        Ok(len) => len as usize,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(None);
        }
        Err(e) => {
            return Err(e);
        }
    };
    if len > MAX_TUNNEL_DATAGRAM {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "tunnelled datagram exceeds the ceiling",
        ));
    }
    let mut datagram = vec![0u8; len];
    r.read_exact(&mut datagram).await?;
    Ok(Some(datagram))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry's behaviour is testable without a QUIC connection only up to
    /// the point where one is needed; the live-connection paths (splice, evict,
    /// revoked) are proven end to end in `tests/park.rs`, which is where a real
    /// parked peer exists.
    #[test]
    fn an_empty_registry_exposes_nothing() {
        let peers = ParkedPeers::new();
        assert!(peers.is_empty());
        assert!(peers.fingerprints().is_empty());
        assert!(peers.lookup(&[0u8; 32]).is_none());
    }

    /// `Debug` must stay a count. If this ever prints a fingerprint or an
    /// address, the registry has grown a logging path for the one thing it must
    /// never log.
    #[test]
    fn debug_reveals_only_a_count() {
        let peers = ParkedPeers::new();
        assert_eq!(format!("{peers:?}"), "ParkedPeers { parked: 0 }");
    }

    /// The tunnel framing is shared wire — `pollis-core`'s device side reads and
    /// writes these exact bytes — so packet boundaries have to survive a stream
    /// that concatenates them, and an empty packet has to stay distinguishable
    /// from an end of stream.
    #[tokio::test]
    async fn tunnel_datagrams_round_trip_and_keep_their_boundaries() {
        let mut buf = Vec::new();
        write_tunnel_datagram(&mut buf, b"first").await.unwrap();
        write_tunnel_datagram(&mut buf, b"second one").await.unwrap();
        write_tunnel_datagram(&mut buf, b"").await.unwrap();

        let mut slice = &buf[..];
        assert_eq!(
            read_tunnel_datagram(&mut slice).await.unwrap().unwrap(),
            b"first"
        );
        assert_eq!(
            read_tunnel_datagram(&mut slice).await.unwrap().unwrap(),
            b"second one"
        );
        assert_eq!(
            read_tunnel_datagram(&mut slice).await.unwrap().unwrap(),
            Vec::<u8>::new()
        );
        // Clean end of stream, not an error.
        assert_eq!(read_tunnel_datagram(&mut slice).await.unwrap(), None);
    }

    /// The ceiling binds at both ends, and the reader refuses an over-long
    /// length prefix *before* allocating for it.
    #[tokio::test]
    async fn an_oversized_tunnel_datagram_is_refused_rather_than_allocated() {
        let mut buf = Vec::new();
        assert!(
            write_tunnel_datagram(&mut buf, &vec![0u8; MAX_TUNNEL_DATAGRAM + 1])
                .await
                .is_err()
        );

        let mut framed = Vec::new();
        framed.extend_from_slice(&((MAX_TUNNEL_DATAGRAM + 1) as u16).to_be_bytes());
        let mut slice = &framed[..];
        assert!(read_tunnel_datagram(&mut slice).await.is_err());
    }
}
