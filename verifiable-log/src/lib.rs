//! # verifiable-log
//! A generic, tenant-agnostic **verifiable append-only log** built on an
//! RFC 6962-style Merkle tree, with ML-DSA-44 Signed Tree Heads and the two
//! standard proofs (inclusion + consistency). It is the reusable
//! Key-Transparency core that later tenants (an MLS commit log, an
//! account-key directory) and a serve layer are built on.

pub mod bundle;
pub mod error;
pub mod hash;
pub mod keyset;
pub mod log;
pub mod merkle;
pub mod monitor;
pub mod proof;
pub mod reset;
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
pub use reset::{
    HeadClass, ListedHead, ResetBody, ResetStatement, RetiredTree, SuccessorTree, RESET_DOMAIN,
    RESET_FORMAT_VERSION,
};
pub use sth::{
    is_equivocation, key_id_for, key_set_authority_for, signing_key_from_seed_hex,
    sth_context_for_tree, verifying_key_from_hex, SigningKey, Sth, VerifyingKey, KEY_ID_HEX_LEN,
    STH_CONTEXTS, STH_CONTEXT_ACCOUNT_KEYS, STH_CONTEXT_BINARIES, STH_CONTEXT_COMMIT_LOG,
    STH_CONTEXT_COMMIT_LOG_2, STH_PUB_LEN, STH_SIG_LEN,
};
