//! Networking wiring that sits between `pollis-core`'s command layer and the
//! outside world. Today this is the closed-overlay relay glue (design
//! `docs/relay-overlay-design.md` §14): starting the loopback SOCKS5 shim, the
//! shared reqwest client seam, and the SOCKS5 shim. All of it is
//! INERT until a non-off mode is applied — either `POLLIS_OVERLAY` at boot or a
//! runtime `commands::overlay::set_overlay_mode` (the Settings toggle). Off is
//! the default in both cases.
//!
//! Layout: [`directory`] verifies the signed relay directory, [`revocation`]
//! enforces the revocation list it anchors, [`path`] chooses which relays a
//! circuit is made of (guards, hop count, the first-party-exit invariant),
//! [`peer`] is this device forwarding OTHER people's traffic, and [`overlay`]
//! wires them into the running shim.

pub mod directory;
pub mod overlay;
pub mod path;
// Peer-hosted relays (#813 D1): this device forwarding OTHER people's traffic,
// off by default behind explicit consent. Inert until `set_relay_serving`
// enables it, and structurally incapable of being a circuit's exit.
pub mod peer;
pub mod revocation;

#[cfg(test)]
pub(crate) mod testing;
