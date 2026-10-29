//! Minting the commit log's **reset statement** (#1579).
//!
//! The first commit-log tree (`/v1/...`, STH context
//! [`verifiable_log::STH_CONTEXT_COMMIT_LOG`]) is retired: after the DS began
//! pruning `mls_commit_log`, the publisher signed heads that do not extend its own
//! earlier history. Its successor, `commit-log-2`, is built from the persisted
//! leaves ([`crate::commit_leaves`]) and signed under
//! [`verifiable_log::STH_CONTEXT_COMMIT_LOG_2`].
//!
//! The facts about the retired tree (its last consistent head, the leaf hashes
//! that head commits to, and every other served head with its root) were
//! recovered from what is served and are committed to the repository as
//! `commit-log-retirement.json`, so they are reviewed like code. The CI publish
//! mints the signed statement from that file, because it is the only place the
//! signing key exists:
//!
//! 1. [`check_against_served`] refuses unless every head the file lists is
//!    actually served with that root, and every chain or fork head verifies under
//!    the signing key in the retired context. A statement that disagreed with what
//!    is served would be worse than none.
//! 2. [`build_body`] adds the successor's identity and its genesis (the hash of
//!    its first persisted leaf).
//! 3. [`verifiable_log::ResetStatement::create`] validates the whole body and
//!    signs it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use verifiable_log::{
    Entry, HeadClass, ResetBody, RetiredTree, Sth, SuccessorTree, VerifyingKey,
    RESET_FORMAT_VERSION, STH_CONTEXT_COMMIT_LOG, STH_CONTEXT_COMMIT_LOG_2,
};

use crate::error::{BuilderError, Result};

/// The successor tree's name, as in [`verifiable_log::STH_CONTEXTS`].
pub const SUCCESSOR_TREE: &str = "commit-log-2";
/// The successor tree's URL path. Matches `verifiable_log_serve::layout`'s
/// `COMMIT_LOG_2_API_PREFIX`; a test there holds the two together.
pub const SUCCESSOR_PATH: &str = "v1/commit-log-2";

/// The committed description of the retired tree (`commit-log-retirement.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementFacts {
    pub reason: String,
    pub retired: RetiredTree,
}

fn refuse(msg: impl Into<String>) -> BuilderError {
    BuilderError::Reset(msg.into())
}

/// Check `facts` against the retired tree's served per-size heads (`served`,
/// keyed by tree size). `candidates` are the keys a retired head may be signed by.
pub fn check_against_served(
    facts: &RetirementFacts,
    served: &BTreeMap<u64, Sth>,
    candidates: &[(String, VerifyingKey)],
) -> Result<()> {
    let r = &facts.retired;
    if r.sth_context.as_bytes() != STH_CONTEXT_COMMIT_LOG || r.tree != "commit-log" || r.path != "v1"
    {
        return Err(refuse(
            "the retirement file must describe the first commit-log tree at /v1",
        ));
    }
    let body = provisional_body(facts);
    body.validate()?;
    let ctx = STH_CONTEXT_COMMIT_LOG;
    let signed = |size: u64| -> Result<&Sth> {
        let sth = served
            .get(&size)
            .ok_or_else(|| refuse(format!("no served head at size {size} to check")))?;
        if sth.tree_size != size {
            return Err(refuse(format!("the head served at size {size} says size {}", sth.tree_size)));
        }
        Ok(sth)
    };
    let last = signed(r.last_consistent.tree_size)?;
    if last != &r.last_consistent {
        return Err(refuse("the last consistent head differs from the served one"));
    }
    for &size in &r.chain_sizes {
        let sth = signed(size)?;
        if body.classify(size, &sth.root_hash) != HeadClass::Chain {
            return Err(refuse(format!("the served head at size {size} is not on the chain")));
        }
        if sth.verify_any_with_context(candidates, ctx).is_none() {
            return Err(refuse(format!("the served chain head at size {size} does not verify")));
        }
    }
    for fork in &r.forks {
        let sth = signed(fork.tree_size)?;
        if sth.root_hash != fork.root_hash || sth.timestamp != fork.timestamp {
            return Err(refuse(format!("the served head at size {} is not the listed fork", fork.tree_size)));
        }
        if sth.verify_any_with_context(candidates, ctx).is_none() {
            return Err(refuse(format!("the listed fork at size {} does not verify", fork.tree_size)));
        }
    }
    for old in &r.superseded {
        let sth = signed(old.tree_size)?;
        if sth.root_hash != old.root_hash {
            return Err(refuse(format!(
                "the served head at size {} is not the listed superseded head",
                old.tree_size
            )));
        }
    }
    // Every served head must be described: a head the file leaves out would make
    // the statement incomplete on the day it is signed.
    for (size, sth) in served {
        if body.classify(*size, &sth.root_hash) == HeadClass::Unlisted {
            return Err(refuse(format!("the served head at size {size} is not listed")));
        }
    }
    Ok(())
}

fn provisional_body(facts: &RetirementFacts) -> ResetBody {
    ResetBody {
        format_version: RESET_FORMAT_VERSION,
        issued_at_ms: 0,
        reason: facts.reason.clone(),
        retired: facts.retired.clone(),
        successor: SuccessorTree {
            tree: SUCCESSOR_TREE.to_string(),
            sth_context: String::from_utf8_lossy(STH_CONTEXT_COMMIT_LOG_2).to_string(),
            path: SUCCESSOR_PATH.to_string(),
            genesis_root_hash: hex::encode([0u8; 32]),
        },
    }
}

/// The statement body: `facts` plus the successor tree, whose genesis is the hash
/// of the first of `successor_leaves` (the persisted commit-log-2 leaves).
pub fn build_body(
    facts: &RetirementFacts,
    successor_leaves: &[Entry],
    issued_at_ms: u64,
) -> Result<ResetBody> {
    let first = successor_leaves
        .first()
        .ok_or_else(|| refuse("the successor tree has no leaves yet, so it has no genesis to name"))?;
    let mut body = provisional_body(facts);
    body.issued_at_ms = issued_at_ms;
    body.successor.genesis_root_hash = hex::encode(first.leaf_hash());
    body.validate()?;
    Ok(body)
}
