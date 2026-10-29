//! # verifiable-log-serve
//!
//! The Key Transparency serve layer (#330): turns a signed monitor bundle into an
//! immutable static artifact tree implementing the log's public read API, plus a
//! dev HTTP server and a "fetch over HTTP and verify" path. Every artifact is
//! immutable (an STH for `tree_size = N` never changes), so it is static files,
//! not a query service.
//!
//! * [`layout`] — generate the `/v1/...` artifact tree, to disk or in memory.
//! * [`server`] — dev/demo HTTP server over a generated directory.
//! * [`live`] — lazily-refreshed server reading the commit log from Turso.
//! * [`remote`] — fetch the static API and verify it trusting only the public key.
//! * [`group`] / [`account`] — the one shared per-group / per-user verifier, so
//!   every caller's verdicts agree. Account keys are a separate Merkle tree with
//!   domain-separated STHs under `/v1/account-keys/...`.
//!
//! The [`verifiable_log`] core stays dependency-pure: all HTTP lives here.

pub mod account;
pub mod bundle;
pub mod error;
pub mod group;
pub mod layout;
pub mod pinned;
/// Behind `live`, the only part needing a database, so verification-only
/// consumers don't link `libsql` (#987).
#[cfg(feature = "live")]
pub mod live;
pub mod release;
pub mod remote;
pub mod server;

pub use account::{
    verify_account, verify_account_in_bundle, verify_account_in_bundle_at, AccountKeyVersion,
    AccountReport,
};
pub use bundle::{
    AccountManifest, BinaryManifest, Bundle, Manifest, PublicKeyDoc, PublicKeyEntry,
};
// Re-exported so pollis-core needn't depend on `verifiable-log` directly.
pub use verifiable_log::{key_id_for, verifying_key_from_hex, KEY_ID_HEX_LEN, STH_PUB_LEN, STH_SIG_LEN};
// Root-signed key sets (#754).
pub use verifiable_log::{root_key_from_hex, KeySetStatement, SignerEntry};
pub use error::{Result, ServeError};
pub use group::{
    verify_group, verify_group_in_bundle, verify_group_in_bundle_at, GroupCommit, GroupReport,
};
pub use layout::{
    generate, generate_account, generate_account_artifacts, generate_artifacts, generate_binaries,
    generate_binaries_artifacts, load_bundle, ACCOUNT_API_PREFIX, API_VERSION, BINARIES_API_PREFIX,
};
#[cfg(feature = "live")]
pub use live::LiveServer;
pub use remote::{verify_remote, Report};
pub use server::DevServer;
