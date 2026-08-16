//! # verifiable-log
//!
//! A generic, tenant-agnostic **verifiable append-only log** built on an
//! RFC 6962-style Merkle tree, with Ed25519 Signed Tree Heads and the two
//! standard proofs (inclusion + consistency). It is the reusable
//! Key-Transparency core that later tenants (an MLS commit log, an
//! account-key directory) and a serve layer are built on.
//!
//! The core is deliberately **deploy-target-agnostic**: no network, no
//! database, no clock. Timestamps are passed in by the caller so behaviour is
//! deterministic and testable.
//!
//! ## Shape of the API
//!
//! * [`VerifiableLog`] — append entries, get the root, emit STHs and proofs.
//! * [`Entry`] / [`TenantInvariant`] — multi-tenant entries and the pluggable
//!   per-tenant correctness hook.
//! * [`Sth`] — Signed Tree Head; [`sth::is_equivocation`] flags conflicting heads.
//! * [`proof::verify_inclusion_proof`] / [`proof::verify_consistency_proof`] —
//!   standalone verifiers (leaf/STH + proof -> bool).
//!
//! Everything on the verification path returns `Result`/`bool` and never
//! panics. The JSON wire shapes are frozen in `README.md`.

pub mod bundle;
pub mod error;
pub mod hash;
pub mod keyset;
pub mod log;
pub mod merkle;
pub mod monitor;
pub mod proof;
pub mod sth;

pub use bundle::{Bundle, ConsistencyCheck, InclusionCheck, PublicKeyEntry};
pub use error::{Error, InvariantViolation, Result};
pub use hash::Hash;
pub use monitor::{unique_invariants, verify_bundle, VerifyOptions};
pub use keyset::{
    root_key_from_hex, KeySetStatement, SignerEntry, KEYSET_DOMAIN, KEYSET_FORMAT_VERSION,
};
pub use log::{Entry, TenantInvariant, UniqueDataInvariant, VerifiableLog};
pub use proof::{
    verify_consistency_proof, verify_inclusion_proof, ConsistencyProof, InclusionProof,
};
pub use sth::{
    is_equivocation, key_id_for, signing_key_from_seed_hex, sth_context_for_tree,
    verifying_key_from_hex, SigningKey, Sth, VerifyingKey, KEY_ID_HEX_LEN,
    STH_CONTEXTS, STH_CONTEXT_ACCOUNT_KEYS, STH_CONTEXT_BINARIES, STH_CONTEXT_COMMIT_LOG,
    STH_PUB_LEN, STH_SIG_LEN,
};
