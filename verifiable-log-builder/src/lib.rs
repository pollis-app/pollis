//! # verifiable-log-builder
//! Reads commit and account-key data from libSQL and turns each tenant into a signed monitor
//! bundle.

pub mod account_key;
pub mod binaries;
pub mod builder;
pub mod commit_leaves;
pub mod commit_log;
pub mod commit_log_reset;
pub mod error;
pub mod keys;
/// The log's row types, their hashing/emptiness guards, and — behind
/// `db-source` the reader; gated so verification-only consumers skip `libsql` (#987).
pub mod source;

pub use account_key::{AccountKeyInvariant, AccountKeyLeaf};
pub use binaries::{BinaryInvariant, BinaryRecord, Layer, Toolchain};
pub use builder::{
    build_account_bundle, build_binaries_bundle, build_bundle, build_bundle_from_leaves,
    build_commit_log_2_bundle, Bundle, RetiredKey,
};
pub use commit_log::{
    derive_conversation_pseudonym, derive_sender_pseudonym, window_for_seq, CommitLeaf,
    CommitLogInvariant, PSEUDONYM_WINDOW_SIZE, TENANT,
};
pub use error::{BuilderError, Result};
pub use source::{AccountKeyRow, CommitRow};
#[cfg(feature = "db-source")]
pub use source::{connect, read_account_key_log, read_commit_log};
