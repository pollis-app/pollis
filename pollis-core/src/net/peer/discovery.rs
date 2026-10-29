//! Where peer-hosted relays come from, and where they are allowed to sit in a
//! circuit.
//!
//! # Trust model: the signed directory, or nothing
//!
//! Peers arrive through **exactly one channel**: the signed directory ([`crate::net::directory`]); an unsigned channel would let an attacker hand-pick relays.
//!
//! Peer entries inherit revocation via the `cert_sha256_b64` selector.
//!
//! [`PeerCatalog::from_verified_directory`] is the only constructor; nothing here fetches.
//!
//! # Position policy: never terminal, never the guard
//!
//! [`peer_position_admissible`] decides:
//!
//! - **Terminal**: the exit must be first-party (§1.2); the engine also refuses destinations.
//! - **Guard**: it sees the client's IP and, via the device cert, the account.

use pollis_relay::CertificateDer;

/// One peer-hosted relay a client may use as a middle hop.
#[derive(Debug, Clone)]
pub struct PeerCandidate {
    /// Opaque directory id, never linkable to an account.
    pub id: String,
    /// The pinned leaf; this peer's identity (§7).
    pub cert_der: CertificateDer<'static>,
    /// The first-party relay this peer parked at; peers are never directly dialable.
    pub via_relay: String,
}

/// Peer candidates from one verified directory fetch.
#[derive(Debug, Clone, Default)]
pub struct PeerCatalog {
    candidates: Vec<PeerCandidate>,
}

impl PeerCatalog {
    /// Build from entries taken out of an **already-verified** signed directory.
    ///
    /// Verification is [`crate::net::directory`]'s job; no constructor takes unchecked bytes.
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

    /// The candidates usable at `position` of a `path_len`-hop path, or an empty
    /// slice when a peer may not sit there at all.
    ///
    /// Selection stays in `net::overlay`; this owns only admissibility.
    pub fn candidates_for(&self, position: usize, path_len: usize) -> &[PeerCandidate] {
        if !peer_position_admissible(position, path_len) {
            return &[];
        }
        &self.candidates
    }
}

/// May a peer-hosted relay occupy `position` (0-based) of a `path_len`-hop path?
///
/// Only a strictly interior hop (`path_len >= 3`).
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

    /// The rule the exit constraint rests on, from the policy side.
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

    /// A two-hop path has no interior position, so it can never carry a peer.
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
