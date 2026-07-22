//! Per-release binary-transparency verification — the one shared function the
//! static report generator, the auditor CLI (`pollis-verify release <tag>`), and
//! a future live endpoint all call, so their verdicts can never diverge. This is
//! the binaries tenant's analogue of [`crate::account`].
//!
//! Given the published binaries tree (served under `/v1/binaries/...`) and a
//! release tag, it isolates that tag's artifacts and decides whether the
//! published tree is trustworthy for it:
//!
//! 1. **Trust anchor.** Verify the latest binaries STH's signature *first* —
//!    crucially under the binaries tree's domain-separated
//!    [`binaries::STH_CONTEXT`], so a commit-log or account-key head can never
//!    stand in for a binaries head even though the same key signs all three.
//! 2. **Inclusion.** Each of the tag's entries must have an inclusion proof that
//!    verifies against that binaries STH (reusing slice 1's
//!    [`verifiable_log::proof::verify_inclusion_proof`]).
//! 3. **Invariant.** Replay the **whole tree** through the [`BinaryInvariant`] —
//!    no fork, monotonic release tags, payload/signed pairing. Unlike the
//!    per-user account replay, the binary invariant is partly *global* (tag order
//!    spans releases; a signed leaf's payload may sit anywhere earlier), so the
//!    honest re-check replays every entry, then reports the requested tag's
//!    artifacts.
//!
//! Every cryptographic check is reused from slices 1–2; nothing here
//! reimplements Merkle, proof, signature, or invariant logic. Transport/parse
//! failures for the prerequisites return `Err`; a tampered/forked tree is **not**
//! an error — it yields a [`ReleaseReport`] with `chain_valid == false` and
//! populated `violations`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use verifiable_log::{proof, verifying_key_from_hex, Entry, InclusionProof, Sth, VerifiableLog};
use verifiable_log_builder::binaries::{self, BinaryInvariant, BinaryRecord};

use crate::bundle::{Bundle, InclusionCheck, PublicKeyDoc};
use crate::error::Result;
use crate::remote::{build_agent, fetch_json};

/// Tenant id the binary entries carry in the shared log. Re-exported from the
/// builder so layout/remote can filter on it without reaching across crates.
pub const BINARIES_TENANT: &str = binaries::TENANT;

/// Re-exported because it is part of [`ReleaseArtifact`]'s public shape — a
/// consumer (e.g. the app's in-app build check, which selects leaves by layer)
/// must be able to name it without depending on the builder crate.
pub use binaries::Layer;

/// One released artifact in a tag's set, as reported to a caller. Mirrors the
/// key structural fields of a [`BinaryRecord`] plus whether its inclusion proof
/// checked out against the signed head.
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
    /// The shipped file name.
    pub artifact_name: String,
    /// Hash of the reproducible pre-signature payload, lowercase hex.
    pub payload_sha256: String,
    /// Hash of the shipped artifact, lowercase hex.
    pub artifact_sha256: String,
    /// URI of this artifact's provenance attestation.
    pub provenance_uri: String,
    /// Did this entry's inclusion proof verify against the latest binaries STH?
    pub included: bool,
}

/// The structured result of verifying a single release tag's binaries. This is
/// the exact shape the CLI prints and the static `/verify/release/<tag>` report
/// carries — same function, same output, so they can never disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseReport {
    /// The release tag that was verified (echoed back).
    pub release_tag: String,
    /// Were any artifacts found for this tag?
    pub found: bool,
    /// Tree size of the binaries STH everything was checked against.
    pub sth_tree_size: u64,
    /// Root hash of that STH, lowercase hex.
    pub root_hex: String,
    /// The tag's artifacts, in tree (publish) order.
    pub artifacts: Vec<ReleaseArtifact>,
    /// Overall verdict: binaries STH signature valid (under the binaries context)
    /// AND every selected artifact included AND the whole tree satisfies the
    /// binary invariant.
    pub chain_valid: bool,
    /// Human-readable reasons `chain_valid` is false (empty when it is true).
    pub violations: Vec<String>,
}

/// Verify a single release tag's binaries against the binaries tree served at
/// `base_url` (e.g. `http://127.0.0.1:8787`), trusting only the published key.
///
/// A thin transport wrapper around [`verify_release_in_bundle`]: it fetches the
/// binaries tree's prerequisites (`binaries/public_key.json`,
/// `binaries/sth/latest.json`, `binaries/entries.json`) and this tag's inclusion
/// proofs into an in-memory [`Bundle`], then hands them to the shared core — the
/// one place the verdict is computed.
///
/// Returns `Err` only for transport/parse failures of the prerequisites; any
/// *verification* failure is folded into the report as `chain_valid = false`.
pub fn verify_release(base_url: &str, release_tag: &str) -> Result<ReleaseReport> {
    let base = base_url.trim_end_matches('/');
    let agent = build_agent();

    // Prerequisites, all under the binaries subtree.
    let pk_doc: PublicKeyDoc =
        fetch_json(&agent, &format!("{base}/v1/binaries/public_key.json"))?;
    let sth: Sth = fetch_json(&agent, &format!("{base}/v1/binaries/sth/latest.json"))?;
    let entries: Vec<Entry> = fetch_json(&agent, &format!("{base}/v1/binaries/entries.json"))?;

    // Plan which proofs to fetch: only this tag's entries (decode + match).
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
        public_key: pk_doc.public_key,
        sths: vec![sth],
        entries,
        enforce_unique: vec![BINARIES_TENANT.to_string()],
        inclusion,
        consistency: Vec::new(),
    };

    Ok(verify_release_in_bundle(&bundle, release_tag))
}

/// Verify a single release tag's binaries against an **already-loaded** binaries
/// [`Bundle`] — no IO. This is the shared verdict core: both the URL-based
/// [`verify_release`] and the static report generator call it, so a tag's verdict
/// is identical no matter how the bundle was obtained.
///
/// Never panics; a tampered/forked tree yields `chain_valid == false` with
/// populated `violations` rather than an error.
pub fn verify_release_in_bundle(bundle: &Bundle, release_tag: &str) -> ReleaseReport {
    let mut violations: Vec<String> = Vec::new();

    // 1. Trust anchor: the newest binaries head, verified under the log key AND
    //    the binaries domain context. An STH minted for the commit-log or
    //    account-key tree fails here even though the same key signed it.
    let verifying_key = verifying_key_from_hex(&bundle.public_key).ok();
    let latest = bundle.sths.iter().max_by_key(|s| s.tree_size);
    let (sth_tree_size, root_hex, sth_sig_ok) = match (latest, &verifying_key) {
        (Some(sth), Some(vk)) => (
            sth.tree_size,
            sth.root_hash.clone(),
            sth.verify_with_context(vk, binaries::STH_CONTEXT),
        ),
        (Some(sth), None) => (sth.tree_size, sth.root_hash.clone(), false),
        (None, _) => (0, String::new(), false),
    };
    if !sth_sig_ok {
        violations.push(
            "binaries STH signature is invalid — published head is not trustworthy".to_string(),
        );
    }

    // 2. Membership: every entry that decodes as a binary leaf for this tag,
    //    paired with its global leaf index (used to locate its proof). Tree order
    //    is publish order, so no explicit sort is needed.
    let selected: Vec<(usize, BinaryRecord)> = bundle
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.tenant == BINARIES_TENANT)
        .filter_map(|(i, e)| BinaryRecord::decode(&e.data).ok().map(|r| (i, r)))
        .filter(|(_, r)| r.release_tag == release_tag)
        .collect();

    let found = !selected.is_empty();

    // Index the bundle's inclusion proofs by leaf index, keeping only those
    // checked against the newest head.
    let inclusion_by_index: BTreeMap<usize, &InclusionProof> = bundle
        .inclusion
        .iter()
        .filter(|c| latest.is_some_and(|s| c.proof.tree_size == s.tree_size))
        .map(|c| (c.proof.leaf_index as usize, &c.proof))
        .collect();

    // 3. Inclusion: each selected entry must be committed by the latest STH.
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
            included,
        });
    }

    // 4. Invariant: replay the WHOLE tree through the binary rules (no fork,
    //    monotonic release tags, payload/signed pairing). The invariant is partly
    //    global — tag order spans releases and a signed leaf's payload can sit
    //    anywhere earlier — so unlike the per-user account replay it must see
    //    every entry. Reuse slice 2's invariant verbatim; a rejected append is
    //    exactly a detected violation.
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
    /// Print a human-readable report to stdout (used by the CLI's text mode).
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

/// Render a [`Layer`] for human output.
fn layer_str(layer: Layer) -> &'static str {
    match layer {
        Layer::Payload => "payload",
        Layer::Signed => "signed",
        Layer::Exe => "exe",
    }
}

/// Abbreviate a long opaque id/hash for human-readable output.
fn short(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}\u{2026}{}", &s[..6], &s[s.len() - 4..])
    }
}
