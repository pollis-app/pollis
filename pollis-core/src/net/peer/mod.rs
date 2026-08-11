//! Peer-hosted relays — this device forwarding *other people's* encrypted
//! traffic (design `docs/relay-overlay-design.md` §7, §8, §10.2; #813 phase D1).
//!
//! This is the opposite direction from [`crate::net::overlay`]: that module
//! decides whether *your* traffic goes through the overlay, this one decides
//! whether *other people's* traffic goes through *your* device. Neither implies
//! the other, and this one is **off by default** behind explicit consent
//! ([`RelayServingConfig`], Preferences → "Run a relay for others").
//!
//! # What a relaying device does, and what it can never do
//!
//! It forwards **opaque bytes** and nothing else (§5.2, §8, §11.7). Concretely,
//! and structurally rather than by good behaviour:
//!
//! - It runs [`pollis_relay::RelayServer`] with an **empty destination
//!   allowlist**. An empty allowlist permits nothing, so a `Connect` frame — the
//!   only frame that ever names a destination — is refused with `NotAllowed`
//!   before any socket is opened. A peer therefore **cannot be a circuit's exit**
//!   even if a buggy or malicious path selector puts it last: the hop that talks
//!   to Turso/DS/R2 is always first-party (§1.2, no third-party liability).
//! - It never stores who connected to it (`peer_observer` stays `None`), never
//!   holds commit-log state, never mints tokens, and holds no key to anything it
//!   carries — the traffic is inside the client's own TLS to a first-party host,
//!   under an onion layer addressed to a hop further along.
//! - It listens on **loopback only**. Nothing on the network can reach the
//!   relay engine directly; every byte arrives over a [`PeerLink`] the device
//!   itself established outbound. No new externally-reachable port is opened on
//!   a volunteer's machine, and no firewall exception is needed.
//!
//! # Position in the circuit: middle only, and why that decides the transport
//!
//! A peer may only ever be a **non-terminal, non-guard** hop
//! ([`discovery::peer_position_admissible`]). Terminal is excluded because of
//! the exit rule above. The **guard** position is excluded for a privacy reason
//! that is easy to miss: every hop verifies the client's device cert, so every
//! hop learns *which account* built the circuit (Phase A's decision log — the
//! property multi-hop delivers is network-address unlinkability, not anonymity).
//! A guard also sees the client's IP. A peer guard would therefore hand a
//! volunteer the account↔IP link that the whole feature exists to keep away from
//! anyone — strictly worse than a first-party guard. A peer **middle** hop learns
//! neither the client's address nor the destination, which is the position where
//! a volunteer adds anonymity-set value while learning the least.
//!
//! That constraint settles the NAT-traversal question §7.1 left open. §7.1
//! assumed a peer must accept **inbound** connections and recommended reusing
//! the shipped WebRTC/ICE stack (libwebrtc via the `livekit` crate) for
//! hole-punching. But a middle hop is reached by the *previous relay*, which is
//! first-party — and a first-party relay is not an ICE agent, so hole-punching
//! between them was never on the table. The connection that can always be made
//! is the one the peer opens itself: **peer → first-party relay, outbound**,
//! which traverses NAT/CGNAT by construction. Reversing the direction removes
//! the NAT problem instead of solving it, and costs no second networking stack.
//! So D1 does **not** take a libwebrtc dependency for relaying; the transport is
//! a [`PeerLink`], a plain bidirectional datagram pipe, and the QUIC connection
//! between the first-party relay and this device's loopback engine rides inside
//! it ([`engine::serve_link`]). QUIC's own loss recovery makes an unreliable
//! datagram pipe the right shape, and the peer's leaf cert is pinned end to end
//! through it, so whatever carries the pipe cannot read or substitute anything.
//!
//! **What is not built here (and is honestly reported, not faked):** the
//! first-party side of that reverse hop — a relay accepting "I am available as a
//! middle hop" and splicing an `Extend` into an existing peer connection instead
//! of dialing a `SocketAddr` — lives in `pollis-relay/src/{server,proto}.rs`,
//! which phase D1 does not own. Until it lands, a consenting device has no
//! [`PeerLink`] producer, reports [`RelayServingHold::NoInboundPath`], and
//! carries nothing. The UI already renders exactly that ("other devices can't
//! reach this one through your network"). Everything on this side of the seam —
//! the engine, the datagram bridge, the consent/condition state machine, the
//! counters, and the position policy — is complete and tested against real
//! multi-hop circuits.
//!
//! # Layout
//!
//! - [`conditions`] — the pure consent + platform-condition state machine. Every
//!   rule is fail-closed and unit-tested; an unknown platform signal never
//!   satisfies a condition the user asked for.
//! - [`platform`] — the OS probes behind those signals (`None` = can't tell).
//! - [`engine`] — the loopback relay node + the [`PeerLink`] ↔ QUIC bridge.
//! - [`link`] — the datagram pipe itself.
//! - [`discovery`] — where peer candidates come from (the signed directory,
//!   never an unauthenticated channel) and where they may sit in a path.
//! - [`serving`] — the live manager behind `get_relay_serving_status` /
//!   `set_relay_serving` and the `relay-serving-status` event.

pub mod conditions;
pub mod discovery;
pub mod engine;
pub mod link;
pub mod platform;
pub mod serving;

pub use conditions::{
    LinkSignals, RelayServingConfig, RelayServingHold, RelayServingState, RelayServingStatus,
    ServingSignals,
};
pub use discovery::{peer_position_admissible, PeerCandidate, PeerCatalog};
pub use engine::{PeerCounters, PeerEngine};
pub use link::{loopback_pair, PeerLink};
pub use serving::{manager, set_event_sink, RelayServingManager, RELAY_SERVING_EVENT};
