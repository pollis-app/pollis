//! Static layout generator: turn a signed [`Bundle`] into the immutable
//! directory tree that implements the log's public read API.
//!
//! Every file written here is **write-once / immutable**: an STH for a given
//! tree size, a proof for a given `(leaf, tree_size)`, an entry at a given
//! index — none of them ever change once published. That is what makes the
//! whole API trivially cacheable and host-agnostic: it is just a directory of
//! JSON blobs you can drop on any static host (R2, Pages, an edge CDN). The two
//! exceptions that *move* as the log grows — `sth/latest.json` and `index.json`
//! — are the only documents a client must refetch.
//!
//! ## URL → file mapping (the read API)
//!
//! | URL                                                   | Contents                          | Cache     |
//! |-------------------------------------------------------|-----------------------------------|-----------|
//! | `/v1/public_key.json`                                 | [`PublicKeyDoc`]                  | immutable |
//! | `/v1/index.json`                                      | [`Manifest`] (discovery)          | short     |
//! | `/v1/sth/latest.json`                                 | newest [`Sth`]                    | short     |
//! | `/v1/sth/<tree_size>.json`                            | [`Sth`] at that size              | immutable |
//! | `/v1/entries.json`                                    | full ordered `[Entry]`            | immutable |
//! | `/v1/entries/<index>.json`                            | one [`Entry`]                     | immutable |
//! | `/v1/proof/inclusion/<tree_size>/<leaf_index>.json`   | [`InclusionProof`]                | immutable |
//! | `/v1/proof/consistency/<first>-<second>.json`         | [`ConsistencyProof`]              | immutable |
//!
//! The file path under the output root mirrors the URL exactly (drop the
//! leading `/`), so serving is a literal static-file mapping.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::bundle::{
    Bundle, ConsistencyRef, InclusionRef, Manifest, PublicKeyDoc,
};
use crate::error::{Result, ServeError};

/// API version segment all artifacts live under.
pub const API_VERSION: &str = "v1";

/// Write the full immutable artifact tree for `bundle` under `root`, returning
/// the [`Manifest`] that was published at `/v1/index.json`.
///
/// `root` is created if missing. Existing files with the same names are
/// overwritten — generating from the same bundle is idempotent, and since every
/// artifact is content-addressed by its `(size, index)` coordinates, a larger
/// bundle only ever *adds* files, it never rewrites an existing immutable one.
pub fn generate(bundle: &Bundle, root: &Path) -> Result<Manifest> {
    let v1 = root.join(API_VERSION);
    std::fs::create_dir_all(&v1)?;

    // /v1/public_key.json
    write_json(
        &v1.join("public_key.json"),
        &PublicKeyDoc {
            public_key: bundle.public_key.clone(),
        },
    )?;

    // /v1/sth/<size>.json for every head, plus latest.json for the largest.
    let sth_dir = v1.join("sth");
    std::fs::create_dir_all(&sth_dir)?;
    let mut sth_sizes: Vec<u64> = Vec::with_capacity(bundle.sths.len());
    for sth in &bundle.sths {
        write_json(&sth_dir.join(format!("{}.json", sth.tree_size)), sth)?;
        sth_sizes.push(sth.tree_size);
    }
    sth_sizes.sort_unstable();
    sth_sizes.dedup();

    let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
    if let Some(latest) = latest {
        write_json(&sth_dir.join("latest.json"), latest)?;
    }

    // /v1/entries.json and /v1/entries/<index>.json
    write_json(&v1.join("entries.json"), &bundle.entries)?;
    if !bundle.entries.is_empty() {
        let entries_dir = v1.join("entries");
        std::fs::create_dir_all(&entries_dir)?;
        for (i, entry) in bundle.entries.iter().enumerate() {
            write_json(&entries_dir.join(format!("{i}.json")), entry)?;
        }
    }

    // /v1/proof/inclusion/<tree_size>/<leaf_index>.json
    let mut inclusion_refs: Vec<InclusionRef> = Vec::with_capacity(bundle.inclusion.len());
    for check in &bundle.inclusion {
        let ts = check.proof.tree_size;
        let li = check.proof.leaf_index;
        let dir = v1.join("proof").join("inclusion").join(ts.to_string());
        std::fs::create_dir_all(&dir)?;
        write_json(&dir.join(format!("{li}.json")), &check.proof)?;
        inclusion_refs.push(InclusionRef {
            tree_size: ts,
            leaf_index: li,
        });
    }

    // /v1/proof/consistency/<first>-<second>.json
    let mut consistency_refs: Vec<ConsistencyRef> = Vec::with_capacity(bundle.consistency.len());
    if !bundle.consistency.is_empty() {
        let dir = v1.join("proof").join("consistency");
        std::fs::create_dir_all(&dir)?;
        for check in &bundle.consistency {
            let f = check.proof.first_size;
            let s = check.proof.second_size;
            write_json(&dir.join(format!("{f}-{s}.json")), &check.proof)?;
            consistency_refs.push(ConsistencyRef {
                first: f,
                second: s,
            });
        }
    }

    // /v1/index.json — the discovery manifest, written last so it only ever
    // advertises artifacts that are already on disk.
    let manifest = Manifest {
        version: API_VERSION.to_string(),
        public_key: bundle.public_key.clone(),
        entry_count: bundle.entries.len() as u64,
        latest_tree_size: latest.map(|s| s.tree_size),
        sth_sizes,
        inclusion: inclusion_refs,
        consistency: consistency_refs,
        enforce_unique: bundle.enforce_unique.clone(),
    };
    write_json(&v1.join("index.json"), &manifest)?;

    Ok(manifest)
}

/// Read and parse a bundle JSON file from disk.
pub fn load_bundle(path: &Path) -> Result<Bundle> {
    let raw = std::fs::read_to_string(path)?;
    let bundle: Bundle =
        serde_json::from_str(&raw).map_err(|e| ServeError::BadBundle(e.to_string()))?;
    Ok(bundle)
}

/// Serialize `value` as pretty JSON to `path` (parent dirs assumed to exist).
fn write_json<T: Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value)?;
    std::fs::write(path, json)?;
    Ok(())
}
