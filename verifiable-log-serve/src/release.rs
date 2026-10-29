//! Per-release binary-transparency verification: the one function the static
//! report generator and the auditor CLI (`pollis-verify release <tag>`) share.
//! The binaries analogue of [`crate::account`].
//!
//! The STH is verified under the domain-separated [`binaries::STH_CONTEXT`], so
//! a head of another tree can never stand in even though one key signs all
//! three. Transport/parse failures are `Err`; a tampered tree is a
//! [`ReleaseReport`] with `chain_valid == false`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use verifiable_log::{proof, Entry, InclusionProof, Sth, VerifiableLog};
use verifiable_log_builder::binaries::{self, BinaryInvariant, BinaryRecord};

use crate::bundle::{now_ms, short, Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::remote::{build_agent, fetch_json};

/// Tenant id of binary entries in the shared log.
pub const BINARIES_TENANT: &str = binaries::TENANT;

/// Re-exported as part of [`ReleaseArtifact`]'s shape, so consumers needn't
/// depend on the builder crate.
pub use binaries::Layer;

/// Re-exported for the same reason as [`Layer`].
pub use binaries::Toolchain;

/// One released artifact in a tag's set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseArtifact {
    /// `darwin` | `windows` | `linux`.
    pub platform: String,
    /// `aarch64` | `x86_64`.
    pub arch: String,
    /// `dmg` | `app` | `nsis` | `appimage` | `deb` | `rpm`.
    pub bundle: String,
    pub layer: Layer,
    pub artifact_name: String,
    /// Hash of the reproducible pre-signature payload, lowercase hex.
    pub payload_sha256: String,
    /// Hash of the shipped artifact, lowercase hex.
    pub artifact_sha256: String,
    pub provenance_uri: String,
    /// The build recipe recorded in the leaf (#944): without it a rebuilder
    /// can't tell a runner-image difference (e.g. AppImage-vendored host libs)
    /// from a real reproducibility failure. Pre-#939 tags carry `"unknown"`,
    /// which a consumer must treat as unknown, never as a match.
    pub toolchain: Toolchain,
    /// Inclusion proof verified against the latest binaries STH.
    pub included: bool,
}

/// Result of verifying one release tag; the shape the CLI prints and the static
/// `/verify/release/<tag>` report carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseReport {
    pub release_tag: String,
    pub found: bool,
    /// Tree size of the binaries STH everything was checked against.
    pub sth_tree_size: u64,
    pub root_hex: String,
    /// The tag's artifacts, in tree (publish) order.
    pub artifacts: Vec<ReleaseArtifact>,
    /// STH signature valid AND every artifact included AND the whole tree
    /// satisfies the binary invariant.
    pub chain_valid: bool,
    /// Why `chain_valid` is false (empty when true).
    pub violations: Vec<String>,
}

/// Verify a release tag's binaries against the binaries tree at `base_url`,
/// trusting only the pinned key. `Err` only for transport/parse failures;
/// verification failures are `chain_valid = false`.
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
    let agent = build_agent(proxy)?;
    // Root-authorised signers are trusted alongside the compiled pin (#754, #1454).
    let trust = crate::pinned::key_set_trust(&agent, base_url, binaries::STH_CONTEXT);
    verify_release_with(&agent, base_url, release_tag, &trust)
}

/// [`verify_release_via`] with a caller-supplied key-set decision; see
/// [`crate::group::verify_group_via_trusting`].
pub fn verify_release_via_trusting(
    base_url: &str,
    release_tag: &str,
    proxy: Option<&str>,
    trust: &crate::pinned::KeySetTrust,
) -> Result<ReleaseReport> {
    let agent = build_agent(proxy)?;
    verify_release_with(&agent, base_url, release_tag, trust)
}

fn verify_release_with(
    agent: &ureq::Agent,
    base_url: &str,
    release_tag: &str,
    trust: &crate::pinned::KeySetTrust,
) -> Result<ReleaseReport> {
    let base = base_url.trim_end_matches('/');

    let pk_doc: PublicKeyDoc =
        fetch_json(agent, &format!("{base}/v1/binaries/public_key.json"))?;
    // H-1: pin at the fetch boundary; see `group::verify_group_with`.
    let pk_doc = crate::pinned::require_pinned_with(&pk_doc, now_ms(), trust)
        .map_err(crate::error::ServeError::BadBundle)?;
    let sth: Sth = fetch_json(agent, &format!("{base}/v1/binaries/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(agent, &format!("{base}/v1/binaries/entries.json"))?;

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
        if let Ok(proof) = fetch_json::<InclusionProof>(agent, &url) {
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
        // Keep the rotation-overlap set, or mid-rotation fails (#875).
        retired_keys: pk_doc.overlap_keys(),
        public_key: pk_doc.public_key,
        sths: vec![sth],
        entries,
        enforce_unique: vec![BINARIES_TENANT.to_string()],
        inclusion,
        consistency: Vec::new(),
    };

    let mut report = verify_release_in_bundle(&bundle, release_tag);
    // An unattributable served key set is an alarm even under the compiled pin.
    if let Some(v) = trust.alarm_violation() {
        report.chain_valid = false;
        report.violations.insert(0, v);
    }
    Ok(report)
}

/// Verify a release tag against an already-loaded binaries [`Bundle`], no IO:
/// the shared verdict core. Never panics.
pub fn verify_release_in_bundle(bundle: &Bundle, release_tag: &str) -> ReleaseReport {
    verify_release_in_bundle_at(bundle, release_tag, now_ms())
}

/// [`verify_release_in_bundle`] against an explicit `now_ms`, which decides
/// which rotation-overlap keys have expired.
pub fn verify_release_in_bundle_at(
    bundle: &Bundle,
    release_tag: &str,
    now_ms: u64,
) -> ReleaseReport {
    let mut violations: Vec<String> = Vec::new();

    // Expiry is applied here, not trusted from the server.
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

    // Tree order is publish order, so no sort is needed.
    let selected: Vec<(usize, BinaryRecord)> = bundle
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == BINARIES_TENANT)
        .filter_map(|(i, e)| BinaryRecord::decode(&e.data).ok().map(|r| (i, r)))
        .filter(|(_, r)| r.release_tag == release_tag)
        .collect();

    let found = !selected.is_empty();

    let inclusion_by_index: BTreeMap<usize, &InclusionProof> = bundle
        .inclusion
        .iter()
        .filter(|c| latest.is_some_and(|s| c.proof.tree_size == s.tree_size))
        .map(|c| (c.proof.leaf_index as usize, &c.proof))
        .collect();

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

    // The binary invariant is partly global (tag order spans releases; a signed
    // leaf's payload may sit anywhere earlier), so replay the whole tree.
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
