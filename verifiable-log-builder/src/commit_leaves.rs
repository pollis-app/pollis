//! The commit log's persisted leaves: its log of record, append-only, because
//! the DS prunes `mls_commit_log` and a rebuild from it would rewrite signed
//! positions (#1579, `docs/transparency.md`). The file holds only the published
//! pseudonymous leaves (#701), so keeping it longer than the DS discloses nothing
//! new. [`append_rows`] fails closed on any leaf that is missing, shrunk,
//! rewritten, or disagrees with a live row below the head.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use verifiable_log::{Entry, Sth, VerifiableLog};

use crate::commit_log::{
    derive_conversation_pseudonym, window_for_seq, CommitLeaf, CommitLogInvariant, TENANT,
};
use crate::error::{BuilderError, Result};
use crate::source::CommitRow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendOutcome {
    /// Every leaf of the log, persisted ones first.
    pub leaves: Vec<Entry>,
    pub appended: usize,
}

/// `Ok(None)` only when the file does not exist, so an unreadable file is never
/// mistaken for "nothing persisted yet".
pub fn load(path: &Path) -> Result<Option<Vec<Entry>>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn refuse(msg: impl Into<String>) -> BuilderError {
    BuilderError::PersistedLeaves(msg.into())
}

fn decode_all(entries: &[Entry]) -> Result<Vec<CommitLeaf>> {
    let mut out: Vec<CommitLeaf> = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        if entry.tenant != TENANT {
            return Err(refuse(format!(
                "persisted leaf {i} has tenant `{}`, expected `{TENANT}`",
                entry.tenant
            )));
        }
        let leaf = CommitLeaf::decode(&entry.data)
            .map_err(|e| refuse(format!("persisted leaf {i} is not a commit-log leaf: {e}")))?;
        if let Some(prev) = out.last() {
            if leaf.seq <= prev.seq {
                return Err(refuse(format!(
                    "persisted leaf {i} has seq {} after seq {}: leaves must be in strictly increasing seq order",
                    leaf.seq, prev.seq
                )));
            }
        }
        out.push(leaf);
    }
    Ok(out)
}

/// Check that `entries` reproduce `published`: at least its size, and the same
/// root over its first `tree_size` leaves.
pub fn check_reproduces(entries: &[Entry], published: &Sth) -> Result<()> {
    let size = published.tree_size as usize;
    if entries.len() < size {
        return Err(refuse(format!(
            "persisted leaves hold {} entries but the published head covers {size}: the file shrank, refusing to publish over the old tree",
            entries.len()
        )));
    }
    let mut log = VerifiableLog::new();
    for entry in &entries[..size] {
        log.append(entry.clone())?;
    }
    let root = log.root();
    let published_root = published.root_bytes()?;
    if root != published_root {
        return Err(refuse(format!(
            "persisted leaves do not reproduce the published head at size {size} (root {} vs published {}): refusing to publish a tree inconsistent with it",
            hex::encode(root),
            published.root_hash
        )));
    }
    Ok(())
}

/// Append the live rows above the last persisted `seq` to `persisted`.
///
/// `published` is the latest head already served for this tree, if any; `live`
/// must be in ascending `seq` order.
pub fn append_rows(
    persisted: &[Entry],
    live: &[CommitRow],
    published: Option<&Sth>,
) -> Result<AppendOutcome> {
    let persisted_leaves = decode_all(persisted)?;
    if let Some(sth) = published {
        check_reproduces(persisted, sth)?;
    }

    for pair in live.windows(2) {
        if pair[1].seq <= pair[0].seq {
            return Err(refuse(format!(
                "live rows are not in strictly increasing seq order (seq {} after {})",
                pair[1].seq, pair[0].seq
            )));
        }
    }

    let last_seq = persisted_leaves.last().map(|l| l.seq);
    let by_seq: HashMap<i64, usize> = persisted_leaves
        .iter()
        .enumerate()
        .map(|(i, l)| (l.seq, i))
        .collect();

    // Rows at or below the head must already be logged, unchanged.
    let mut new_rows: Vec<&CommitRow> = Vec::new();
    for row in live {
        match last_seq {
            Some(last) if row.seq <= last => {
                let Some(&i) = by_seq.get(&row.seq) else {
                    return Err(refuse(format!(
                        "live row seq {} is below the last persisted seq {last} but was never logged: refusing to skip it",
                        row.seq
                    )));
                };
                if row.to_leaf().to_entry()? != persisted[i] {
                    return Err(refuse(format!(
                        "live row seq {} no longer matches the leaf published for it (persisted leaf {i}): the source row changed",
                        row.seq
                    )));
                }
            }
            _ => new_rows.push(row),
        }
    }

    // Full-strength gate on real ids: re-key a persisted leaf to its real
    // conversation when a live one derives its pseudonym, so new commits are
    // checked against history whose rows were already pruned.
    let mut real_by_pseudonym: HashMap<(String, u64), String> = HashMap::new();
    let windows: BTreeSet<u64> = persisted_leaves
        .iter()
        .map(|l| window_for_seq(l.seq))
        .collect();
    let mut conversations: Vec<&str> = live.iter().map(|r| r.conversation_id.as_str()).collect();
    conversations.sort_unstable();
    conversations.dedup();
    for conv in conversations {
        for &window in &windows {
            real_by_pseudonym.insert(
                (derive_conversation_pseudonym(conv, window), window),
                conv.to_string(),
            );
        }
    }
    let mut identity_log = VerifiableLog::new();
    identity_log.register_invariant(TENANT, Box::new(CommitLogInvariant));
    for leaf in &persisted_leaves {
        let mut keyed = leaf.clone();
        let key = (leaf.conversation_pseudonym.clone(), window_for_seq(leaf.seq));
        if let Some(real) = real_by_pseudonym.get(&key) {
            keyed.conversation_pseudonym = real.clone();
        }
        identity_log.append(keyed.to_entry()?)?;
    }
    for row in &new_rows {
        identity_log.append(row.to_identity_leaf().to_entry()?)?;
    }

    // The published pseudonymous stream, exactly as a replayer checks it.
    let mut leaves: Vec<Entry> = persisted.to_vec();
    for row in &new_rows {
        leaves.push(row.to_leaf().to_entry()?);
    }
    replay_public(&leaves)?;

    Ok(AppendOutcome {
        appended: new_rows.len(),
        leaves,
    })
}

/// Replay `leaves` through the public [`CommitLogInvariant`], as an outside
/// auditor would.
pub fn replay_public(leaves: &[Entry]) -> Result<VerifiableLog> {
    let mut log = VerifiableLog::new();
    log.register_invariant(TENANT, Box::new(CommitLogInvariant));
    for entry in leaves {
        log.append(entry.clone())?;
    }
    Ok(log)
}
