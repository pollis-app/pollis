//! # verifiable-log-builder
//!
//! Slice 2 of the Key Transparency work (issue #330): reads real MLS commit
//! data from a Turso/libSQL database and turns it into the signed "monitor
//! bundle" that slice 1's [`verifiable_log`] `monitor` CLI already verifies.
//!
//! Slice 1 (the Merkle/STH/proof core) is depended on, never reimplemented.
//! This crate adds exactly three things on top:
//!
//! * [`source`] — a libSQL reader for `mls_commit_log` and `account_key_log`
//!   (remote Turso or a local SQLite file). It hashes each `commit_data` blob and
//!   drops the raw bytes — they are never returned, logged, or persisted; the
//!   account public key, public by design, is read out verbatim (hex).
//! * [`commit_log`] — the **mls-commit-log** tenant: a frozen canonical leaf
//!   encoding and [`commit_log::CommitLogInvariant`], the globally-auditable
//!   form of #357 (no fork; no epoch regression per conversation). Pure, no IO.
//! * [`account_key`] — the **account-key** tenant: the canonical leaf encoding
//!   and [`account_key::AccountKeyInvariant`] (no duplicate or regressing
//!   `identity_version` per user). Its own tree, signed under the
//!   domain-separated [`account_key::STH_CONTEXT`]. Pure, no IO.
//! * [`builder`] — appends every row to a [`verifiable_log::VerifiableLog`],
//!   signs Signed Tree Heads, and emits the [`builder::Bundle`] — one per tenant.
//!
//! Out of scope (later slices): real key custody, deep MLS authorization of
//! committers, the WASM explorer.

pub mod account_key;
pub mod builder;
pub mod commit_log;
pub mod error;
pub mod keys;
pub mod source;

pub use account_key::{AccountKeyInvariant, AccountKeyLeaf};
pub use builder::{build_account_bundle, build_bundle, Bundle};
pub use commit_log::{CommitLeaf, CommitLogInvariant, TENANT};
pub use error::{BuilderError, Result};
pub use source::{
    connect, read_account_key_log, read_commit_log, AccountKeyRow, CommitRow,
};
