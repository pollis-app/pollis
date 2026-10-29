//! Where peer-hosted relays come from, and where they are allowed to sit in a
//! circuit.
//!
//! Candidates come only from the Ed25519-signed relay directory
//! ([`crate::net::directory`]), verified against the pinned key: an unsigned
//! channel could feed a target hand-picked relays. That also gives peers the
//! directory's revocation (by `cert_sha256_b64`) for free.
//!
//! [`peer_position_admissible`] decides where a peer may sit:
//!
//! - never terminal: the exit talks to Turso/DS/R2 and must be first-party (the
//!   peer engine also refuses all destinations, a second barrier);
//! - never the guard: every hop learns the account from the device cert, and the
//!   guard also sees the client's IP, so a peer guard would hand a volunteer the
//!   account↔IP link.
//!
//! The middle is also where a peer learns the least.

use pollis_relay::CertificateDer;

/// One peer-hosted relay a client may use as a middle hop.
#[derive(Debug, Clone)]
pub struct PeerCandidate {
    /// Stable opaque id from the directory, never linkable to an account.
    pub id: String,
    /// The leaf cert the client pins for this peer; the layer to this hop is
    /// authenticated against exactly this cert.
    pub cert_der: CertificateDer<'static>,
    /// The first-party relay this peer has parked an outbound link with (a peer
    /// never listens for inbound connections).
    pub via_relay: String,
}

/// Peer candidates from one verified directory fetch.
#[derive(Debug, Clone, Default)]
pub struct PeerCatalog {
    candidates: Vec<PeerCandidate>,
}

impl PeerCatalog {
    /// Build from entries of an already-verified signed directory. There is
    /// deliberately no constructor taking bytes or a URL.
    pub fn from_verified_directory<I>(entries: I) -> PeerCatalog
    where
        I: IntoIterator<Item = PeerCandidate>,
    {
        PeerCatalog {
            candidates: entries.into_iter().collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// The candidates usable at `position` of a `path_len`-hop path, or empty
    /// where a peer may not sit. Selection itself stays in `net::path`.
    pub fn candidates_for(&self, position: usize, path_len: usize) -> &[PeerCandidate] {
        if !peer_position_admissible(position, path_len) {
            return &[];
        }
        &self.candidates
    }
}

/// May a peer-hosted relay occupy `position` (0-based) of a `path_len`-hop path?
///
/// True only for a strictly interior hop (`path_len >= 3`); see the module docs.
pub fn peer_position_admissible(position: usize, path_len: usize) -> bool {
    if path_len < 3 {
        return false;
    }
    if position >= path_len {
        return false;
    }
    position > 0 && position + 1 < path_len
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str) -> PeerCandidate {
        PeerCandidate {
            id: id.to_string(),
            cert_der: CertificateDer::from(vec![0x30, 0x82, 0x01, 0x02]),
            via_relay: "relay-a".to_string(),
        }
    }

    fn catalog() -> PeerCatalog {
        PeerCatalog::from_verified_directory([candidate("p1"), candidate("p2")])
    }

    #[test]
    fn a_peer_is_never_admissible_as_the_exit() {
        for path_len in 1..=4 {
            let terminal = path_len - 1;
            assert!(
                !peer_position_admissible(terminal, path_len),
                "terminal hop of a {path_len}-hop path"
            );
            assert!(
                catalog().candidates_for(terminal, path_len).is_empty(),
                "catalog offered a peer for the exit of a {path_len}-hop path"
            );
        }
    }

    #[test]
    fn a_peer_is_never_admissible_as_the_guard() {
        for path_len in 1..=4 {
            assert!(!peer_position_admissible(0, path_len));
            assert!(catalog().candidates_for(0, path_len).is_empty());
        }
    }

    #[test]
    fn a_peer_is_admissible_in_the_middle() {
        assert!(peer_position_admissible(1, 3));
        assert_eq!(catalog().candidates_for(1, 3).len(), 2);

        assert!(peer_position_admissible(1, 4));
        assert!(peer_position_admissible(2, 4));
    }

    #[test]
    fn short_paths_have_no_room_for_a_peer() {
        assert!(!peer_position_admissible(0, 2));
        assert!(!peer_position_admissible(1, 2));
        assert!(!peer_position_admissible(0, 1));
    }

    #[test]
    fn out_of_range_positions_are_refused() {
        assert!(!peer_position_admissible(3, 3));
        assert!(!peer_position_admissible(9, 3));
    }

    #[test]
    fn an_empty_catalog_offers_nothing_anywhere() {
        let empty = PeerCatalog::default();
        assert!(empty.is_empty());
        assert!(empty.candidates_for(1, 3).is_empty());
    }
}
