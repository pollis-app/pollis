//! RFC 6962 Merkle Tree Hash (MTH), inclusion proofs, and consistency
//! proofs, plus the standalone verifiers from RFC 9162 (6962-bis).
//!
//! Generation works over a slice of *leaf hashes* (each already
//! `SHA-256(0x00 || entry_bytes)`). The append-only [`crate::log::VerifiableLog`]
//! keeps that slice and computes the structure on demand, so an "append" is
//! just pushing one more leaf hash.
//!
//! Verification is implemented exactly as specified in RFC 9162 §2.1.3.2
//! (inclusion) and §2.1.4.2 (consistency): bit-twiddling walks that need only
//! the audit path, the leaf/old roots, and the tree sizes — never the whole
//! tree.

use crate::hash::{empty_root, node_hash, Hash};

/// Largest power of two strictly less than `n` (requires `n >= 2`). This is
/// the split point `k` used throughout RFC 6962's recursive definitions.
fn largest_power_of_two_below(n: usize) -> usize {
    debug_assert!(n >= 2);
    let mut k = 1;
    while k << 1 < n {
        k <<= 1;
    }
    k
}

/// Merkle Tree Hash of a list of leaf hashes (RFC 6962 §2.1).
///
/// * `MTH({})`   = SHA-256()
/// * `MTH({d0})` = d0 (the leaf hash is already `H(0x00 || entry)`)
/// * `MTH(D[n])` = `H(0x01 || MTH(D[0:k]) || MTH(D[k:n]))`, `k` = largest
///   power of two `< n`.
pub fn root_hash(leaves: &[Hash]) -> Hash {
    match leaves.len() {
        0 => empty_root(),
        1 => leaves[0],
        n => {
            let k = largest_power_of_two_below(n);
            node_hash(&root_hash(&leaves[..k]), &root_hash(&leaves[k..]))
        }
    }
}

/// Audit path proving the leaf at `index` is included in `MTH(leaves)`
/// (RFC 6962 §2.1.1, `PATH`).
///
/// Returns the sibling hashes bottom-up. `index` must be `< leaves.len()`;
/// callers in this crate guarantee that.
pub fn inclusion_path(index: usize, leaves: &[Hash]) -> Vec<Hash> {
    let n = leaves.len();
    if n <= 1 {
        return Vec::new();
    }
    let k = largest_power_of_two_below(n);
    if index < k {
        let mut path = inclusion_path(index, &leaves[..k]);
        path.push(root_hash(&leaves[k..]));
        path
    } else {
        let mut path = inclusion_path(index - k, &leaves[k..]);
        path.push(root_hash(&leaves[..k]));
        path
    }
}

/// Consistency path proving the size-`first` tree is a prefix of the
/// size-`second` tree (RFC 6962 §2.1.2, `PROOF`). Requires
/// `0 < first <= second <= leaves.len()`.
pub fn consistency_path(first: usize, leaves: &[Hash]) -> Vec<Hash> {
    subproof(first, &leaves[..], true)
}

/// RFC 6962 `SUBPROOF`. `b` tracks whether the current subtree is the older
/// tree in its entirety (in which case the verifier can recompute its root,
/// so we omit it from the path).
fn subproof(m: usize, leaves: &[Hash], b: bool) -> Vec<Hash> {
    let n = leaves.len();
    if m == n {
        if b {
            return Vec::new();
        }
        return vec![root_hash(leaves)];
    }
    let k = largest_power_of_two_below(n);
    if m <= k {
        let mut path = subproof(m, &leaves[..k], b);
        path.push(root_hash(&leaves[k..]));
        path
    } else {
        let mut path = subproof(m - k, &leaves[k..], false);
        path.push(root_hash(&leaves[..k]));
        path
    }
}

/// Verify an inclusion proof (RFC 9162 §2.1.3.2). Recomputes the root from
/// `leaf_hash`, `leaf_index`, `tree_size`, and the audit `path`, then compares
/// it to `root`. Returns `false` on any inconsistency — never panics.
pub fn verify_inclusion(
    leaf_hash: &Hash,
    leaf_index: usize,
    tree_size: usize,
    path: &[Hash],
    root: &Hash,
) -> bool {
    if leaf_index >= tree_size {
        return false;
    }
    let mut fln = leaf_index;
    let mut sn = tree_size - 1;
    let mut r = *leaf_hash;
    for p in path {
        if sn == 0 {
            return false;
        }
        if fln & 1 == 1 || fln == sn {
            r = node_hash(p, &r);
            if fln & 1 == 0 {
                while fln & 1 == 0 && fln != 0 {
                    fln >>= 1;
                    sn >>= 1;
                }
            }
        } else {
            r = node_hash(&r, p);
        }
        fln >>= 1;
        sn >>= 1;
    }
    sn == 0 && r == *root
}

/// Verify a consistency proof (RFC 9162 §2.1.4.2) between an older tree of
/// size `first` with root `first_hash` and a newer tree of size `second` with
/// root `second_hash`. Returns `false` on any inconsistency — never panics.
pub fn verify_consistency(
    first: usize,
    second: usize,
    path: &[Hash],
    first_hash: &Hash,
    second_hash: &Hash,
) -> bool {
    if first == 0 || first > second {
        return false;
    }
    // Identical trees: the proof is empty and the roots must match.
    if first == second {
        return path.is_empty() && first_hash == second_hash;
    }

    // When `first` is a power of two the older root is omitted from the path
    // (the verifier already holds it), so prepend it.
    let mut work: Vec<Hash> = Vec::with_capacity(path.len() + 1);
    if first.is_power_of_two() {
        work.push(*first_hash);
    }
    work.extend_from_slice(path);

    if work.is_empty() {
        return false;
    }

    let mut fln = first - 1;
    let mut sn = second - 1;
    while fln & 1 == 1 {
        fln >>= 1;
        sn >>= 1;
    }

    let mut fr = work[0];
    let mut sr = work[0];
    for c in &work[1..] {
        if sn == 0 {
            return false;
        }
        if fln & 1 == 1 || fln == sn {
            fr = node_hash(c, &fr);
            sr = node_hash(c, &sr);
            if fln & 1 == 0 {
                while fln & 1 == 0 && fln != 0 {
                    fln >>= 1;
                    sn >>= 1;
                }
            }
        } else {
            sr = node_hash(&sr, c);
        }
        fln >>= 1;
        sn >>= 1;
    }

    fr == *first_hash && sr == *second_hash && sn == 0
}
