//! # pollis-relay
//!
//! The closed-overlay relay (design `docs/relay-overlay-design.md` §14).
//!
//! The primitive is a generic `CONNECT(host, port)` stream; the allowlist is relay-side config, not protocol.
//!
//! Relays forward opaque bytes inside the client's own TLS; auth is the offline device-cert chain (§11.1).

pub mod anchor;
pub mod backoff;
pub mod circuit;
pub mod client;
pub mod config;
pub mod health;
pub mod http;
pub mod nexthop;
pub mod onion;
pub mod park;
pub mod policy;
pub mod proto;
pub mod ratelimit;
pub mod revocation_sync;
pub mod server;
pub mod shim;
pub mod stream;
pub mod tls;

pub use circuit::{Circuit, CircuitFactory, Hop, SingleHopFactory, StaticPathFactory, MAX_HOPS};
pub use client::{ClientIdentity, RelayClient, RelayLink};
pub use anchor::{AccountAnchor, AccountKeyLeaf, AnchorError, AnchorPolicy, AnchorVerifier};
pub use config::{RateLimitFileConfig, RelayFileConfig};
pub use backoff::{jittered, jittered_down};
pub use http::{http_client, http_client_builder};
pub use policy::{
    verify_revocations, Admission, FinalAction, OverlayMode, PlannedRoute, RelayIdentity,
    RevocationError, RevocationList, RevocationStore, RoutingPolicy, REVOCATION_TYPE,
    REVOCATION_VERSION,
};
pub use nexthop::{
    is_public_dial_address, verify_next_hops, DialVerdict, DirectoryError, NextHopDirectory,
    NextHops, DIRECTORY_TYPE, DIRECTORY_VERSION,
};
pub use park::{ParkedPeers, PeerFingerprint};
pub use proto::{ClientFrame, DeviceCertMaterial, Park, RejectReason, VerifiedClient};
// Re-exported so consumers (pollis-core's `net::overlay`) can name the pinned
// relay leaf type without taking a direct `rustls`/`rustls-pki-types` dependency.
pub use rustls::pki_types::CertificateDer;
pub use ratelimit::{ConnectionLimiter, IpKey, RateLimitConfig, RateLimiter};
pub use server::{
    Allowlist, HostPattern, PeerObserver, PortPattern, RelayConfig, RelayServer, RelayStats,
    DEFAULT_ALLOWED_PORT,
};
pub use shim::{OverlayHandle, OverlayShim};
pub use stream::{BoxedStream, RelayStream};
