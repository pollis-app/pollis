//! Where peer-hosted relays come from, and where they are allowed to sit in a
//! circuit.
//!
//! # Trust model: the signed directory, or nothing
//!
//! A client learns of candidate peer relays through **exactly one channel**: the
//! Ed25519-signed relay directory it already consumes ([`crate::net::directory`],
//! #616 §3), verified against the client-pinned `POLLIS_OVERLAY_DIRECTORY_KEY`
//! before a single byte of it is believed. Peers are an additive optional field
//! in that artifact, the same shape phase C used for the revocation anchor — no
//! version bump, no second trust root, no gossip, no DHT, no
//! "peer told me about another peer". A discovery channel that is not signed by
//! the key clients already pin would be a way to feed a target a hand-picked set
//! of relays, which is the one attack the whole directory design exists to stop.
//!
//! Practically that also means peer entries inherit phase C's revocation for
//! free: revocation selectors include `cert_sha256_b64` precisely so a single
//! peer identity can be pulled without touching the rest of the pool.
//!
//! [`PeerCatalog::from_verified_directory`] is the only constructor, and its
//! name is the contract: the caller must have verified the artifact. Nothing in
//! this module fetches anything.
//!
//! # Position policy: never terminal, never the guard
//!
//! [`peer_position_admissible`] is the single place that decides. Both
//! exclusions are load-bearing and neither is about trust in the volunteer:
//!
//! - **Terminal** is excluded because the exit hop talks to Turso/DS/R2 and must
//!   be first-party (§1.2 — no third-party liability). The peer engine also
//!   refuses destinations outright ([`super::engine`]), so this rule is the
//!   second of two independent barriers, not the only one.
//! - **Guard** (hop 1) is excluded because every hop authenticates the client's
//!   device cert and therefore learns which **account** built the circuit (Phase
//!   A's decision log). A guard additionally sees the client's IP, so a peer
//!   guard would hand a volunteer the exact account↔IP link this feature exists
//!   to keep away from everyone. A first-party guard already holds it and is
//!   accountable for it.
//!
//! What is left is the middle, which is also the position where a peer learns
//! the least: not the client's address, not the destination — only that some
//! account is building a circuit through it.

use pollis_relay::CertificateDer;

/// One peer-hosted relay a client may use as a middle hop.
#[derive(Debug, Clone)]
pub struct PeerCandidate {
    /// Stable opaque id from the directory. Not a user id and not derived from
    /// one — a relay identity must not be linkable to an account.
    pub id: String,
    /// The leaf cert the client pins for this peer. Its identity *is* this cert
    /// (§7): whatever carries the link cannot substitute itself for it, because
    /// the layer to this hop is authenticated end to end against exactly this
    /// cert.
    pub cert_der: CertificateDer<'static>,
    /// The first-party relay this peer has parked an outbound link with. There
    /// is deliberately no "dial the peer directly" alternative to represent: a
    /// peer never listens for inbound connections ([`super::engine`] binds
    /// loopback), so a directly-dialable peer is not a state this design can be
    /// in.
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
    /// The verification (signature against the pinned key, version, expiry) is
    /// [`crate::net::directory`]'s job and has already happened by the time
    /// entries reach here. There is deliberately no constructor that takes bytes
    /// or a URL: that would make it possible to build a catalog from something
    /// nobody checked.
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
    /// Returning a slice rather than a choice keeps selection (guard pinning,
    /// diversity, latency) in `net::overlay` where the rest of path selection
    /// lives; this module owns only the admissibility rule.
    pub fn candidates_for(&self, position: usize, path_len: usize) -> &[PeerCandidate] {
        if !peer_position_admissible(position, path_len) {
            return &[];
        }
        &self.candidates
    }
}

/// May a peer-hosted relay occupy `position` (0-based) of a `path_len`-hop path?
///
/// True only for a strictly interior hop, which needs `path_len >= 3`. See the
/// module docs for why the guard is excluded as well as the exit.
pub fn peer_position_admissible(position: usize, path_len: usize) -> bool {
    if path_len < 3 {
        return false;
    }
    if position >= path_len {
        return false;
    }
    // Not the guard, not the exit.
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

        // A 4-hop path has two interior positions.
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
