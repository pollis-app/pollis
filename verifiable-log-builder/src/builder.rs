//! Turns read [`CommitRow`]s (and [`AccountKeyRow`]s) into a signed monitor
//! bundle.
//! Each tenant has its own tree and bundle; they share only [`seal`].

use ml_dsa::Keypair;
use verifiable_log::SigningKey;
use verifiable_log::{Entry, Sth, VerifiableLog};

use crate::account_key::{self, AccountKeyInvariant};
use crate::binaries::{self, BinaryInvariant, BinaryRecord};
use crate::commit_log::{CommitLogInvariant, TENANT};
use crate::error::Result;
use crate::source::{AccountKeyRow, CommitRow};

/// The monitor bundle this crate emits. Defined once, in [`verifiable_log`], and
/// re-exported so the builder and the `monitor` CLI cannot drift.
pub use verifiable_log::bundle::{Bundle, ConsistencyCheck, InclusionCheck};

/// A key inside its rotation-overlap window. Named `RetiredKey` here because
/// that is what `--retired-key` produces.
pub use verifiable_log::bundle::PublicKeyEntry as RetiredKey;

/// Build a signed bundle from commit rows (assumed already in `seq` order).
///
/// Leaves carry windowed pseudonyms (#701). A full-strength gate on real ids runs
/// first and catches window-straddling forks; then the public tree is built.
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

/// Build a signed commit-log bundle from already-published leaves — the
/// persisted log of record (#1579), replayed through the public [`CommitLogInvariant`].
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

/// [`build_bundle_from_leaves`] for the **second** commit-log tree (#1579): the
/// same leaves, signed under [`verifiable_log::STH_CONTEXT_COMMIT_LOG_2`].
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
/// Its own tree with [`AccountKeyInvariant`], signed under [`account_key::STH_CONTEXT`].
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
/// Its own tree with [`BinaryInvariant`], signed under [`binaries::STH_CONTEXT`].
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
/// `sth_context`: `None` is the frozen commit-log context, `Some(ctx)` signs under `ctx`.
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

    // A final head, plus a midpoint head when there are at least 2 entries.
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
        // Set by the caller from --retired-key; publishing policy, not signing.
        retired_keys: Vec::new(),
        sths,
        entries,
        enforce_unique: enforce_unique.to_vec(),
        inclusion,
        consistency,
    })
}
