//! Turns source rows into signed monitor bundles, one per tenant, each with its
//! own tree and its own STH context so a head of one can never pass as another's.

use ml_dsa::Keypair;
use verifiable_log::SigningKey;
use verifiable_log::{Entry, Sth, VerifiableLog};

use crate::account_key::{self, AccountKeyInvariant};
use crate::binaries::{self, BinaryInvariant, BinaryRecord};
use crate::commit_log::{CommitLogInvariant, TENANT};
use crate::error::Result;
use crate::source::{AccountKeyRow, CommitRow};

/// Re-exported, never redeclared: a second copy of the shape is where a field
/// silently goes missing between builder and `monitor`.
pub use verifiable_log::bundle::{Bundle, ConsistencyCheck, InclusionCheck};

/// A key inside its rotation-overlap window, as `--retired-key` supplies it.
pub use verifiable_log::bundle::PublicKeyEntry as RetiredKey;

/// Build a signed bundle from commit rows (assumed already in `seq` order).
///
/// Published leaves carry windowed pseudonyms (#701). The invariant is first
/// replayed on the real ids, the only place they are in hand, which catches a
/// fork that straddles a pseudonym window and the public check cannot see.
pub fn build_bundle(
    rows: &[CommitRow],
    signing_key: &SigningKey,
    timestamp: u64,
) -> Result<Bundle> {
    let mut source_log = VerifiableLog::new();
    source_log.register_invariant(TENANT, Box::new(CommitLogInvariant));
    for row in rows {
        source_log.append(row.to_identity_leaf().to_entry()?)?;
    }

    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        entries.push(row.to_leaf().to_entry()?);
    }
    build_bundle_from_leaves(&entries, signing_key, timestamp)
}

/// Build a commit-log bundle from the persisted leaves (#1579). Only the public
/// replay runs here; the real-id gate runs in [`crate::commit_leaves::append_rows`].
pub fn build_bundle_from_leaves(
    leaves: &[Entry],
    signing_key: &SigningKey,
    timestamp: u64,
) -> Result<Bundle> {
    let log = crate::commit_leaves::replay_public(leaves)?;
    seal(
        log,
        leaves.to_vec(),
        &[TENANT.to_string()],
        signing_key,
        timestamp,
        None,
    )
}

/// [`build_bundle_from_leaves`] signed under
/// [`verifiable_log::STH_CONTEXT_COMMIT_LOG_2`], so no head can verify as one of
/// the retired first tree or vice versa (#1579).
pub fn build_commit_log_2_bundle(
    leaves: &[Entry],
    signing_key: &SigningKey,
    timestamp: u64,
) -> Result<Bundle> {
    let log = crate::commit_leaves::replay_public(leaves)?;
    seal(
        log,
        leaves.to_vec(),
        &[TENANT.to_string()],
        signing_key,
        timestamp,
        Some(verifiable_log::STH_CONTEXT_COMMIT_LOG_2),
    )
}

/// Build a signed bundle from account-key rows (assumed already in `seq` order).
/// A duplicate or regressing `identity_version` aborts the build.
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
/// Any [`BinaryInvariant`] violation aborts the build.
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

/// Emit a final STH, a midpoint STH plus consistency proof when there are at
/// least 2 entries, and an inclusion proof per entry. `None` context is the
/// frozen default commit-log context.
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
        // Filled by the caller from --retired-key: publishing policy.
        retired_keys: Vec::new(),
        sths,
        entries,
        enforce_unique: enforce_unique.to_vec(),
        inclusion,
        consistency,
    })
}
