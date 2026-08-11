//! [`PeerLink`] — the datagram pipe a peer-hosted relay is reached through.
//!
//! A link carries **QUIC datagrams verbatim** between this device's loopback
//! relay engine and whatever is on the other end (in v1: a first-party relay the
//! device dialled outbound — see the module docs for why the direction is
//! peer→server and why that removes the NAT problem rather than solving it).
//!
//! Datagram semantics, deliberately:
//!
//! - **Unreliable.** A full channel drops the datagram instead of blocking. QUIC
//!   already retransmits what matters, and back-pressuring a shared pipe on one
//!   slow reader would stall every circuit on it (head-of-line blocking is
//!   exactly what a datagram tunnel exists to avoid).
//! - **Unordered.** Nothing here reorders or reassembles; QUIC's packet numbers
//!   do that end to end.
//! - **Opaque.** The bytes are a QUIC packet whose TLS terminates at this
//!   device's relay identity or beyond it. Nothing on this path can read them,
//!   and the pinned leaf cert is verified end to end through the pipe, so
//!   whatever carries the link cannot substitute itself for the relay either.

use tokio::sync::mpsc;

/// How many datagrams may queue in each direction before the pipe starts
/// dropping. ~1.5 MB at a 1200-byte MTU: enough to ride out a scheduling hiccup,
/// small enough that a wedged reader cannot balloon memory.
pub const LINK_QUEUE_DEPTH: usize = 1024;

/// One end of a bidirectional datagram pipe.
pub struct PeerLink {
    /// Datagrams to hand to the other end.
    outbound: mpsc::Sender<Vec<u8>>,
    /// Datagrams arriving from the other end.
    inbound: mpsc::Receiver<Vec<u8>>,
}

impl PeerLink {
    /// Build a link from an already-connected transport's channel pair.
    pub fn new(outbound: mpsc::Sender<Vec<u8>>, inbound: mpsc::Receiver<Vec<u8>>) -> PeerLink {
        PeerLink { outbound, inbound }
    }

    /// Queue one datagram. Returns `false` when it was dropped — either the pipe
    /// is full (see the module docs) or the far end is gone.
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
/// This is the test double for a real transport, and it is also the shape any
/// real transport has to present, which keeps [`super::engine::serve_link`]
/// honest: the engine is written against the pipe, never against a transport.
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

    /// A full pipe drops rather than blocking — the property that keeps one
    /// stalled circuit from wedging every other circuit sharing the link.
    #[tokio::test]
    async fn a_full_pipe_drops_instead_of_blocking() {
        let (a, _b) = loopback_pair();
        for _ in 0..LINK_QUEUE_DEPTH {
            assert!(a.try_send(vec![0u8; 8]));
        }
        assert!(!a.try_send(vec![0u8; 8]));
    }
}
