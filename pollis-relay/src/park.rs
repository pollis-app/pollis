//! Parked peer relays: the reverse hop (#813 Phase D1/P1, design §7.1).
//!
//! A NATed peer dials each first-party relay, sends a [`crate::proto::Park`]
//! naming the leaf cert it serves under, and leaves the connection open; a
//! later `Extend` naming that leaf is spliced into it instead of dialled.
//!
//! The map holds `sha256(leaf) -> { connection, leaf DER }` and nothing else:
//! §5.2/§11.7 forbid recording who a relay's users are. The DER is kept because
//! revocation's `cert_sha256_b64` selector needs the preimage.
//!
//! [`ParkedPeer::splice`] opens one bi-stream per circuit on the parked
//! connection and bridges it to a loopback UDP socket, which the normal
//! `Extend` path dials with the same fingerprint pin. So a device parked under
//! a leaf whose key it lacks fails the handshake: `Park` needs no new
//! credential. Framing is [`write_tunnel_datagram`], shared with `pollis-core`.
//!
//! The two directions run as separate tasks, never one `select!`: a framed
//! read is not cancel-safe.
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use crate::proto::cert_fingerprint;

/// SHA-256 of a parked peer's DER leaf, as carried in `Extend::next_cert_sha256`.
pub type PeerFingerprint = [u8; 32];

/// Bounds a peer-forced allocation (quinn packets stay under 1452). Must match
/// `pollis-core`'s `net::peer::park::MAX_TUNNEL_DATAGRAM`.
pub const MAX_TUNNEL_DATAGRAM: usize = 4096;

/// Every peer currently parked at this node; shared with the health endpoint.
#[derive(Default)]
pub struct ParkedPeers {
    map: Mutex<HashMap<PeerFingerprint, ParkedEntry>>,
}

struct ParkedEntry {
    connection: quinn::Connection,
    leaf_der: Arc<Vec<u8>>,
}

impl ParkedPeers {
    pub fn new() -> Arc<ParkedPeers> {
        Arc::new(ParkedPeers::default())
    }

    /// Register `connection` for `leaf_der`; dropping the registration unparks.
    ///
    /// `None` if a live connection already holds the fingerprint. First wins:
    /// last-wins would let any device evict a genuine peer. Neither can
    /// intercept traffic, since a squatter can't complete the pinned handshake.
    pub(crate) fn park(
        self: &Arc<Self>,
        leaf_der: Vec<u8>,
        connection: quinn::Connection,
    ) -> Option<ParkRegistration> {
        let fingerprint = cert_fingerprint(&leaf_der);
        let mut map = self.lock();
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

    pub(crate) fn lookup(&self, fingerprint: &PeerFingerprint) -> Option<ParkedPeer> {
        let map = self.lock();
        let entry = map.get(fingerprint)?;
        // Closed but not yet reaped: not a next hop.
        if entry.connection.close_reason().is_some() {
            return None;
        }
        Some(ParkedPeer {
            connection: entry.connection.clone(),
            leaf_der: entry.leaf_der.clone(),
        })
    }

    pub fn fingerprints(&self) -> Vec<PeerFingerprint> {
        let mut out: Vec<PeerFingerprint> = self.lock().keys().copied().collect();
        out.sort_unstable();
        out
    }

    /// The parked DER leaves, ordered by fingerprint: the entire external view
    /// of the registry (already public via the signed directory). Nothing else
    /// may leave it.
    pub fn peer_leaves(&self) -> Vec<Vec<u8>> {
        let map = self.lock();
        let mut entries: Vec<(&PeerFingerprint, &ParkedEntry)> = map.iter().collect();
        entries.sort_unstable_by_key(|(fingerprint, _)| *fingerprint);
        entries
            .into_iter()
            .map(|(_, entry)| entry.leaf_der.as_ref().clone())
            .collect()
    }

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
    /// Only a count, so `Debug` is never a logging path for the registry.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParkedPeers")
            .field("parked", &self.len())
            .finish()
    }
}

/// Held for the parking stream's life; dropping it unparks on every exit path.
pub(crate) struct ParkRegistration {
    peers: Arc<ParkedPeers>,
    fingerprint: PeerFingerprint,
    connection_id: usize,
}

impl Drop for ParkRegistration {
    fn drop(&mut self) {
        let mut map = self.peers.lock();
        // Only reap our own registration, not a later re-park.
        if let Some(entry) = map.get(&self.fingerprint) {
            if entry.connection.stable_id() == self.connection_id {
                map.remove(&self.fingerprint);
            }
        }
    }
}

pub(crate) struct ParkedPeer {
    connection: quinn::Connection,
    leaf_der: Arc<Vec<u8>>,
}

impl ParkedPeer {
    /// The identity revocation is evaluated against before splicing.
    pub(crate) fn leaf_der(&self) -> &[u8] {
        &self.leaf_der
    }

    /// Splice one circuit: a stream on the parked connection bridged to a
    /// loopback UDP address the `Extend` path dials. QUIC opens the stream
    /// lazily, on the tunnelled connection's first packet.
    pub(crate) async fn splice(&self) -> anyhow::Result<Tunnel> {
        let (send, recv) = self.connection.open_bi().await?;
        let socket = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?);
        let addr = socket.local_addr()?;

        // The return path: the relay always speaks first, from one source port.
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

pub(crate) struct Tunnel {
    addr: SocketAddr,
    pumps: [JoinHandle<()>; 2],
}

impl Tunnel {
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

/// Also learns the return path.
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
            // EOF or an untrustworthy frame: the circuit fails and the client rebuilds.
            Ok(None) | Err(_) => {
                return;
            }
        }
    }
}

/// Write one tunnelled packet: `u16` big-endian length, then the bytes. Shared
/// wire with `pollis-core`. `len == 0` is an empty packet, not end of stream.
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

    /// Live-connection paths are covered in `tests/park.rs`.
    #[test]
    fn an_empty_registry_exposes_nothing() {
        let peers = ParkedPeers::new();
        assert!(peers.is_empty());
        assert!(peers.fingerprints().is_empty());
        assert!(peers.lookup(&[0u8; 32]).is_none());
    }

    #[test]
    fn debug_reveals_only_a_count() {
        let peers = ParkedPeers::new();
        assert_eq!(format!("{peers:?}"), "ParkedPeers { parked: 0 }");
    }

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
        assert_eq!(read_tunnel_datagram(&mut slice).await.unwrap(), None);
    }

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
