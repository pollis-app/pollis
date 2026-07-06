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
//! * [`layout`] — generate the immutable `/v1/...` artifact tree from a bundle,
//!   either to disk or as an in-memory map ([`layout::generate_artifacts`]).
//! * [`server`] — a tiny dev/demo HTTP server over a generated directory (local
//!   testing only; production is "serve the directory statically"). It also
//!   exposes the dynamic `GET /verify/group/<id>` endpoint.
//! * [`live`] — a live, lazily-refreshed server that reads the commit log
//!   straight from Turso and rebuilds the same `/v1` surface in memory on demand
//!   (single-flight, at most one DB pull per TTL). New commits appear within the
//!   TTL with no idle DB load.
//! * [`remote`] — fetch the static API over HTTP and verify the whole log
//!   trusting only the public key, reusing slice 1's verifiers.
//! * [`group`] — the one shared per-group verifier the CLI, the static endpoint,
//!   and the live endpoint all call, so their verdicts cannot diverge.
//! * [`account`] — the account-key tenant's analogue of [`group`]: the one
//!   shared per-user key-history verifier. A second, fully separate Merkle tree
//!   (its STHs signed under a domain-separated context) served under
//!   `/v1/account-keys/...`; the commit-log `/v1` surface is untouched.
//!
//! The [`verifiable_log`] core stays dependency-pure: all HTTP lives here.

pub mod account;
pub mod bundle;
pub mod error;
pub mod group;
pub mod layout;
pub mod live;
pub mod release;
pub mod remote;
pub mod server;

pub use account::{verify_account, verify_account_in_bundle, AccountKeyVersion, AccountReport};
pub use bundle::{AccountManifest, Bundle, Manifest, PublicKeyDoc};
pub use error::{Result, ServeError};
pub use group::{verify_group, verify_group_in_bundle, GroupCommit, GroupReport};
pub use layout::{
    generate, generate_account, generate_account_artifacts, generate_artifacts, load_bundle,
    ACCOUNT_API_PREFIX, API_VERSION,
};
pub use live::LiveServer;
pub use remote::{verify_remote, Report};
pub use server::DevServer;
