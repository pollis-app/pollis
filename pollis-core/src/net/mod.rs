//! Closed-overlay relay glue (`docs/relay-overlay-design.md` §14). Inert until
//! a non-off mode is applied, via `POLLIS_OVERLAY` at boot or
//! `commands::overlay::set_overlay_mode`.
//!
//! [`directory`] verifies the signed relay directory, [`revocation`] enforces
//! its revocation list, [`path`] chooses a circuit's relays, [`peer`] forwards
//! other people's traffic, and [`overlay`] wires them into the running shim.

pub mod directory;
pub mod overlay;
pub mod path;
// Peer-hosted relays (#813), off by default and never a circuit's exit.
pub mod peer;
pub mod revocation;

#[cfg(test)]
pub(crate) mod testing;
