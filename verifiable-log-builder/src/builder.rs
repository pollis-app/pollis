//! Turns read [`CommitRow`]s (and [`AccountKeyRow`]s) into a signed monitor
//! bundle.
//!
//! The output [`Bundle`] serialises to **exactly** the JSON schema that
//! `verifiable-log`'s `monitor` CLI consumes (`public_key`, `sths`, `entries`,
//! `enforce_unique`, `inclusion`, `consistency`) — see `verifiable-log/README.md`
//! and `verifiable-log/fixtures/example.json`. No changes to the monitor are
//! needed to verify what this produces.
//!
//! There are three tenants, each with its **own** tree and **own** bundle:
//! [`build_bundle`] over the MLS commit log (default STH context),
//! [`build_account_bundle`] over the account-key directory (the domain-separated
//! [`account_key::STH_CONTEXT`]), and [`build_binaries_bundle`] over the
//! released-binaries tree (the domain-separated [`binaries::STH_CONTEXT`]). They
//! share the STH/proof assembly ([`seal`]) but never share a tree — see the
//! tenant modules for why.

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use verifiable_log::{
    ConsistencyProof, Entry, InclusionProof, Sth, VerifiableLog,
};

use crate::account_key::{self, AccountKeyInvariant};
use crate::binaries::{self, BinaryInvariant, BinaryRecord};
use crate::commit_log::{CommitLogInvariant, TENANT};
use crate::error::Result;
use crate::source::{AccountKeyRow, CommitRow};

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

    // Default (commit-log) STH context — byte-identical to the frozen contract.
    seal(log, entries, &[TENANT.to_string()], signing_key, timestamp, None)
}

/// Build a signed bundle from account-key rows (assumed already in `seq` order).
///
/// The account-key directory is a **separate** tenant with its **own** tree, so
/// this builds an independent [`VerifiableLog`] with [`AccountKeyInvariant`]
/// registered for [`account_key::TENANT`] — a duplicate or regressing
/// `identity_version` aborts the build rather than producing a bundle that hides
/// it. Crucially, its STHs are signed under the domain-separated
/// [`account_key::STH_CONTEXT`] (not the commit-log context), so an account-key
/// head can never be presented as a commit-log head even though the same key
/// signs both.
pub fn build_account_bundle(
    rows: &[AccountKeyRow],
    signing_key: &SigningKey,
    timestamp: u64,
) -> Result<Bundle> {
    let mut log = VerifiableLog::new();
    log.register_invariant(account_key::TENANT, Box::new(AccountKeyInvariant));

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let entry = row.to_leaf().to_entry()?;
        log.append(entry.clone())?;
        entries.push(entry);
    }

    seal(
        log,
        entries,
        &[account_key::TENANT.to_string()],
        signing_key,
        timestamp,
        Some(account_key::STH_CONTEXT),
    )
}

/// Build a signed bundle from binary records (assumed already in publish order).
///
/// The released-binaries tree is a **third** independent tenant with its **own**
/// tree, so this builds an independent [`VerifiableLog`] with [`BinaryInvariant`]
/// registered for [`binaries::TENANT`] — a fork (same released unit, different
/// `artifact_sha256`), a tag reappearing out of publish order, or a `signed`
/// leaf with no matching `payload` aborts the build rather than producing a
/// bundle that hides it. Its STHs are signed under the domain-separated
/// [`binaries::STH_CONTEXT`], so a binaries head can never be presented as a
/// commit-log or account-key head even though the same key signs all three.
pub fn build_binaries_bundle(
    records: &[BinaryRecord],
    signing_key: &SigningKey,
    timestamp: u64,
) -> Result<Bundle> {
    let mut log = VerifiableLog::new();
    log.register_invariant(binaries::TENANT, Box::new(BinaryInvariant));

    let mut entries = Vec::with_capacity(records.len());
    for record in records {
        let entry = record.to_entry()?;
        log.append(entry.clone())?;
        entries.push(entry);
    }

    seal(
        log,
        entries,
        &[binaries::TENANT.to_string()],
        signing_key,
        timestamp,
        Some(binaries::STH_CONTEXT),
    )
}

/// Shared bundle assembly over an already-populated [`VerifiableLog`]: emit a
/// final STH (plus a midpoint STH + consistency proof when ≥2 entries), and an
/// inclusion proof for every entry against the final STH.
///
/// `sth_context` selects the STH domain separation: `None` is the default
/// commit-log context (frozen, byte-identical to the original path); `Some(ctx)`
/// signs under `ctx` for a second tenant's tree. The two tenants differ in
/// *which* context, nothing else — so the proof/STH shape stays identical.
fn seal(
    log: VerifiableLog,
    entries: Vec<Entry>,
    enforce_unique: &[String],
    signing_key: &SigningKey,
    timestamp: u64,
    sth_context: Option<&[u8]>,
) -> Result<Bundle> {
    let n = log.size();

    let make_sth = |size: u64, root: verifiable_log::Hash| match sth_context {
        Some(ctx) => Sth::create_with_context(signing_key, size, root, timestamp, ctx),
        None => Sth::create(signing_key, size, root, timestamp),
    };

    // STHs: a final head over the whole tree, plus a midpoint head when the
    // tree is big enough to carry a meaningful consistency proof.
    let mut sths: Vec<Sth> = Vec::new();
    let mut midpoint: Option<usize> = None;
    if n >= 2 {
        let m = n / 2;
        midpoint = Some(m);
        let mid_root = log.root_at(m)?;
        sths.push(make_sth(m as u64, mid_root));
    }
    sths.push(make_sth(n as u64, log.root()));
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
        enforce_unique: enforce_unique.to_vec(),
        inclusion,
        consistency,
    })
}
