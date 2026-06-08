//! Turns read [`CommitRow`]s into a signed monitor bundle.
//!
//! The output [`Bundle`] serialises to **exactly** the JSON schema that
//! `verifiable-log`'s `monitor` CLI consumes (`public_key`, `sths`, `entries`,
//! `enforce_unique`, `inclusion`, `consistency`) — see `verifiable-log/README.md`
//! and `verifiable-log/fixtures/example.json`. No changes to the monitor are
//! needed to verify what this produces.

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use verifiable_log::{
    ConsistencyProof, Entry, InclusionProof, Sth, VerifiableLog,
};

use crate::commit_log::{CommitLogInvariant, TENANT};
use crate::error::Result;
use crate::source::CommitRow;

/// Top-level monitor bundle. Field names and shapes match the frozen wire
/// contract in `verifiable-log/README.md`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Bundle {
    /// Ed25519 log public key, lowercase hex (32 bytes).
    pub public_key: String,
    /// Signed Tree Heads, oldest first.
    pub sths: Vec<Sth>,
    /// Full ordered log contents.
    pub entries: Vec<Entry>,
    /// Tenants the monitor's uniqueness invariant is enforced for on replay.
    pub enforce_unique: Vec<String>,
    /// Inclusion proofs (one per entry).
    pub inclusion: Vec<InclusionCheck>,
    /// Consistency proofs between successive STHs.
    pub consistency: Vec<ConsistencyCheck>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InclusionCheck {
    pub entry: Entry,
    pub proof: InclusionProof,
    /// Index into `sths` whose root the proof is checked against.
    pub sth_index: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConsistencyCheck {
    pub old_index: usize,
    pub new_index: usize,
    pub proof: ConsistencyProof,
}

/// Build a signed bundle from commit rows (assumed already in `seq` order).
///
/// Every commit is appended to a [`VerifiableLog`] with [`CommitLogInvariant`]
/// registered for the [`TENANT`], so a fork or epoch regression in the source
/// aborts the build with an [`crate::error::BuilderError::Invariant`] rather than
/// producing a bundle that hides it.
///
/// Emits, over the full log:
/// * an STH over the final tree (and, when there are ≥2 entries, an earlier STH
///   at the midpoint so the bundle also carries a consistency proof proving the
///   log only appended between the two heads);
/// * an inclusion proof for every entry against the final STH;
/// * a consistency proof between the midpoint and final STHs (when ≥2 entries).
///
/// `timestamp` (ms since epoch) is supplied by the caller — never read from the
/// clock — so the output is deterministic. Both STHs carry this single
/// timestamp; in a real deployment each head would carry its own signing time.
pub fn build_bundle(
    rows: &[CommitRow],
    signing_key: &SigningKey,
    timestamp: u64,
) -> Result<Bundle> {
    let mut log = VerifiableLog::new();
    log.register_invariant(TENANT, Box::new(CommitLogInvariant));

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let entry = row.to_leaf().to_entry()?;
        log.append(entry.clone())?;
        entries.push(entry);
    }

    let n = log.size();

    // STHs: a final head over the whole tree, plus a midpoint head when the
    // tree is big enough to carry a meaningful consistency proof.
    let mut sths: Vec<Sth> = Vec::new();
    let mut midpoint: Option<usize> = None;
    if n >= 2 {
        let m = n / 2;
        midpoint = Some(m);
        let mid_root = log.root_at(m)?;
        sths.push(Sth::create(signing_key, m as u64, mid_root, timestamp));
    }
    sths.push(log.signed_tree_head(signing_key, timestamp));
    let final_index = sths.len() - 1;

    // Inclusion proof for every entry, checked against the final STH.
    let mut inclusion = Vec::with_capacity(n);
    for i in 0..n {
        inclusion.push(InclusionCheck {
            entry: entries[i].clone(),
            proof: log.inclusion_proof(i)?,
            sth_index: final_index,
        });
    }

    // Consistency between the midpoint and final heads (append-only proof).
    let mut consistency = Vec::new();
    if let Some(m) = midpoint {
        consistency.push(ConsistencyCheck {
            old_index: 0,
            new_index: final_index,
            proof: log.consistency_proof(m, n)?,
        });
    }

    let public_key = hex::encode(signing_key.verifying_key().to_bytes());

    Ok(Bundle {
        public_key,
        sths,
        entries,
        enforce_unique: vec![TENANT.to_string()],
        inclusion,
        consistency,
    })
}
