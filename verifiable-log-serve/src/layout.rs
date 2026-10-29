//! Static layout generator: turn a signed [`Bundle`] into the immutable
//! directory tree that implements the log's public read API. Every artifact is
//! write-once; only `sth/latest.json` and `index.json` move. File paths mirror
//! URLs (drop the leading `/`).
//!
//! | URL                                                   | Contents                          | Cache     |
//! |-------------------------------------------------------|-----------------------------------|-----------|
//! | `/v1/public_key.json`                                 | [`PublicKeyDoc`](crate::bundle::PublicKeyDoc)                  | immutable |
//! | `/v1/index.json`                                      | [`Manifest`] (discovery)          | short     |
//! | `/v1/sth/latest.json`                                 | newest [`Sth`]                    | short     |
//! | `/v1/sth/<tree_size>.json`                            | [`Sth`] at that size              | immutable |
//! | `/v1/entries.json`                                    | full ordered `[Entry]`            | immutable |
//! | `/v1/entries/<index>.json`                            | one [`Entry`]                     | immutable |
//! | `/v1/proof/inclusion/<tree_size>/<leaf_index>.json`   | [`InclusionProof`]                | immutable |
//! | `/v1/proof/consistency/<first>-<second>.json`         | [`ConsistencyProof`]              | immutable |
//!
//! The account-key, binaries and second commit-log trees are separate Merkle
//! trees with their own domain-separated STHs, laid out identically under
//! [`ACCOUNT_API_PREFIX`], [`BINARIES_API_PREFIX`] and [`COMMIT_LOG_2_API_PREFIX`].
//! The account and binaries trees add precomputed `/verify/account/<user_id>` and
//! `/verify/release/<tag>` reports. The tree directly under `/v1` is retired and
//! frozen (#1579); [`RESET_STATEMENT_PATH`] ties it to its successor.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use verifiable_log_builder::account_key::AccountKeyLeaf;
use verifiable_log_builder::binaries::BinaryRecord;
use crate::account::{verify_account_in_bundle, ACCOUNT_TENANT};
use crate::bundle::{
    AccountManifest, BinaryManifest, Bundle, ConsistencyRef, InclusionRef, Manifest, FORMAT_VERSION,
};
use crate::error::{Result, ServeError};
use crate::release::{verify_release_in_bundle, BINARIES_TENANT};

/// API version segment all artifacts live under.
pub const API_VERSION: &str = "v1";

/// Path prefix of the account-key tree.
pub const ACCOUNT_API_PREFIX: &str = "v1/account-keys";

/// Emit the root-signed key-set statement at `/v1/key-set.json` (#754),
/// refusing one that does not verify under the root pin: an unverifiable key
/// set makes every client trust no signer, taking the whole log down.
pub fn insert_key_set(
    map: &mut BTreeMap<String, Vec<u8>>,
    statement: &verifiable_log::KeySetStatement,
    root_public_key_hex: &str,
) -> Result<()> {
    let root = verifiable_log::root_key_from_hex(root_public_key_hex)
        .map_err(|e| crate::error::ServeError::BadBundle(format!("root key: {e}")))?;
    statement
        .verify(&root)
        .map_err(|e| crate::error::ServeError::BadBundle(format!("refusing to publish an unverifiable key set: {e}")))?;
    insert_json(map, format!("{API_VERSION}/key-set.json"), statement)
}

/// Write an artifact map out file-for-file under `root`.
fn write_artifacts(root: &Path, artifacts: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    for (rel, bytes) in artifacts {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
    }
    Ok(())
}

/// The complete `/v1` read API as **relative path → JSON bytes**. The single
/// source of truth: [`generate`] writes it to disk and the live server serves it
/// from memory, keyed by the same path a static host uses, so they can't diverge.
pub fn generate_artifacts(bundle: &Bundle) -> Result<(Manifest, BTreeMap<String, Vec<u8>>)> {
    commit_tree_artifacts(bundle, API_VERSION)
}

/// Path prefix of the second commit-log tree (#1579), so no URL of the retired
/// tree is reused. The segment equals the tree's STH context suffix and its
/// `STH_CONTEXTS` entry.
pub const COMMIT_LOG_2_API_PREFIX: &str = "v1/commit-log-2";

/// Where the signed reset statement retiring the first commit-log tree is
/// served (#1579).
pub const RESET_STATEMENT_PATH: &str = "v1/commit-log-reset.json";

/// The read API for the second commit-log tree, under [`COMMIT_LOG_2_API_PREFIX`].
pub fn generate_commit_log_2_artifacts(
    bundle: &Bundle,
) -> Result<(Manifest, BTreeMap<String, Vec<u8>>)> {
    commit_tree_artifacts(bundle, COMMIT_LOG_2_API_PREFIX)
}

/// Write the second commit-log tree under `root`.
pub fn generate_commit_log_2(bundle: &Bundle, root: &Path) -> Result<Manifest> {
    let (manifest, artifacts) = generate_commit_log_2_artifacts(bundle)?;
    write_artifacts(root, &artifacts)?;
    Ok(manifest)
}

/// Emit a reset statement at [`RESET_STATEMENT_PATH`], refusing one that does not
/// verify under `candidates`.
pub fn insert_reset_statement(
    map: &mut BTreeMap<String, Vec<u8>>,
    statement: &verifiable_log::ResetStatement,
    candidates: &[(String, verifiable_log::VerifyingKey)],
) -> Result<()> {
    statement.verify_any(candidates).map_err(|e| {
        ServeError::BadBundle(format!("refusing to publish an unverifiable reset statement: {e}"))
    })?;
    insert_json(map, RESET_STATEMENT_PATH.to_string(), statement)
}

/// The commit-log artifact set under `prefix` (`v1` for the first tree,
/// [`COMMIT_LOG_2_API_PREFIX`] for its successor).
fn commit_tree_artifacts(
    bundle: &Bundle,
    prefix: &str,
) -> Result<(Manifest, BTreeMap<String, Vec<u8>>)> {
    let mut map: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    insert_json(
        &mut map,
        format!("{prefix}/public_key.json"),
        &bundle.public_key_doc(),
    )?;

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

    insert_json(&mut map, format!("{prefix}/entries.json"), &bundle.entries)?;
    for (i, entry) in bundle.entries.iter().enumerate() {
        insert_json(&mut map, format!("{prefix}/entries/{i}.json"), entry)?;
    }

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

    // No per-group reports or conversation list (#701): any static key would
    // enumerate conversations or conversation-windows. Per-group verification is
    // dynamic and member-gated, taking the real conversation id.

    // Built last so it only advertises artifacts already in the map.
    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        version: API_VERSION.to_string(),
        public_key: bundle.public_key.clone(),
        entry_count: bundle.entries.len() as u64,
        latest_tree_size: latest.map(|s| s.tree_size),
        sth_sizes,
        inclusion: inclusion_refs,
        consistency: consistency_refs,
        enforce_unique: bundle.enforce_unique.clone(),
    };
    insert_json(&mut map, format!("{prefix}/index.json"), &manifest)?;

    Ok((manifest, map))
}

/// Write the full artifact tree for `bundle` under `root` (created if missing).
/// Idempotent; a larger bundle only adds files, never rewriting an immutable one.
pub fn generate(bundle: &Bundle, root: &Path) -> Result<Manifest> {
    let (manifest, artifacts) = generate_artifacts(bundle)?;
    write_artifacts(root, &artifacts)?;
    Ok(manifest)
}

/// The account-key tree's read API under [`ACCOUNT_API_PREFIX`], plus a
/// precomputed `/verify/account/<user_id>` report per user.
pub fn generate_account_artifacts(
    bundle: &Bundle,
) -> Result<(AccountManifest, BTreeMap<String, Vec<u8>>)> {
    let mut map: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let prefix = ACCOUNT_API_PREFIX;

    insert_json(
        &mut map,
        format!("{prefix}/public_key.json"),
        &bundle.public_key_doc(),
    )?;

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

    insert_json(&mut map, format!("{prefix}/entries.json"), &bundle.entries)?;
    for (i, entry) in bundle.entries.iter().enumerate() {
        insert_json(&mut map, format!("{prefix}/entries/{i}.json"), entry)?;
    }

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

    // Compact JSON of the shared verdict, so a live endpoint serving the same
    // core would return identical bytes. The path is the endpoint URL verbatim.
    let users: Vec<String> = bundle
        .entries
        .iter()
        .filter(|e| e.tenant == ACCOUNT_TENANT)
        .filter_map(|e| AccountKeyLeaf::decode(&e.data).ok())
        .map(|leaf| leaf.user_id)
        // Guard against malformed ids escaping `verify/account/`.
        .filter(|id| is_safe_segment(id))
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    for user in &users {
        let report = verify_account_in_bundle(bundle, user);
        let bytes = serde_json::to_vec(&report)?;
        map.insert(format!("verify/account/{user}"), bytes);
    }

    // Built last so it only advertises artifacts already in the map.
    let manifest = AccountManifest {
        format_version: FORMAT_VERSION,
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

/// Write the account-key subtree under the same `root` as [`generate`].
pub fn generate_account(account_bundle: &Bundle, root: &Path) -> Result<AccountManifest> {
    let (manifest, artifacts) = generate_account_artifacts(account_bundle)?;
    write_artifacts(root, &artifacts)?;
    Ok(manifest)
}

/// Path prefix of the binaries tree.
pub const BINARIES_API_PREFIX: &str = "v1/binaries";

/// The binaries tree's read API under [`BINARIES_API_PREFIX`], plus a
/// precomputed `/verify/release/<tag>` report per release tag.
pub fn generate_binaries_artifacts(
    bundle: &Bundle,
) -> Result<(BinaryManifest, BTreeMap<String, Vec<u8>>)> {
    let mut map: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let prefix = BINARIES_API_PREFIX;

    insert_json(
        &mut map,
        format!("{prefix}/public_key.json"),
        &bundle.public_key_doc(),
    )?;

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

    insert_json(&mut map, format!("{prefix}/entries.json"), &bundle.entries)?;
    for (i, entry) in bundle.entries.iter().enumerate() {
        insert_json(&mut map, format!("{prefix}/entries/{i}.json"), entry)?;
    }

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

    // Compact JSON of the shared verdict, byte-identical to what the CLI and a
    // live endpoint compute. The path is the endpoint URL verbatim.
    let tags: Vec<String> = bundle
        .entries
        .iter()
        .filter(|e| e.tenant == BINARIES_TENANT)
        .filter_map(|e| BinaryRecord::decode(&e.data).ok())
        .map(|r| r.release_tag)
        // Guard against malformed tags escaping `verify/release/`.
        .filter(|t| is_safe_segment(t))
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    for tag in &tags {
        let report = verify_release_in_bundle(bundle, tag);
        let bytes = serde_json::to_vec(&report)?;
        map.insert(format!("verify/release/{tag}"), bytes);
    }

    // Built last so it only advertises artifacts already in the map.
    let manifest = BinaryManifest {
        format_version: FORMAT_VERSION,
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

/// Write the binaries subtree under the same `root` as [`generate`].
pub fn generate_binaries(binaries_bundle: &Bundle, root: &Path) -> Result<BinaryManifest> {
    let (manifest, artifacts) = generate_binaries_artifacts(binaries_bundle)?;
    write_artifacts(root, &artifacts)?;
    Ok(manifest)
}

pub fn load_bundle(path: &Path) -> Result<Bundle> {
    let raw = std::fs::read_to_string(path)?;
    let bundle: Bundle =
        serde_json::from_str(&raw).map_err(|e| ServeError::BadBundle(e.to_string()))?;
    Ok(bundle)
}

/// A single path segment that can't escape the output directory.
fn is_safe_segment(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains('/') && !id.contains('\\')
}

fn insert_json<T: Serialize>(
    map: &mut BTreeMap<String, Vec<u8>>,
    rel: String,
    value: &T,
) -> Result<()> {
    let json = serde_json::to_vec_pretty(value)?;
    map.insert(rel, json);
    Ok(())
}

#[cfg(test)]
mod key_set_artifact_tests {
    use super::*;
    use ml_dsa::{Keypair, MlDsa44, SigningKey};
    use verifiable_log::{KeySetStatement, SignerEntry};

    fn root() -> SigningKey<MlDsa44> {
        SigningKey::<MlDsa44>::from_seed(&[42u8; 32].into())
    }

    fn signer() -> SignerEntry {
        let sk = SigningKey::<MlDsa44>::from_seed(&[7u8; 32].into());
        let vk = sk.verifying_key();
        SignerEntry {
            key_id: verifiable_log::sth::key_id_for(&vk),
            public_key: hex::encode(vk.encode()),
            trees: vec!["pollis-verifiable-log:sth:v2".into()],
            not_after_ms: None,
        }
    }

    #[test]
    fn a_verifying_statement_is_published_at_the_expected_path() {
        let r = root();
        let st = KeySetStatement::create(&r, 1, None, vec![signer()]).expect("create");
        let mut map = BTreeMap::new();
        insert_key_set(&mut map, &st, &hex::encode(r.verifying_key().encode())).expect("insert");
        assert!(map.contains_key(&format!("{API_VERSION}/key-set.json")));
    }

    #[test]
    fn a_statement_the_root_did_not_sign_is_refused() {
        let other = SigningKey::<MlDsa44>::from_seed(&[9u8; 32].into());
        let st = KeySetStatement::create(&other, 1, None, vec![signer()]).expect("create");
        let mut map = BTreeMap::new();
        let err = insert_key_set(&mut map, &st, &hex::encode(root().verifying_key().encode()));
        assert!(err.is_err());
        assert!(map.is_empty(), "nothing may be written when verification fails");
    }

    #[test]
    fn the_successor_path_matches_the_one_the_reset_statement_names() {
        assert_eq!(
            COMMIT_LOG_2_API_PREFIX,
            verifiable_log_builder::commit_log_reset::SUCCESSOR_PATH
        );
        assert_eq!(
            COMMIT_LOG_2_API_PREFIX.rsplit('/').next(),
            Some(verifiable_log_builder::commit_log_reset::SUCCESSOR_TREE)
        );
    }

    #[test]
    fn a_malformed_root_pin_is_refused() {
        let r = root();
        let st = KeySetStatement::create(&r, 1, None, vec![signer()]).expect("create");
        let mut map = BTreeMap::new();
        assert!(insert_key_set(&mut map, &st, "not-a-key").is_err());
        assert!(map.is_empty());
    }
}
