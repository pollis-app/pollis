//! [`PeerLink`] — the datagram pipe a peer-hosted relay is reached through.
//!
//! Carries **QUIC datagrams verbatim** between the loopback engine and the far end.
//!
//! Datagram semantics, deliberately:
//!
//! - **Unreliable.** A full channel drops instead of blocking; QUIC retransmits, and blocking would stall every circuit.
//! - **Unordered.** QUIC handles ordering end to end.
//! - **Opaque.** TLS terminates at this relay or beyond, with the leaf pinned through the pipe.

use tokio::sync::mpsc;

/// Per-direction queue depth before dropping (~1.5 MB at 1200-byte MTU).
pub const LINK_QUEUE_DEPTH: usize = 1024;

/// One end of a bidirectional datagram pipe.
pub struct PeerLink {
    outbound: mpsc::Sender<Vec<u8>>,
    inbound: mpsc::Receiver<Vec<u8>>,
}

impl PeerLink {
    /// Build a link from an already-connected transport's channel pair.
    pub fn new(outbound: mpsc::Sender<Vec<u8>>, inbound: mpsc::Receiver<Vec<u8>>) -> PeerLink {
        PeerLink { outbound, inbound }
    }

    /// Queue one datagram; `false` when dropped (full or closed).
    pub fn try_send(&self, datagram: Vec<u8>) -> bool {
        self.outbound.try_send(datagram).is_ok()
    }

    /// Next datagram from the far end; `None` once the link is closed.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.inbound.recv().await
    }

    /// True once the far end has dropped its sender.
    pub fn is_closed(&self) -> bool {
        self.outbound.is_closed()
    }
}

/// Two ends of an in-process pipe, wired to each other.
///
/// The test double, and the shape every real transport must present.
pub fn loopback_pair() -> (PeerLink, PeerLink) {
    let (a_tx, a_rx) = mpsc::channel(LINK_QUEUE_DEPTH);
    let (b_tx, b_rx) = mpsc::channel(LINK_QUEUE_DEPTH);
    (PeerLink::new(a_tx, b_rx), PeerLink::new(b_tx, a_rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_loopback_pair_carries_datagrams_both_ways() {
        let (a, mut b) = loopback_pair();
        assert!(a.try_send(b"hello".to_vec()));
        assert_eq!(b.recv().await.unwrap(), b"hello");

        assert!(b.try_send(b"back".to_vec()));
        let mut a = a;
        assert_eq!(a.recv().await.unwrap(), b"back");
    }

    #[tokio::test]
    async fn dropping_one_end_closes_the_other() {
        let (a, b) = loopback_pair();
        drop(b);
        assert!(a.is_closed());
        assert!(!a.try_send(b"x".to_vec()));
    }

    /// A full pipe drops rather than blocking.
    #[tokio::test]
    async fn a_full_pipe_drops_instead_of_blocking() {
        let (a, _b) = loopback_pair();
        for _ in 0..LINK_QUEUE_DEPTH {
            assert!(a.try_send(vec![0u8; 8]));
        }
        assert!(!a.try_send(vec![0u8; 8]));
    }
}
