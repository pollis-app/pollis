//! Networking glue between the command layer and the outside world; inert until a non-off overlay mode is applied.
//!
//! Layout: [`directory`] verifies the signed relay directory, [`revocation`]
//! enforces the revocation list it anchors, [`path`] chooses which relays a
//! circuit is made of (guards, hop count, the first-party-exit invariant),
//! [`peer`] is this device forwarding OTHER people's traffic, and [`overlay`]
//! wires them into the running shim.

pub mod directory;
pub mod overlay;
pub mod path;
// Peer-hosted relays (#813 D1): off by default, never a circuit's exit.
pub mod peer;
pub mod revocation;

#[cfg(test)]
pub(crate) mod testing;
