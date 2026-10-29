//! Peer-hosted relays: this device forwarding other people's encrypted traffic
//! (#813; `docs/relay-overlay-design.md` §7, §8, §10.2). Off by default behind
//! explicit consent ([`RelayServingConfig`]); independent of whether your own
//! traffic uses the overlay ([`crate::net::overlay`]).
//!
//! What a relaying device can never do, structurally:
//!
//! - be an exit: it runs [`pollis_relay::RelayServer`] with an empty
//!   destination allowlist, so every `Connect` is refused before a socket opens;
//! - record who connected, hold commit-log state, mint tokens, or hold a key to
//!   anything it carries;
//! - accept inbound connections: it listens on loopback only, and every byte
//!   arrives over a [`PeerLink`] the device opened outbound ([`park`]).
//!
//! A peer is a middle hop only ([`discovery`] says why), reached by a
//! first-party relay over a link the peer dialled out, which removes the NAT
//! problem instead of needing ICE (§7.1).

pub mod conditions;
pub mod context;
pub mod discovery;
pub mod engine;
pub mod link;
pub mod park;
pub mod platform;
pub mod reachability;
pub mod serving;

pub use conditions::{
    LinkSignals, RelayServingConfig, RelayServingHold, RelayServingState, RelayServingStatus,
    ServingSignals,
};
pub use context::{AppStateContext, ServingContext};
pub use discovery::{peer_position_admissible, PeerCandidate, PeerCatalog};
pub use engine::{PeerCounters, PeerEngine};
pub use link::{loopback_pair, PeerLink};
pub use park::{LinkAcceptor, ParkedRelay, ParkingSupervisor};
pub use serving::{
    manager, set_event_sink, set_serving_context, RelayServingManager, RELAY_SERVING_EVENT,
};
