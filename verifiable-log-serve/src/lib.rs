//! # verifiable-log-serve
//!
//! Slice 3 of the Key Transparency work (issue #330): the **serve layer**. It
//! turns a signed monitor bundle (the output of `verifiable-log-builder`) into
//! an immutable, host-agnostic **static artifact tree** that implements the
//! log's public, unauthenticated read API — plus a tiny dev HTTP server and an
//! end-to-end "fetch over HTTP and verify" path.
//!
//! ## Why static
//!
//! Every artifact a transparency log serves is deterministic and immutable: an
//! STH for `tree_size = N` never changes; a proof for `(leaf, tree_size)` is
//! fixed forever. So the serve layer is **not** a query service over a DB — it
//! is a precomputed directory of immutable JSON files served as static assets.
//! Generate the tree, drop it on any static host (R2, Cloudflare Pages, an edge
//! CDN — none chosen here), and reads are trivially cacheable.
//!
//! ## Layers
//!
//! * [`layout`] — generate the immutable `/v1/...` directory tree from a bundle.
//! * [`server`] — a tiny dev/demo HTTP server over a generated tree (local
//!   testing only; production is "serve the directory statically"). It also
//!   exposes the dynamic `GET /verify/group/<id>` endpoint.
//! * [`remote`] — fetch the static API over HTTP and verify the whole log
//!   trusting only the public key, reusing slice 1's verifiers.
//! * [`group`] — the one shared per-group verifier the CLI and the HTTP endpoint
//!   both call, so server-side and command-line verdicts cannot diverge.
//!
//! The [`verifiable_log`] core stays dependency-pure: all HTTP lives here.

pub mod bundle;
pub mod error;
pub mod group;
pub mod layout;
pub mod remote;
pub mod server;

pub use bundle::{Bundle, Manifest, PublicKeyDoc};
pub use error::{Result, ServeError};
pub use group::{verify_group, GroupCommit, GroupReport};
pub use layout::{generate, load_bundle, API_VERSION};
pub use remote::{verify_remote, Report};
pub use server::DevServer;
