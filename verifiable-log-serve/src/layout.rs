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

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::bundle::{
    Bundle, ConsistencyRef, InclusionRef, Manifest, PublicKeyDoc,
};
use crate::error::{Result, ServeError};

/// API version segment all artifacts live under.
pub const API_VERSION: &str = "v1";

/// The complete read API for a bundle as an in-memory map of
/// **relative path → JSON bytes** (e.g. `"v1/sth/latest.json"`), exactly the
/// content the static tree would write to disk.
///
/// This is the single source of truth for the `/v1` surface: the disk
/// [`generate`] writes this map out file-for-file, and the live server
/// ([`crate::live`]) holds it in memory and serves the bytes directly. Keying by
/// the request-relative path (no leading `/`) means a server looks an artifact
/// up with the same `path.trim_start_matches('/')` it uses for the static host,
/// so on-disk and in-memory serving cannot diverge.
pub fn generate_artifacts(bundle: &Bundle) -> Result<(Manifest, BTreeMap<String, Vec<u8>>)> {
    let mut map: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    // /v1/public_key.json
    insert_json(
        &mut map,
        format!("{API_VERSION}/public_key.json"),
        &PublicKeyDoc {
            public_key: bundle.public_key.clone(),
        },
    )?;

    // /v1/sth/<size>.json for every head, plus latest.json for the largest.
    let mut sth_sizes: Vec<u64> = Vec::with_capacity(bundle.sths.len());
    for sth in &bundle.sths {
        insert_json(&mut map, format!("{API_VERSION}/sth/{}.json", sth.tree_size), sth)?;
        sth_sizes.push(sth.tree_size);
    }
    sth_sizes.sort_unstable();
    sth_sizes.dedup();

    let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
    if let Some(latest) = latest {
        insert_json(&mut map, format!("{API_VERSION}/sth/latest.json"), latest)?;
    }

    // /v1/entries.json and /v1/entries/<index>.json
    insert_json(&mut map, format!("{API_VERSION}/entries.json"), &bundle.entries)?;
    for (i, entry) in bundle.entries.iter().enumerate() {
        insert_json(&mut map, format!("{API_VERSION}/entries/{i}.json"), entry)?;
    }

    // /v1/proof/inclusion/<tree_size>/<leaf_index>.json
    let mut inclusion_refs: Vec<InclusionRef> = Vec::with_capacity(bundle.inclusion.len());
    for check in &bundle.inclusion {
        let ts = check.proof.tree_size;
        let li = check.proof.leaf_index;
        insert_json(
            &mut map,
            format!("{API_VERSION}/proof/inclusion/{ts}/{li}.json"),
            &check.proof,
        )?;
        inclusion_refs.push(InclusionRef {
            tree_size: ts,
            leaf_index: li,
        });
    }

    // /v1/proof/consistency/<first>-<second>.json
    let mut consistency_refs: Vec<ConsistencyRef> = Vec::with_capacity(bundle.consistency.len());
    for check in &bundle.consistency {
        let f = check.proof.first_size;
        let s = check.proof.second_size;
        insert_json(
            &mut map,
            format!("{API_VERSION}/proof/consistency/{f}-{s}.json"),
            &check.proof,
        )?;
        consistency_refs.push(ConsistencyRef {
            first: f,
            second: s,
        });
    }

    // /v1/index.json — the discovery manifest. Built last so it only ever
    // advertises artifacts already present in the map.
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
    insert_json(&mut map, format!("{API_VERSION}/index.json"), &manifest)?;

    Ok((manifest, map))
}

/// Write the full immutable artifact tree for `bundle` under `root`, returning
/// the [`Manifest`] that was published at `/v1/index.json`.
///
/// `root` is created if missing. Existing files with the same names are
/// overwritten — generating from the same bundle is idempotent, and since every
/// artifact is content-addressed by its `(size, index)` coordinates, a larger
/// bundle only ever *adds* files, it never rewrites an existing immutable one.
///
/// The bytes written are exactly [`generate_artifacts`]'s output, so the static
/// host and the live server serve byte-identical artifacts.
pub fn generate(bundle: &Bundle, root: &Path) -> Result<Manifest> {
    let (manifest, artifacts) = generate_artifacts(bundle)?;
    for (rel, bytes) in &artifacts {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
    }
    Ok(manifest)
}

/// Read and parse a bundle JSON file from disk.
pub fn load_bundle(path: &Path) -> Result<Bundle> {
    let raw = std::fs::read_to_string(path)?;
    let bundle: Bundle =
        serde_json::from_str(&raw).map_err(|e| ServeError::BadBundle(e.to_string()))?;
    Ok(bundle)
}

/// Serialize `value` as pretty JSON and insert it into the artifact map at `rel`.
fn insert_json<T: Serialize>(
    map: &mut BTreeMap<String, Vec<u8>>,
    rel: String,
    value: &T,
) -> Result<()> {
    let json = serde_json::to_vec_pretty(value)?;
    map.insert(rel, json);
    Ok(())
}
