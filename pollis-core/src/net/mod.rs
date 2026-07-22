//! Networking wiring that sits between `pollis-core`'s command layer and the
//! outside world. Today this is the closed-overlay relay glue (design
//! `docs/relay-overlay-design.md` §14): starting the loopback SOCKS5 shim, the
//! shared reqwest client seam, and the libsql SOCKS connector. All of it is
//! INERT unless `POLLIS_OVERLAY` selects a non-off mode at runtime.

pub mod overlay;
