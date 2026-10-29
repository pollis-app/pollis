//! Wire types for the two proof shapes plus their standalone verifiers.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::hash::{from_hex, Hash};
use crate::log::Entry;
use crate::merkle;
use crate::sth::Sth;

/// Proof that a leaf is committed by an STH root; the leaf is supplied separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InclusionProof {
    pub leaf_index: u64,
    pub tree_size: u64,
    /// Sibling hashes bottom-up, lowercase hex (32 bytes each).
    pub audit_path: Vec<String>,
}

/// Proof that the size-`first_size` tree is a prefix of the size-`second_size`
/// tree — i.e. the log only appended and never rewrote history.
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

/// Verify that `entry` is included in the tree committed by `sth`
/// (leaf + proof + STH -> bool). Returns `false` on any mismatch.
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

/// Verify a consistency proof between two STHs (old STH + new STH + proof ->
/// bool). Returns `false` on any mismatch.
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
