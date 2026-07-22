//! # pollis-relay
//!
//! Slice 1a of the closed-overlay relay (design `docs/relay-overlay-design.md`
//! §14). A self-contained transport core + minimal relay server.
//!
//! The overlay's transport primitive is a **generic anonymized stream**:
//! `CONNECT(host, port)` inside a QUIC circuit. The first-party destination
//! allowlist is enforced as **relay-side policy** (config data), never baked
//! into the protocol — so a future in-app browser / VPN can reuse the same shim
//! (§14.0).
//!
//! Layout:
//! - [`proto`] — the wire protocol + the Ed25519 device-signature handshake.
//! - [`server`] — the QUIC relay: handshake → allowlist → dial → pipe.
//! - [`client`] — the QUIC relay client.
//! - [`circuit`] — an n-hop `Circuit` (v0: n = 1) + a [`circuit::CircuitFactory`].
//! - [`shim`] — the local SOCKS5 CONNECT server on loopback.
//! - [`policy`] — pure `off | prefer | strict` routing + the plane split (§6.4).
//! - [`http`] — the shared reqwest client helper.
//! - [`tls`] — cert generation + the pinned-cert QUIC verifier.
//! - [`stream`] — the byte-pipe stream types.
//!
//! The relay only ever forwards opaque bytes inside the client's own TLS to a
//! first-party host; it never terminates that TLS and never sees plaintext or
//! keys (§8).

pub mod circuit;
pub mod client;
pub mod http;
pub mod policy;
pub mod proto;
pub mod server;
pub mod shim;
pub mod stream;
pub mod tls;

// Re-export the load-bearing types at the crate root for ergonomic consumers.
pub use circuit::{Circuit, CircuitFactory, Hop, SingleHopFactory};
pub use client::{ClientIdentity, RelayClient};
pub use http::{http_client, http_client_builder};
pub use policy::{FinalAction, OverlayMode, PlannedRoute, RoutingPolicy};
pub use proto::{InMemoryKeyResolver, KeyResolver, RejectReason};
pub use server::{Allowlist, HostPattern, RelayConfig, RelayServer, RelayStats};
pub use shim::{OverlayHandle, OverlayShim};
pub use stream::{BoxedStream, RelayStream};
