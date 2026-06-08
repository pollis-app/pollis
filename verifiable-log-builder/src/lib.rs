//! # verifiable-log-builder
//!
//! Slice 2 of the Key Transparency work (issue #330): reads real MLS commit
//! data from a Turso/libSQL database and turns it into the signed "monitor
//! bundle" that slice 1's [`verifiable_log`] `monitor` CLI already verifies.
//!
//! Slice 1 (the Merkle/STH/proof core) is depended on, never reimplemented.
//! This crate adds exactly three things on top:
//!
//! * [`source`] — a libSQL reader for `mls_commit_log` (remote Turso or a local
//!   SQLite file). It hashes each `commit_data` blob and drops the raw bytes —
//!   they are never returned, logged, or persisted.
//! * [`commit_log`] — the **mls-commit-log** tenant: a frozen canonical leaf
//!   encoding and [`commit_log::CommitLogInvariant`], the globally-auditable
//!   form of #357 (no fork; no epoch regression per conversation). Pure, no IO.
//! * [`builder`] — appends every commit to a [`verifiable_log::VerifiableLog`],
//!   signs Signed Tree Heads, and emits the [`builder::Bundle`].
//!
//! Out of scope (later slices): any HTTP/serve layer, real key custody, deep
//! MLS authorization of committers, the account-key tenant, the WASM explorer.

pub mod builder;
pub mod commit_log;
pub mod error;
pub mod keys;
pub mod source;

pub use builder::{build_bundle, Bundle};
pub use commit_log::{CommitLeaf, CommitLogInvariant, TENANT};
pub use error::{BuilderError, Result};
pub use source::{connect, read_commit_log, CommitRow};
