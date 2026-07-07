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
//! | `/verify/group/<conversation_id>`                     | [`GroupReport`] (precomputed)     | short     |
//!
//! ### The account-key tree (`/v1/account-keys/...`)
//!
//! The account-key directory is a **fully separate** Merkle tree from the commit
//! log — its own entries, its own STHs (signed under the domain-separated
//! [`account_key::STH_CONTEXT`]), its own manifest. So it gets its own subtree,
//! mirroring the commit-log layout one level down under `account-keys/`; the
//! commit-log `/v1/...` paths above are never touched. The two trees only share
//! the signing key and the `/verify/...` namespace.
//!
//! | URL                                                              | Contents                          | Cache     |
//! |------------------------------------------------------------------|-----------------------------------|-----------|
//! | `/v1/account-keys/public_key.json`                               | [`PublicKeyDoc`]                  | immutable |
//! | `/v1/account-keys/index.json`                                    | [`AccountManifest`] (discovery)   | short     |
//! | `/v1/account-keys/sth/latest.json`                               | newest account [`Sth`]            | short     |
//! | `/v1/account-keys/sth/<tree_size>.json`                          | account [`Sth`] at that size      | immutable |
//! | `/v1/account-keys/entries.json`                                  | full ordered `[Entry]`            | immutable |
//! | `/v1/account-keys/entries/<index>.json`                          | one [`Entry`]                     | immutable |
//! | `/v1/account-keys/proof/inclusion/<tree_size>/<leaf_index>.json` | [`InclusionProof`]                | immutable |
//! | `/v1/account-keys/proof/consistency/<first>-<second>.json`       | [`ConsistencyProof`]              | immutable |
//! | `/verify/account/<user_id>`                                      | [`AccountReport`] (precomputed)   | short     |
//!
//! ### The binaries tree (`/v1/binaries/...`)
//!
//! A **third** fully separate Merkle tree (binary transparency, #453) — its own
//! entries, its own STHs signed under the domain-separated
//! [`binaries::STH_CONTEXT`], its own manifest. It mirrors the account-key subtree
//! one level down under `binaries/`, and the commit-log / account-key paths are
//! never touched. The three trees only share the signing key and the `/verify/...`
//! namespace.
//!
//! | URL                                                          | Contents                          | Cache     |
//! |--------------------------------------------------------------|-----------------------------------|-----------|
//! | `/v1/binaries/public_key.json`                               | [`PublicKeyDoc`]                  | immutable |
//! | `/v1/binaries/index.json`                                    | [`BinaryManifest`] (discovery)    | short     |
//! | `/v1/binaries/sth/latest.json`                               | newest binaries [`Sth`]           | short     |
//! | `/v1/binaries/sth/<tree_size>.json`                          | binaries [`Sth`] at that size     | immutable |
//! | `/v1/binaries/entries.json`                                  | full ordered `[Entry]`            | immutable |
//! | `/v1/binaries/entries/<index>.json`                          | one [`Entry`]                     | immutable |
//! | `/v1/binaries/proof/inclusion/<tree_size>/<leaf_index>.json` | [`InclusionProof`]                | immutable |
//! | `/v1/binaries/proof/consistency/<first>-<second>.json`       | [`ConsistencyProof`]              | immutable |
//! | `/verify/release/<tag>`                                      | [`ReleaseReport`] (precomputed)   | short     |
//!
//! [`binaries::STH_CONTEXT`]: verifiable_log_builder::binaries::STH_CONTEXT
//! [`ReleaseReport`]: crate::release::ReleaseReport
//!
//! The file path under the output root mirrors the URL exactly (drop the
//! leading `/`), so serving is a literal static-file mapping.
//!
//! ## Precomputed per-group reports (`/verify/group/<id>`)
//!
//! In addition to the immutable `/v1` surface, the generator emits a precomputed
//! per-conversation report at `verify/group/<conversation_id>` (no extension —
//! the file *is* the endpoint URL) for every conversation present in the bundle's
//! commit-log entries. The bytes are **byte-identical** to what the live
//! `GET /verify/group/<id>` endpoint returns, because both serialize the exact
//! same [`GroupReport`] from the shared [`verify_group_in_bundle`] as compact
//! JSON — the static host therefore serves the same verdict the live server
//! would, with no server on the path. These reports move as the log grows (a new
//! head changes every group's `sth_tree_size`/inclusion), so they are
//! short-cached like `index.json` and `sth/latest.json`. The account-key tree's
//! `/verify/account/<user_id>` reports are emitted the same way (see
//! [`generate_account`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use verifiable_log_builder::account_key::AccountKeyLeaf;
use verifiable_log_builder::binaries::BinaryRecord;
use verifiable_log_builder::{CommitLeaf, TENANT};

use crate::account::{verify_account_in_bundle, ACCOUNT_TENANT};
use crate::bundle::{
    AccountManifest, BinaryManifest, Bundle, ConsistencyRef, InclusionRef, Manifest, PublicKeyDoc,
};
use crate::error::{Result, ServeError};
use crate::group::verify_group_in_bundle;
use crate::release::{verify_release_in_bundle, BINARIES_TENANT};

/// API version segment all artifacts live under.
pub const API_VERSION: &str = "v1";

/// Path prefix the account-key tree's artifacts live under — the commit-log
/// `/v1` surface one level down, in its own subtree.
pub const ACCOUNT_API_PREFIX: &str = "v1/account-keys";

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

    // /verify/group/<conversation_id> — a precomputed per-conversation report
    // for every distinct conversation in the bundle's commit-log entries. The
    // bytes are compact JSON of the shared [`verify_group_in_bundle`] verdict,
    // so they are byte-identical to the live `GET /verify/group/<id>` endpoint's
    // response (which serializes the same `GroupReport` the same way). The file
    // has no extension — its path is the endpoint URL verbatim.
    let conversations: Vec<String> = bundle
        .entries
        .iter()
        .filter(|e| e.tenant == TENANT)
        .filter_map(|e| CommitLeaf::decode(&e.data).ok())
        .map(|leaf| leaf.conversation_id)
        // A report is one path segment under `verify/group/`; reject anything
        // that could escape it. Real MLS conversation ids are ULID-shaped and
        // always pass — this only guards against malformed source data.
        .filter(|id| is_safe_segment(id))
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    for conv in &conversations {
        let report = verify_group_in_bundle(bundle, conv);
        // Compact (not pretty) to match the live endpoint's `to_string`.
        let bytes = serde_json::to_vec(&report)?;
        map.insert(format!("verify/group/{conv}"), bytes);
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
        conversations,
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

/// The complete read API for the **account-key** tree as an in-memory map of
/// **relative path → JSON bytes** (e.g. `"v1/account-keys/sth/latest.json"`),
/// exactly the content the static subtree would write to disk.
///
/// This mirrors [`generate_artifacts`] one level down under
/// [`ACCOUNT_API_PREFIX`]: the same immutable STH / entries / proofs surface,
/// plus a precomputed `/verify/account/<user_id>` report for every user in the
/// bundle. The artifact *bytes* are tenant-agnostic (an STH already carries its
/// own signature, signed under the account context by the builder); only the
/// path prefix, the per-user reports, and the manifest shape differ.
pub fn generate_account_artifacts(
    bundle: &Bundle,
) -> Result<(AccountManifest, BTreeMap<String, Vec<u8>>)> {
    let mut map: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let prefix = ACCOUNT_API_PREFIX;

    // /v1/account-keys/public_key.json
    insert_json(
        &mut map,
        format!("{prefix}/public_key.json"),
        &PublicKeyDoc {
            public_key: bundle.public_key.clone(),
        },
    )?;

    // /v1/account-keys/sth/<size>.json for every head, plus latest.json.
    let mut sth_sizes: Vec<u64> = Vec::with_capacity(bundle.sths.len());
    for sth in &bundle.sths {
        insert_json(&mut map, format!("{prefix}/sth/{}.json", sth.tree_size), sth)?;
        sth_sizes.push(sth.tree_size);
    }
    sth_sizes.sort_unstable();
    sth_sizes.dedup();

    let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
    if let Some(latest) = latest {
        insert_json(&mut map, format!("{prefix}/sth/latest.json"), latest)?;
    }

    // /v1/account-keys/entries.json and /v1/account-keys/entries/<index>.json
    insert_json(&mut map, format!("{prefix}/entries.json"), &bundle.entries)?;
    for (i, entry) in bundle.entries.iter().enumerate() {
        insert_json(&mut map, format!("{prefix}/entries/{i}.json"), entry)?;
    }

    // /v1/account-keys/proof/inclusion/<tree_size>/<leaf_index>.json
    let mut inclusion_refs: Vec<InclusionRef> = Vec::with_capacity(bundle.inclusion.len());
    for check in &bundle.inclusion {
        let ts = check.proof.tree_size;
        let li = check.proof.leaf_index;
        insert_json(
            &mut map,
            format!("{prefix}/proof/inclusion/{ts}/{li}.json"),
            &check.proof,
        )?;
        inclusion_refs.push(InclusionRef {
            tree_size: ts,
            leaf_index: li,
        });
    }

    // /v1/account-keys/proof/consistency/<first>-<second>.json
    let mut consistency_refs: Vec<ConsistencyRef> = Vec::with_capacity(bundle.consistency.len());
    for check in &bundle.consistency {
        let f = check.proof.first_size;
        let s = check.proof.second_size;
        insert_json(
            &mut map,
            format!("{prefix}/proof/consistency/{f}-{s}.json"),
            &check.proof,
        )?;
        consistency_refs.push(ConsistencyRef {
            first: f,
            second: s,
        });
    }

    // /verify/account/<user_id> — a precomputed report for every distinct user
    // in the bundle's account-key entries. The bytes are compact JSON of the
    // shared [`verify_account_in_bundle`] verdict, so a future live
    // `GET /verify/account/<id>` endpoint serving the same core returns
    // byte-identical responses. The file has no extension — its path is the
    // endpoint URL verbatim.
    let users: Vec<String> = bundle
        .entries
        .iter()
        .filter(|e| e.tenant == ACCOUNT_TENANT)
        .filter_map(|e| AccountKeyLeaf::decode(&e.data).ok())
        .map(|leaf| leaf.user_id)
        // A report is one path segment under `verify/account/`; reject anything
        // that could escape it. Real user ids are ULID-shaped and always pass —
        // this only guards against malformed source data.
        .filter(|id| is_safe_segment(id))
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    for user in &users {
        let report = verify_account_in_bundle(bundle, user);
        // Compact (not pretty) to match a live endpoint's `to_string`.
        let bytes = serde_json::to_vec(&report)?;
        map.insert(format!("verify/account/{user}"), bytes);
    }

    // /v1/account-keys/index.json — the discovery manifest, built last so it
    // only ever advertises artifacts already present in the map.
    let manifest = AccountManifest {
        version: API_VERSION.to_string(),
        public_key: bundle.public_key.clone(),
        entry_count: bundle.entries.len() as u64,
        latest_tree_size: latest.map(|s| s.tree_size),
        sth_sizes,
        inclusion: inclusion_refs,
        consistency: consistency_refs,
        enforce_unique: bundle.enforce_unique.clone(),
        users,
    };
    insert_json(&mut map, format!("{prefix}/index.json"), &manifest)?;

    Ok((manifest, map))
}

/// Write the account-key tree's static subtree for `account_bundle` under
/// `root`, returning the [`AccountManifest`] published at
/// `/v1/account-keys/index.json`.
///
/// `root` is the *same* output root as [`generate`] — the account subtree lives
/// alongside the commit-log tree under it, never overlapping any commit-log
/// path. Existing files are overwritten; like the commit-log tree every
/// per-size artifact is content-addressed, so a larger bundle only ever adds
/// files.
pub fn generate_account(account_bundle: &Bundle, root: &Path) -> Result<AccountManifest> {
    let (manifest, artifacts) = generate_account_artifacts(account_bundle)?;
    for (rel, bytes) in &artifacts {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
    }
    Ok(manifest)
}

/// Path prefix the binaries tree's artifacts live under — a third sibling of the
/// commit-log `/v1` surface and the account-key subtree, one level down in its
/// own subtree (`v1/binaries/...`), never overlapping either.
pub const BINARIES_API_PREFIX: &str = "v1/binaries";

/// The complete read API for the **binaries** tree (binary transparency) as an
/// in-memory map of **relative path → JSON bytes** (e.g.
/// `"v1/binaries/sth/latest.json"`), exactly the content the static subtree would
/// write to disk.
///
/// This mirrors [`generate_account_artifacts`] one level down under
/// [`BINARIES_API_PREFIX`]: the same immutable STH / entries / proofs surface,
/// plus a precomputed `/verify/release/<tag>` report for every release tag in the
/// bundle. Like the account tree the artifact *bytes* are tenant-agnostic (each
/// STH already carries its own signature, signed under the binaries context by
/// the builder); only the path prefix, the per-release reports, and the manifest
/// shape differ.
pub fn generate_binaries_artifacts(
    bundle: &Bundle,
) -> Result<(BinaryManifest, BTreeMap<String, Vec<u8>>)> {
    let mut map: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let prefix = BINARIES_API_PREFIX;

    // /v1/binaries/public_key.json
    insert_json(
        &mut map,
        format!("{prefix}/public_key.json"),
        &PublicKeyDoc {
            public_key: bundle.public_key.clone(),
        },
    )?;

    // /v1/binaries/sth/<size>.json for every head, plus latest.json.
    let mut sth_sizes: Vec<u64> = Vec::with_capacity(bundle.sths.len());
    for sth in &bundle.sths {
        insert_json(&mut map, format!("{prefix}/sth/{}.json", sth.tree_size), sth)?;
        sth_sizes.push(sth.tree_size);
    }
    sth_sizes.sort_unstable();
    sth_sizes.dedup();

    let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
    if let Some(latest) = latest {
        insert_json(&mut map, format!("{prefix}/sth/latest.json"), latest)?;
    }

    // /v1/binaries/entries.json and /v1/binaries/entries/<index>.json
    insert_json(&mut map, format!("{prefix}/entries.json"), &bundle.entries)?;
    for (i, entry) in bundle.entries.iter().enumerate() {
        insert_json(&mut map, format!("{prefix}/entries/{i}.json"), entry)?;
    }

    // /v1/binaries/proof/inclusion/<tree_size>/<leaf_index>.json
    let mut inclusion_refs: Vec<InclusionRef> = Vec::with_capacity(bundle.inclusion.len());
    for check in &bundle.inclusion {
        let ts = check.proof.tree_size;
        let li = check.proof.leaf_index;
        insert_json(
            &mut map,
            format!("{prefix}/proof/inclusion/{ts}/{li}.json"),
            &check.proof,
        )?;
        inclusion_refs.push(InclusionRef {
            tree_size: ts,
            leaf_index: li,
        });
    }

    // /v1/binaries/proof/consistency/<first>-<second>.json
    let mut consistency_refs: Vec<ConsistencyRef> = Vec::with_capacity(bundle.consistency.len());
    for check in &bundle.consistency {
        let f = check.proof.first_size;
        let s = check.proof.second_size;
        insert_json(
            &mut map,
            format!("{prefix}/proof/consistency/{f}-{s}.json"),
            &check.proof,
        )?;
        consistency_refs.push(ConsistencyRef {
            first: f,
            second: s,
        });
    }

    // /verify/release/<tag> — a precomputed report for every distinct release tag
    // in the bundle's binary entries. The bytes are compact JSON of the shared
    // [`verify_release_in_bundle`] verdict, so the CLI (`pollis-verify release`)
    // and a future live endpoint serving the same core return byte-identical
    // responses. The file has no extension — its path is the endpoint URL verbatim.
    let tags: Vec<String> = bundle
        .entries
        .iter()
        .filter(|e| e.tenant == BINARIES_TENANT)
        .filter_map(|e| BinaryRecord::decode(&e.data).ok())
        .map(|r| r.release_tag)
        // A report is one path segment under `verify/release/`; reject anything
        // that could escape it. Real tags are `vX.Y.Z`-shaped and always pass —
        // this only guards against malformed source data.
        .filter(|t| is_safe_segment(t))
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    for tag in &tags {
        let report = verify_release_in_bundle(bundle, tag);
        // Compact (not pretty) to match a live endpoint's `to_string`.
        let bytes = serde_json::to_vec(&report)?;
        map.insert(format!("verify/release/{tag}"), bytes);
    }

    // /v1/binaries/index.json — the discovery manifest, built last so it only
    // ever advertises artifacts already present in the map.
    let manifest = BinaryManifest {
        version: API_VERSION.to_string(),
        public_key: bundle.public_key.clone(),
        entry_count: bundle.entries.len() as u64,
        latest_tree_size: latest.map(|s| s.tree_size),
        sth_sizes,
        inclusion: inclusion_refs,
        consistency: consistency_refs,
        enforce_unique: bundle.enforce_unique.clone(),
        tags,
    };
    insert_json(&mut map, format!("{prefix}/index.json"), &manifest)?;

    Ok((manifest, map))
}

/// Write the binaries tree's static subtree for `binaries_bundle` under `root`,
/// returning the [`BinaryManifest`] published at `/v1/binaries/index.json`.
///
/// `root` is the *same* output root as [`generate`] — the binaries subtree lives
/// alongside the commit-log and account-key trees under it, never overlapping any
/// of their paths. Existing files are overwritten; like the other trees every
/// per-size artifact is content-addressed, so a larger bundle only ever adds
/// files.
pub fn generate_binaries(binaries_bundle: &Bundle, root: &Path) -> Result<BinaryManifest> {
    let (manifest, artifacts) = generate_binaries_artifacts(binaries_bundle)?;
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

/// Is `id` safe to use as the single `verify/group/<id>` path segment? Rejects
/// empty ids, `.`/`..`, and anything containing a path separator so a malformed
/// conversation id can never write outside the output directory.
fn is_safe_segment(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains('/') && !id.contains('\\')
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
