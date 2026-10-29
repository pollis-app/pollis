//! Parked peer relays: the reverse hop (#813 Phase D1/P1, design §7.1).
//!
//! # Why a peer connection has to be parked
//!
//! A NAT'd peer cannot be dialled, so it parks an outbound connection; an `Extend` for its leaf fingerprint is spliced in.
//!
//! # What is in the map, and what is deliberately not
//!
//! `sha256(relay_leaf_der) -> { the live connection, that DER }`.
//! Nothing else: no client identity, addresses, counts or persistence (§5.2/§11.7).
//!
//! - the **connection**, because that IS the parked socket;
//! - the **DER leaf**, because revocation's `cert_sha256_b64` selector needs the preimage.
//!
//! Entries are dropped the moment the parking stream ends — which a dead
//! connection also ends — so nothing accumulates.
//!
//! # Splicing: a QUIC connection inside a QUIC connection
//!
//! [`ParkedPeer::splice`] opens one bi-stream per circuit, bridged to a loopback UDP socket `Extend` dials.
//!
//! Each splice stream carries length-delimited QUIC packets ([`write_tunnel_datagram`]), as `pollis-core` does.
//!
//! Three things fall out of dialling a loopback socket, all load-bearing:
//!
//! 1. **The fingerprint pin still binds:** a device without the leaf's key fails the handshake.
//! 2. **Revocation still binds** — checked before the splice on the full
//!    identity, and again on the leaf actually presented.
//! 3. **The onion wire is unchanged.** The peer sees a `Layer` frame arriving on
//!    a QUIC connection, exactly as a dialled relay does.
//!
//! One stream per circuit, so head-of-line blocking stays within a circuit.
//!
//! The two directions run as **separate tasks, never one `select!`**: framed reads are not cancel-safe.
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

/// Ceiling on one tunnelled packet (quinn stays under 1452); must match `pollis-core`'s `MAX_TUNNEL_DATAGRAM`.
pub const MAX_TUNNEL_DATAGRAM: usize = 4096;

/// Every peer currently parked at this node.
#[derive(Default)]
pub struct ParkedPeers {
    map: Mutex<HashMap<PeerFingerprint, ParkedEntry>>,
}

/// One parked peer (see the module docs).
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
    /// `None` if a live connection already holds this fingerprint: first wins, so a squatter cannot evict a serving peer.
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
        // A closed but unreaped connection is not a next hop.
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
    /// The registry's entire external view; leaves are public directory data.
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

/// Holds a registration while its parking stream lives; dropping it unparks on every path.
pub(crate) struct ParkRegistration {
    peers: Arc<ParkedPeers>,
    fingerprint: PeerFingerprint,
    connection_id: usize,
}

impl Drop for ParkRegistration {
    fn drop(&mut self) {
        let mut map = self.peers.lock();
        // Only reap our own registration, not a newer re-park.
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
    pub(crate) async fn splice(&self) -> anyhow::Result<Tunnel> {
        let (send, recv) = self.connection.open_bi().await?;
        let socket = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?);
        let addr = socket.local_addr()?;

        // The relay always speaks first from one port, so the return path is known before any reply.
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
            // Clean end or an untrustworthy frame: this splice is over.
            Ok(None) | Err(_) => {
                return;
            }
        }
    }
}

/// Write one tunnelled packet: `u16` big-endian length, then that many bytes.
///
/// Shared wire with `pollis-core`; `len == 0` is an empty packet, not EOF.
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

    /// Live-connection paths are proven end to end in `tests/park.rs`.
    #[test]
    fn an_empty_registry_exposes_nothing() {
        let peers = ParkedPeers::new();
        assert!(peers.is_empty());
        assert!(peers.fingerprints().is_empty());
        assert!(peers.lookup(&[0u8; 32]).is_none());
    }

    /// `Debug` must stay a count.
    #[test]
    fn debug_reveals_only_a_count() {
        let peers = ParkedPeers::new();
        assert_eq!(format!("{peers:?}"), "ParkedPeers { parked: 0 }");
    }

    /// Packet boundaries survive concatenation, and an empty packet is not EOF.
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
