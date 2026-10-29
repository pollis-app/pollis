//! # verifiable-log-serve
//! The serve layer: turns a signed monitor bundle into the static `/v1` artifact tree, plus dev
//! and live servers and the HTTP verifiers.

pub mod account;
pub mod bundle;
pub mod error;
pub mod group;
pub mod layout;
pub mod pinned;
/// The live, lazily-refreshed server that reads the commit log straight out of
/// Turso. Behind `live`, so a verification-only consumer does not link `libsql` (#987).
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
// Re-exported so pollis-core can check key lengths without a direct dependency.
pub use verifiable_log::{key_id_for, verifying_key_from_hex, KEY_ID_HEX_LEN, STH_PUB_LEN, STH_SIG_LEN};
// Root-signed key sets (#754), re-exported for the client.
pub use verifiable_log::{root_key_from_hex, KeySetStatement, SignerEntry};
pub use pinned::{key_set_trust_via, KeySetOutcome, KeySetTrust, PINNED_LOG_ROOT_KEYS};
// The per-tree STH contexts a key set authorises signers for.
pub use verifiable_log::{
    STH_CONTEXT_ACCOUNT_KEYS, STH_CONTEXT_BINARIES, STH_CONTEXT_COMMIT_LOG,
    STH_CONTEXT_COMMIT_LOG_2,
};
pub use error::{Result, ServeError};
pub use group::{
    verify_group, verify_group_in_bundle, verify_group_in_bundle_at,
    verify_group_in_bundle_with_context, GroupCommit, GroupReport,
};
pub use layout::{
    generate, generate_account, generate_account_artifacts, generate_artifacts, generate_binaries,
    generate_binaries_artifacts, generate_commit_log_2, generate_commit_log_2_artifacts,
    load_bundle, ACCOUNT_API_PREFIX, API_VERSION, BINARIES_API_PREFIX, COMMIT_LOG_2_API_PREFIX,
    RESET_STATEMENT_PATH,
};
#[cfg(feature = "live")]
pub use live::LiveServer;
pub use remote::{verify_remote, Report};
pub use server::DevServer;
