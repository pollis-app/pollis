//! Proof wire types and their standalone verifiers. With [`crate::sth::Sth`]
//! and [`crate::log::Entry`] these are the frozen JSON contract (`README.md`).

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::hash::{from_hex, Hash};
use crate::log::Entry;
use crate::merkle;
use crate::sth::Sth;

/// Proof that a leaf is committed by an STH's root. The leaf itself is passed
/// to the verifier separately as an [`Entry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InclusionProof {
    pub leaf_index: u64,
    pub tree_size: u64,
    /// Sibling hashes bottom-up, lowercase hex (32 bytes each).
    pub audit_path: Vec<String>,
}

/// Proof that the size-`first_size` tree is a prefix of the size-`second_size`
/// tree (the log only appended).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsistencyProof {
    pub first_size: u64,
    pub second_size: u64,
    /// Consistency path, lowercase hex (32 bytes each).
    pub path: Vec<String>,
}

fn decode_path(hexes: &[String]) -> Result<Vec<Hash>> {
    hexes.iter().map(|h| from_hex(h)).collect()
}

/// Verify that `entry` is included in the tree committed by `sth`. Any
/// mismatch, including malformed hex or a tree-size disagreement, is `false`.
pub fn verify_inclusion_proof(entry: &Entry, proof: &InclusionProof, sth: &Sth) -> bool {
    if proof.tree_size != sth.tree_size {
        return false;
    }
    let Ok(root) = sth.root_bytes() else {
        return false;
    };
    let Ok(path) = decode_path(&proof.audit_path) else {
        return false;
    };
    merkle::verify_inclusion(
        &entry.leaf_hash(),
        proof.leaf_index as usize,
        proof.tree_size as usize,
        &path,
        &root,
    )
}

/// Verify a consistency proof between two STHs. The proof's sizes must match
/// both STHs and the path must reconstruct both roots; otherwise `false`.
pub fn verify_consistency_proof(old: &Sth, new: &Sth, proof: &ConsistencyProof) -> bool {
    if proof.first_size != old.tree_size || proof.second_size != new.tree_size {
        return false;
    }
    let Ok(old_root) = old.root_bytes() else {
        return false;
    };
    let Ok(new_root) = new.root_bytes() else {
        return false;
    };
    let Ok(path) = decode_path(&proof.path) else {
        return false;
    };
    merkle::verify_consistency(
        proof.first_size as usize,
        proof.second_size as usize,
        &path,
        &old_root,
        &new_root,
    )
}
