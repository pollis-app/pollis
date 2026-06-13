//! RFC 6962 / RFC 9162 known-answer tests against the canonical Certificate
//! Transparency test vectors (the 8-leaf set published in RFC 6962 and the
//! `google/certificate-transparency` Go repository, `merkle/merkle_test.go`).
//!
//! Every expected value below is a HARDCODED hex constant copied from the
//! spec/CT vectors. None is derived by calling this crate's own code — these
//! are true known-answer tests, not self-referential round-trips. We feed the
//! canonical leaf inputs through our hashing + Merkle math and assert that the
//! roots, inclusion paths, and consistency paths come out byte-exact against
//! the published numbers.
//!
//! ## Why these run at the hash/merkle layer, not through `Entry`
//!
//! Our application leaf encoding wraps payloads in a tenant frame —
//! `len(tenant) as u32 BE || tenant || data` (see [`verifiable_log::Entry`]).
//! That framing prepends bytes, so a framed entry's hashed bytes can NEVER be
//! byte-equal to a bare RFC leaf input (e.g. the empty input `""` is impossible
//! to reproduce — the frame is at minimum 4 bytes). The RFC vectors therefore
//! cannot be applied at the `Entry` layer.
//!
//! They CAN be applied — bit-exactly — one layer down, where the raw leaf input
//! is exactly what gets hashed under the `0x00` prefix: `hash::leaf_hash(input)`
//! computes `SHA-256(0x00 || input)`, and `merkle::{root_hash, inclusion_path,
//! consistency_path, verify_inclusion, verify_consistency}` operate directly on
//! those leaf hashes. So the Merkle tree math this crate signs prod STHs over is
//! demonstrably RFC 6962-conformant; only the per-tenant leaf framing (an
//! intentional multi-tenancy choice) sits above it. The
//! `tenant_frame_cannot_reproduce_raw_rfc_leaf` test pins that distinction.

use verifiable_log::hash::{self, Hash};
use verifiable_log::merkle;
use verifiable_log::Entry;

/// Decode a hardcoded vector hex string into a 32-byte hash. Uses the crate's
/// own `from_hex` only as a hex *decoder* — the bytes themselves are the
/// external known answer, not anything this crate computed.
fn h(s: &str) -> Hash {
    hash::from_hex(s).expect("vector hex must be 32 bytes")
}

// ── Canonical leaf inputs (RFC 6962 / CT `merkle_test.go`) ──────────────────
// The 8 leaf inputs, as raw bytes. Leaf 0 is the empty input.
fn leaf_inputs() -> Vec<Vec<u8>> {
    [
        "",
        "00",
        "10",
        "2021",
        "3031",
        "40414243",
        "5051525354555657",
        "606162636465666768696a6b6c6d6e6f",
    ]
    .iter()
    .map(|s| hex::decode(s).unwrap())
    .collect()
}

/// Leaf hashes for the first `n` canonical inputs, computed through our
/// `hash::leaf_hash` (the layer that hashes the raw RFC input under `0x00`).
fn leaves(n: usize) -> Vec<Hash> {
    leaf_inputs()
        .iter()
        .take(n)
        .map(|input| hash::leaf_hash(input))
        .collect()
}

// ── Known-answer constants ──────────────────────────────────────────────────

/// Merkle Tree Hash of the empty list = SHA-256() (RFC 6962 §2.1).
const EMPTY_ROOT: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Published roots (Merkle Tree Hash) for tree sizes 1..=8 over the canonical
/// inputs. Index 0 here is tree size 1.
const ROOTS: [&str; 8] = [
    "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
    "fac54203e7cc696cf0dfcb42c92a1d9dbaf70ad9e621f4bd8d98662f00e3c125",
    "aeb6bcfe274b70a14fb067a5e5578264db0fa9b51af5e0ba159158f329e06e77",
    "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7",
    "4e3bbb1f7b478dcfe71fb631631519a3bca12c9aefca1612bfce4c13a86264d4",
    "76e67dadbcdf1e10e1b74ddc608abd2f98dfb16fbce75277b5232a127f2087ef",
    "ddb89be403809e325750d3d263cd78929c2942b7942a34b77e122c9594a74c8c",
    "5dc9da79a70659a9ad559cb701ded9a2ab9d823aad2f4960cfe370eff4604328",
];

// ── Empty tree + per-size roots ─────────────────────────────────────────────

#[test]
fn empty_tree_hash_matches_rfc() {
    assert_eq!(hash::empty_root(), h(EMPTY_ROOT));
    // The tree-of-zero-leaves root takes the same path through merkle::root_hash.
    assert_eq!(merkle::root_hash(&[]), h(EMPTY_ROOT));
}

#[test]
fn known_leaf_hashes_match_rfc() {
    // Spot-check the leaf-hash primitive against published values:
    // leaf 0 = SHA-256(0x00), leaf 1 = SHA-256(0x00 || 0x00).
    assert_eq!(
        hash::leaf_hash(b""),
        h("6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d"),
    );
    assert_eq!(
        hash::leaf_hash(&[0x00]),
        h("96a296d224f285c67bee93c30f8a309157f0daa35dc5b87e410b78630a09cfc7"),
    );
}

#[test]
fn roots_match_rfc_for_every_size_1_through_8() {
    for n in 1..=8usize {
        let got = merkle::root_hash(&leaves(n));
        assert_eq!(
            got,
            h(ROOTS[n - 1]),
            "root for tree size {n} must match the published CT vector",
        );
    }
}

// ── Inclusion proofs ────────────────────────────────────────────────────────
//
// Published CT inclusion vectors (`merkle_test.go`, 1-based leaf indices in the
// source, written here 0-based): (leaf_index, tree_size, audit_path).

struct InclusionVector {
    leaf_index: usize,
    tree_size: usize,
    audit_path: &'static [&'static str],
}

const INCLUSION_VECTORS: &[InclusionVector] = &[
    // leaf 0 in tree 8
    InclusionVector {
        leaf_index: 0,
        tree_size: 8,
        audit_path: &[
            "96a296d224f285c67bee93c30f8a309157f0daa35dc5b87e410b78630a09cfc7",
            "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
            "6b47aaf29ee3c2af9af889bc1fb9254dabd31177f16232dd6aab035ca39bf6e4",
        ],
    },
    // leaf 5 in tree 8
    InclusionVector {
        leaf_index: 5,
        tree_size: 8,
        audit_path: &[
            "bc1a0643b12e4d2d7c77918f44e0f4f79a838b6cf9ec5b5c283e1f4d88599e6b",
            "ca854ea128ed050b41b35ffc1b87b8eb2bde461e9e3b5596ece6b9d5975a0ae0",
            "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7",
        ],
    },
    // leaf 2 in tree 3
    InclusionVector {
        leaf_index: 2,
        tree_size: 3,
        audit_path: &["fac54203e7cc696cf0dfcb42c92a1d9dbaf70ad9e621f4bd8d98662f00e3c125"],
    },
    // leaf 1 in tree 5
    InclusionVector {
        leaf_index: 1,
        tree_size: 5,
        audit_path: &[
            "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
            "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
            "bc1a0643b12e4d2d7c77918f44e0f4f79a838b6cf9ec5b5c283e1f4d88599e6b",
        ],
    },
];

#[test]
fn inclusion_paths_generate_to_published_vectors() {
    for v in INCLUSION_VECTORS {
        let path = merkle::inclusion_path(v.leaf_index, &leaves(v.tree_size));
        let expected: Vec<Hash> = v.audit_path.iter().map(|s| h(s)).collect();
        assert_eq!(
            path, expected,
            "generated inclusion path for leaf {} in tree {} must match the CT vector",
            v.leaf_index, v.tree_size,
        );
    }
}

#[test]
fn inclusion_proofs_verify_against_published_vectors() {
    for v in INCLUSION_VECTORS {
        let leaf = hash::leaf_hash(&leaf_inputs()[v.leaf_index]);
        let path: Vec<Hash> = v.audit_path.iter().map(|s| h(s)).collect();
        let root = h(ROOTS[v.tree_size - 1]);
        assert!(
            merkle::verify_inclusion(&leaf, v.leaf_index, v.tree_size, &path, &root),
            "published inclusion proof for leaf {} in tree {} must verify",
            v.leaf_index, v.tree_size,
        );
        // Negative control: the same proof against a different root must fail.
        let wrong_root = h(EMPTY_ROOT);
        assert!(
            !merkle::verify_inclusion(&leaf, v.leaf_index, v.tree_size, &path, &wrong_root),
            "inclusion proof must not verify against the wrong root",
        );
    }
}

// ── Consistency proofs ──────────────────────────────────────────────────────
//
// Published CT consistency vectors (`merkle_test.go`): (first, second, path).
// These are the exact pairs the CT vectors publish.

struct ConsistencyVector {
    first: usize,
    second: usize,
    path: &'static [&'static str],
}

const CONSISTENCY_VECTORS: &[ConsistencyVector] = &[
    // 1 -> 8
    ConsistencyVector {
        first: 1,
        second: 8,
        path: &[
            "96a296d224f285c67bee93c30f8a309157f0daa35dc5b87e410b78630a09cfc7",
            "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
            "6b47aaf29ee3c2af9af889bc1fb9254dabd31177f16232dd6aab035ca39bf6e4",
        ],
    },
    // 6 -> 8
    ConsistencyVector {
        first: 6,
        second: 8,
        path: &[
            "0ebc5d3437fbe2db158b9f126a1d118e308181031d0a949f8dededebc558ef6a",
            "ca854ea128ed050b41b35ffc1b87b8eb2bde461e9e3b5596ece6b9d5975a0ae0",
            "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7",
        ],
    },
    // 2 -> 5
    ConsistencyVector {
        first: 2,
        second: 5,
        path: &[
            "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
            "bc1a0643b12e4d2d7c77918f44e0f4f79a838b6cf9ec5b5c283e1f4d88599e6b",
        ],
    },
];

#[test]
fn consistency_paths_generate_to_published_vectors() {
    for v in CONSISTENCY_VECTORS {
        let path = merkle::consistency_path(v.first, &leaves(v.second));
        let expected: Vec<Hash> = v.path.iter().map(|s| h(s)).collect();
        assert_eq!(
            path, expected,
            "generated consistency path {} -> {} must match the CT vector",
            v.first, v.second,
        );
    }
}

#[test]
fn consistency_proofs_verify_against_published_vectors() {
    for v in CONSISTENCY_VECTORS {
        let path: Vec<Hash> = v.path.iter().map(|s| h(s)).collect();
        let first_root = h(ROOTS[v.first - 1]);
        let second_root = h(ROOTS[v.second - 1]);
        assert!(
            merkle::verify_consistency(v.first, v.second, &path, &first_root, &second_root),
            "published consistency proof {} -> {} must verify",
            v.first, v.second,
        );
        // Negative control: swapping in the wrong older root must fail.
        let wrong_first = h(EMPTY_ROOT);
        assert!(
            !merkle::verify_consistency(v.first, v.second, &path, &wrong_first, &second_root),
            "consistency proof must not verify against the wrong first root",
        );
    }
}

// ── The framing-layer finding ───────────────────────────────────────────────

#[test]
fn tenant_frame_cannot_reproduce_raw_rfc_leaf() {
    // An `Entry` prepends `len(tenant) as u32 BE || tenant` before the payload,
    // so even with an empty tenant the hashed bytes differ from the bare RFC
    // input — which is exactly why the vectors above are applied at the
    // hash/merkle layer, not through `Entry`.
    let raw = &leaf_inputs()[1]; // input "00"
    let framed = Entry::new("", raw.clone());
    assert_ne!(
        framed.leaf_hash(),
        hash::leaf_hash(raw),
        "tenant framing must change the hashed bytes vs the bare RFC leaf input",
    );
    // The framed bytes carry the 4-byte length prefix (empty tenant => 0u32).
    assert_eq!(&framed.encode()[..4], &[0, 0, 0, 0]);
    assert_eq!(&framed.encode()[4..], &raw[..]);
}
