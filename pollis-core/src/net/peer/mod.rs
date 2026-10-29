//! Peer-hosted relays — this device forwarding *other people's* encrypted
//! traffic (design `docs/relay-overlay-design.md` §7, §8, §10.2; #813 phase D1).
//!
//! The opposite of [`crate::net::overlay`]: other people's traffic through *your* device. **Off by default** ([`RelayServingConfig`]).
//!
//! # What a relaying device does, and what it can never do
//!
//! It forwards **opaque bytes** only (§5.2, §8, §11.7), structurally:
//!
//! - An **empty destination allowlist** refuses every `Connect`, so a peer can never be a circuit's exit.
//! - It records no peers, holds no commit-log state or keys, and sees only onion-wrapped TLS.
//! - It listens on **loopback only**; every byte arrives over a [`PeerLink`] it opened outbound.
//!
//! # Position in the circuit: middle only, and why that decides the transport
//!
//! **Non-terminal, non-guard** only ([`discovery::peer_position_admissible`]): a guard sees the IP and the account, a middle learns neither.
//!
//! Middles are reached by a first-party relay, so the peer dials out (**peer → relay**), avoiding NAT entirely; QUIC rides a [`PeerLink`] ([`engine::serve_link`]).
//!
//! Wave 3 builds that reverse hop in [`park`]; [`RelayServingHold::NoInboundPath`] now means "no parked connection is live".
//!
//! # Layout
//!
//! - [`conditions`] — the fail-closed consent + platform-condition state machine.
//! - [`platform`] — the OS probes behind those signals (`None` = can't tell).
//! - [`engine`] — the loopback relay node + the [`PeerLink`] ↔ QUIC bridge.
//! - [`link`] — the datagram pipe itself.
//! - [`park`] — the outbound parked connections, the `Park` frame and tunnel framing.
//! - [`reachability`] — the directory, revocation evidence and parked set.
//! - [`context`] — how the app hands over a directory and identity.
//! - [`discovery`] — where peer candidates come from and where they may sit.
//! - [`serving`] — the manager behind `get/set_relay_serving` and its event.

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
