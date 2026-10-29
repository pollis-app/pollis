//! Per-release binary-transparency verification: the binaries analogue of
//! [`crate::account`], shared by every caller so verdicts cannot diverge.
//!
//! The STH is verified under the domain-separated [`binaries::STH_CONTEXT`], so
//! no other tree's head can stand in for it. The [`BinaryInvariant`] is partly
//! global (tag order spans releases; a signed leaf's payload may sit anywhere
//! earlier), so the whole tree is replayed, not just the requested tag.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use verifiable_log::{proof, Entry, InclusionProof, Sth, VerifiableLog};
use verifiable_log_builder::binaries::{self, BinaryInvariant, BinaryRecord};

use crate::bundle::{now_ms, short, Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::remote::{build_agent, fetch_json};

/// Tenant id of binary entries in the shared log.
pub const BINARIES_TENANT: &str = binaries::TENANT;

// Re-exported as part of [`ReleaseArtifact`]'s public shape.
pub use binaries::Layer;
pub use binaries::Toolchain;

/// One released artifact in a tag's set: [`BinaryRecord`]'s key fields plus
/// whether its inclusion proof verified against the signed head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseArtifact {
    /// `darwin` | `windows` | `linux`.
    pub platform: String,
    /// `aarch64` | `x86_64`.
    pub arch: String,
    /// `dmg` | `app` | `nsis` | `appimage` | `deb` | `rpm`.
    pub bundle: String,
    /// The reproducible payload vs the signed wrapper.
    pub layer: Layer,
    pub artifact_name: String,
    /// Hash of the reproducible pre-signature payload, lowercase hex.
    pub payload_sha256: String,
    /// Hash of the shipped artifact, lowercase hex.
    pub artifact_sha256: String,
    pub provenance_uri: String,
    /// The recorded build recipe, verbatim from the leaf: a rebuild on a
    /// different runner image legitimately diverges (the AppImage vendors host
    /// libraries), so a rebuilder needs it to classify a divergence (#944).
    /// Pre-#939 tags carry `"unknown"`, which must never be treated as a match.
    pub toolchain: Toolchain,
    pub included: bool,
}

/// The result of verifying one release tag's binaries, as the CLI prints it and
/// the static `/verify/release/<tag>` report carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseReport {
    pub release_tag: String,
    pub found: bool,
    /// Tree size of the binaries STH everything was checked against.
    pub sth_tree_size: u64,
    pub root_hex: String,
    /// The tag's artifacts, in tree (publish) order.
    pub artifacts: Vec<ReleaseArtifact>,
    /// Binaries STH signature valid (under the binaries context) AND every
    /// artifact included AND the whole tree satisfies the binary invariant.
    pub chain_valid: bool,
    /// Why `chain_valid` is false (empty when true).
    pub violations: Vec<String>,
}

/// Verify a release tag's binaries against the binaries tree at `base_url`,
/// trusting only the pinned key. Fetches into a [`Bundle`] and defers to
/// [`verify_release_in_bundle`].
///
/// `Err` only for transport/parse failures of the prerequisites.
pub fn verify_release(base_url: &str, release_tag: &str) -> Result<ReleaseReport> {
    verify_release_via(base_url, release_tag, None)
}

/// [`verify_release`] with an optional SOCKS5 `proxy`; see
/// [`crate::group::verify_group_via`].
pub fn verify_release_via(
    base_url: &str,
    release_tag: &str,
    proxy: Option<&str>,
) -> Result<ReleaseReport> {
    let base = base_url.trim_end_matches('/');
    let agent = build_agent(proxy)?;

    let pk_doc: PublicKeyDoc =
        fetch_json(&agent, &format!("{base}/v1/binaries/public_key.json"))?;
    // H-1: pin at the fetch boundary (see `require_pinned`).
    let pk_doc = crate::pinned::require_pinned(&pk_doc, now_ms()).map_err(crate::error::ServeError::BadBundle)?;
    let sth: Sth = fetch_json(&agent, &format!("{base}/v1/binaries/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(&agent, &format!("{base}/v1/binaries/entries.json"))?;

    // Fetch only this tag's proofs.
    let tag_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == BINARIES_TENANT)
        .filter_map(|(i, e)| BinaryRecord::decode(&e.data).ok().map(|r| (i, r)))
        .filter(|(_, r)| r.release_tag == release_tag)
        .map(|(i, _)| i)
        .collect();

    let mut inclusion: Vec<InclusionCheck> = Vec::with_capacity(tag_indices.len());
    for i in &tag_indices {
        let url = format!("{base}/v1/binaries/proof/inclusion/{}/{}.json", sth.tree_size, i);
        if let Ok(proof) = fetch_json::<InclusionProof>(&agent, &url) {
            if let Some(entry) = entries.get(*i) {
                inclusion.push(InclusionCheck {
                    entry: entry.clone(),
                    proof,
                    sth_index: 0,
                });
            }
        }
    }

    let bundle = Bundle {
        // Carry the overlap set through, or an honest log mid-rotation reports a
        // genuine binary as unattested (#875).
        retired_keys: pk_doc.overlap_keys(),
        public_key: pk_doc.public_key,
        sths: vec![sth],
        entries,
        enforce_unique: vec![BINARIES_TENANT.to_string()],
        inclusion,
        consistency: Vec::new(),
    };

    Ok(verify_release_in_bundle(&bundle, release_tag))
}

/// The shared verdict core over an already-loaded binaries [`Bundle`] (no IO).
pub fn verify_release_in_bundle(bundle: &Bundle, release_tag: &str) -> ReleaseReport {
    verify_release_in_bundle_at(bundle, release_tag, now_ms())
}

/// [`verify_release_in_bundle`] against an explicit `now_ms`, which decides which
/// rotation-overlap keys have expired.
pub fn verify_release_in_bundle_at(
    bundle: &Bundle,
    release_tag: &str,
    now_ms: u64,
) -> ReleaseReport {
    let mut violations: Vec<String> = Vec::new();

    // 1. Trust anchor under the binaries context: another tree's STH fails here
    //    even though the same key signed it. Expiry is applied here.
    let candidates = bundle.key_candidates(now_ms);
    let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
    let (sth_tree_size, root_hex, sth_sig_ok) = match latest {
        Some(sth) => (
            sth.tree_size,
            sth.root_hash.clone(),
            sth.verify_any_with_context(&candidates, binaries::STH_CONTEXT)
                .is_some(),
        ),
        None => (0, String::new(), false),
    };
    if !sth_sig_ok {
        violations.push(
            "binaries STH signature is invalid — published head is not trustworthy".to_string(),
        );
    }

    // 2. Membership, with each entry's global leaf index (tree order is publish
    //    order).
    let selected: Vec<(usize, BinaryRecord)> = bundle
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == BINARIES_TENANT)
        .filter_map(|(i, e)| BinaryRecord::decode(&e.data).ok().map(|r| (i, r)))
        .filter(|(_, r)| r.release_tag == release_tag)
        .collect();

    let found = !selected.is_empty();

    // Only proofs against the newest head.
    let inclusion_by_index: BTreeMap<usize, &InclusionProof> = bundle
        .inclusion
        .iter()
        .filter(|c| latest.is_some_and(|s| c.proof.tree_size == s.tree_size))
        .map(|c| (c.proof.leaf_index as usize, &c.proof))
        .collect();

    // 3. Inclusion against the latest STH.
    let mut artifacts: Vec<ReleaseArtifact> = Vec::with_capacity(selected.len());
    let mut all_included = true;
    for (leaf_index, record) in &selected {
        let included = match (
            inclusion_by_index.get(leaf_index),
            latest,
            bundle.entries.get(*leaf_index),
        ) {
            (Some(proof), Some(sth), Some(entry)) => {
                proof::verify_inclusion_proof(entry, proof, sth)
            }
            _ => false,
        };
        if !included {
            all_included = false;
            violations.push(format!(
                "artifact `{}` ({:?}) is not provably included in the signed binaries tree",
                record.artifact_name, record.layer
            ));
        }
        artifacts.push(ReleaseArtifact {
            platform: record.platform.clone(),
            arch: record.arch.clone(),
            bundle: record.bundle.clone(),
            layer: record.layer,
            artifact_name: record.artifact_name.clone(),
            payload_sha256: record.payload_sha256.clone(),
            artifact_sha256: record.artifact_sha256.clone(),
            provenance_uri: record.provenance_uri.clone(),
            toolchain: record.toolchain.clone(),
            included,
        });
    }

    // 4. Invariant over the WHOLE tree (it is partly global).
    let mut log = VerifiableLog::new();
    log.register_invariant(BINARIES_TENANT, Box::new(BinaryInvariant));
    let mut invariant_ok = true;
    for entry in &bundle.entries {
        if entry.tenant != BINARIES_TENANT {
            continue;
        }
        if let Err(violation) = log.append(entry.clone()) {
            invariant_ok = false;
            violations.push(violation.to_string());
        }
    }

    let chain_valid = sth_sig_ok && all_included && invariant_ok;

    ReleaseReport {
        release_tag: release_tag.to_string(),
        found,
        sth_tree_size,
        root_hex,
        artifacts,
        chain_valid,
        violations,
    }
}

impl ReleaseReport {
    /// Print a human-readable report to stdout.
    pub fn print(&self) {
        println!("Release: {}", self.release_tag);
        println!("Found:   {}", if self.found { "yes" } else { "no" });
        println!("STH:     tree_size {}  root {}", self.sth_tree_size, self.root_hex);
        if self.artifacts.is_empty() {
            println!("Artifacts: (none)");
        } else {
            println!("Artifacts (publish order):");
            for a in &self.artifacts {
                println!(
                    "  {:<8} {:<8} {:<9} {:<8} payload {}  artifact {}  {}",
                    a.platform,
                    a.arch,
                    a.bundle,
                    layer_str(a.layer),
                    short(&a.payload_sha256),
                    short(&a.artifact_sha256),
                    if a.included { "[included \u{2713}]" } else { "[MISSING \u{2717}]" },
                );
            }
        }
        if !self.violations.is_empty() {
            println!("Violations:");
            for v in &self.violations {
                println!("  - {v}");
            }
        }
        println!(
            "\n{}",
            if self.chain_valid {
                "PASS: release binaries tree is valid"
            } else {
                "FAIL: release binaries tree is NOT valid"
            }
        );
    }
}

fn layer_str(layer: Layer) -> &'static str {
    match layer {
        Layer::Payload => "payload",
        Layer::Signed => "signed",
        Layer::Exe => "exe",
    }
}
