//! Builds signed monitor bundles from Pollis data (#330), one tree per tenant:
//! [`commit_log`], [`account_key`] and [`binaries`]. Tenant modules are pure;
//! [`source`] reads the rows, hashing each `commit_data` blob and never
//! returning, logging or persisting the raw bytes.

pub mod account_key;
pub mod binaries;
pub mod builder;
pub mod commit_log;
pub mod error;
pub mod keys;
/// Row types and guards, plus (behind `db-source`) the Turso reader. The reader
/// is gated so verification-only consumers don't link `libsql` (#987).
pub mod source;

pub use account_key::{AccountKeyInvariant, AccountKeyLeaf};
pub use binaries::{BinaryInvariant, BinaryRecord, Layer, Toolchain};
pub use builder::{
    build_account_bundle, build_binaries_bundle, build_bundle, Bundle, RetiredKey,
};
pub use commit_log::{
    derive_conversation_pseudonym, derive_sender_pseudonym, window_for_seq, CommitLeaf,
    CommitLogInvariant, PSEUDONYM_WINDOW_SIZE, TENANT,
};
pub use error::{BuilderError, Result};
pub use source::{AccountKeyRow, CommitRow};
#[cfg(feature = "db-source")]
pub use source::{connect, read_account_key_log, read_commit_log};
