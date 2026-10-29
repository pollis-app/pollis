//! Turns rows into signed monitor bundles: one tree and one STH context per
//! tenant, sharing only the STH/proof assembly ([`seal`]).

use ml_dsa::Keypair;
use verifiable_log::SigningKey;
use verifiable_log::{Entry, Sth, VerifiableLog};

use crate::account_key::{self, AccountKeyInvariant};
use crate::binaries::{self, BinaryInvariant, BinaryRecord};
use crate::commit_log::{CommitLogInvariant, TENANT};
use crate::error::Result;
use crate::source::{AccountKeyRow, CommitRow};

/// Defined once in [`verifiable_log`] so the builder and `monitor` can't drift.
pub use verifiable_log::bundle::{Bundle, ConsistencyCheck, InclusionCheck};

/// A key inside its rotation-overlap window (from `--retired-key`).
pub use verifiable_log::bundle::PublicKeyEntry as RetiredKey;

/// Build a signed bundle from commit rows (assumed already in `seq` order).
///
/// Published leaves carry windowed pseudonyms, not raw ids (#701). Two passes,
/// either of which aborts the build on a violation:
///
/// 1. [`CommitLogInvariant`] over leaves keyed on the real `conversation_id`:
///    the only point that catches a fork straddling a pseudonym-window boundary.
/// 2. The published pseudonymous tree, under the same invariant an outside
///    replayer runs.
///
/// `timestamp` is caller-supplied so the output is deterministic.
pub fn build_bundle(
    rows: &[CommitRow],
    signing_key: &SigningKey,
    timestamp: u64,
) -> Result<Bundle> {
    // (1) Source-integrity gate on the real ids (#701).
    let mut source_log = VerifiableLog::new();
    source_log.register_invariant(TENANT, Box::new(CommitLogInvariant));
    for row in rows {
        source_log.append(row.to_identity_leaf().to_entry()?)?;
    }

    // (2) The published, pseudonymous tree.
    let mut log = VerifiableLog::new();
    log.register_invariant(TENANT, Box::new(CommitLogInvariant));

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let entry = row.to_leaf().to_entry()?;
        log.append(entry.clone())?;
        entries.push(entry);
    }

    seal(log, entries, &[TENANT.to_string()], signing_key, timestamp, None)
}

/// Build a signed bundle from account-key rows (assumed already in `seq` order).
///
/// Its own tree under [`account_key::STH_CONTEXT`]; a duplicate or regressing
/// `identity_version` aborts the build.
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
/// Its own tree under [`binaries::STH_CONTEXT`]; any [`BinaryInvariant`]
/// violation aborts the build.
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

/// Emit a final STH (plus a midpoint STH and consistency proof when ≥2
/// entries) and an inclusion proof per entry. `sth_context: None` is the frozen
/// commit-log context.
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

    let mut inclusion = Vec::with_capacity(n);
    for (i, entry) in entries.iter().enumerate().take(n) {
        inclusion.push(InclusionCheck {
            entry: entry.clone(),
            proof: log.inclusion_proof(i)?,
            sth_index: final_index,
        });
    }

    let mut consistency = Vec::new();
    if let Some(m) = midpoint {
        consistency.push(ConsistencyCheck {
            old_index: 0,
            new_index: final_index,
            proof: log.consistency_proof(m, n)?,
        });
    }

    let public_key = hex::encode(signing_key.verifying_key().encode());

    Ok(Bundle {
        public_key,
        // Populated by the caller from --retired-key.
        retired_keys: Vec::new(),
        sths,
        entries,
        enforce_unique: enforce_unique.to_vec(),
        inclusion,
        consistency,
    })
}
