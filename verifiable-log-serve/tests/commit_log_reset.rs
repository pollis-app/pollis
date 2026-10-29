//! #1579: `pollis-verify remote` over a log whose first commit-log tree has been
//! retired by a signed reset statement and continued as `commit-log-2`.
//!
//! The fixture mirrors production: a retired `/v1/...` tree with a consistent
//! chain of heads, a forked head that its frozen `latest.json` and `index.json`
//! point at, a superseded head, and a successor tree built from persisted
//! leaves. A remote verify must PASS with the retired tree reported as RETIRED
//! and the fork as KNOWN, and must FAIL on any head the statement does not list,
//! on a tampered statement, and on any inconsistency in the successor.

use std::path::Path;

use ml_dsa::Keypair;
use verifiable_log::{
    key_id_for, Entry, ListedHead, ResetStatement, RetiredTree, SigningKey, Sth, VerifiableLog,
    STH_CONTEXT_COMMIT_LOG,
};
use verifiable_log_builder::commit_log_reset::{build_body, RetirementFacts};
use verifiable_log_builder::{build_bundle_from_leaves, build_commit_log_2_bundle, CommitRow};
use verifiable_log_serve::bundle::Bundle;
use verifiable_log_serve::{layout, remote, DevServer};

const TS: u64 = 1_761_669_796_874;

fn key() -> SigningKey {
    SigningKey::from_seed(&[21u8; 32].into())
}

fn row(seq: i64, conv: &str, epoch: i64) -> CommitRow {
    CommitRow {
        seq,
        conversation_id: conv.to_string(),
        generation: 0,
        epoch,
        sender_id: "u".to_string(),
        commit_sha256: hex::encode([(seq % 251) as u8; 32]),
    }
}

fn leaves(rows: &[CommitRow]) -> Vec<Entry> {
    rows.iter().map(|r| r.to_leaf().to_entry().unwrap()).collect()
}

fn write_json<T: serde::Serialize>(root: &Path, rel: &str, v: &T) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, serde_json::to_vec_pretty(v).unwrap()).unwrap();
}

struct Fixture {
    dir: tempfile::TempDir,
    facts: RetirementFacts,
    successor: Bundle,
}

/// Retired chain: 10 leaves, heads at 4, 5 and 10. Fork: 6 other leaves, the
/// frozen `latest.json` and `index.json` point at it. Superseded: a head at 2
/// from an earlier key. Successor: 4 leaves under commit-log-2.
fn fixture() -> Fixture {
    let k = key();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let chain_rows: Vec<CommitRow> = (1..=10).map(|s| row(s, &format!("c{s}"), 0)).collect();
    let chain = leaves(&chain_rows);
    let mut log = VerifiableLog::new();
    for e in &chain {
        log.append(e.clone()).unwrap();
    }

    // The fork: the publish that rebuilt from a pruned table. Its generated
    // /v1 tree is what is "frozen" in place, as in production.
    let fork_rows: Vec<CommitRow> = (40..46).map(|s| row(s, &format!("f{s}"), 0)).collect();
    let fork_bundle: Bundle = serde_json::from_value(
        serde_json::to_value(build_bundle_from_leaves(&leaves(&fork_rows), &k, TS + 9).unwrap())
            .unwrap(),
    )
    .unwrap();
    layout::generate(&fork_bundle, root).unwrap();
    let fork_head = fork_bundle.sths.last().unwrap().clone();
    let fork_mid = fork_bundle.sths.first().unwrap().clone();

    // The consistent chain's heads, still served beside it.
    let chain_head = |n: usize| Sth::create(&k, n as u64, log.root_at(n).unwrap(), TS + n as u64);
    let heads: Vec<Sth> = [4usize, 5, 10].iter().map(|n| chain_head(*n)).collect();
    for h in &heads {
        write_json(root, &format!("v1/sth/{}.json", h.tree_size), h);
    }
    let old = Sth::create(&SigningKey::from_seed(&[3u8; 32].into()), 2, [7u8; 32], 1);
    write_json(root, "v1/sth/2.json", &old);

    let listed = |s: &Sth, note: &str| ListedHead {
        tree_size: s.tree_size,
        root_hash: s.root_hash.clone(),
        timestamp: s.timestamp,
        note: note.into(),
    };
    let facts = RetirementFacts {
        reason: "test reset".into(),
        retired: RetiredTree {
            tree: "commit-log".into(),
            sth_context: String::from_utf8_lossy(STH_CONTEXT_COMMIT_LOG).into(),
            path: "v1".into(),
            last_consistent: heads[2].clone(),
            leaf_hashes: chain.iter().map(|e| hex::encode(e.leaf_hash())).collect(),
            chain_sizes: vec![4, 5, 10],
            forks: vec![listed(&fork_mid, "fork midpoint"), listed(&fork_head, "fork")],
            superseded: vec![listed(&old, "old")],
        },
    };

    // The successor, seeded from what is live and signed under its own context.
    let succ_rows: Vec<CommitRow> = (60..64).map(|s| row(s, "s", s - 60)).collect();
    let successor: Bundle = serde_json::from_value(
        serde_json::to_value(build_commit_log_2_bundle(&leaves(&succ_rows), &k, TS + 99).unwrap())
            .unwrap(),
    )
    .unwrap();
    layout::generate_commit_log_2(&successor, root).unwrap();

    let body = build_body(&facts, &successor.entries, TS + 100).unwrap();
    let st = ResetStatement::create(&k, body).unwrap();
    let mut map = std::collections::BTreeMap::new();
    layout::insert_reset_statement(&mut map, &st, &successor.key_candidates(0)).unwrap();
    for (rel, bytes) in map {
        std::fs::write(root.join(rel), bytes).unwrap();
    }
    Fixture { dir, facts, successor }
}

fn verify(root: &Path) -> remote::Report {
    let server = DevServer::spawn(root.to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();
    server.shutdown();
    report
}

fn failed(r: &remote::Report) -> Vec<String> {
    r.checks.iter().filter(|(ok, _)| !ok).map(|(_, l)| l.clone()).collect()
}

#[test]
fn a_retired_tree_with_listed_forks_passes_and_is_reported_retired() {
    let f = fixture();
    let report = verify(f.dir.path());
    assert!(report.ok, "must pass; failed: {:?}", failed(&report));
    assert!(report.info.iter().any(|(t, m)| t == "RETIRED" && m.contains("commit-log-2")));
    let known: Vec<&String> =
        report.info.iter().filter(|(t, _)| t == "KNOWN").map(|(_, m)| m).collect();
    assert!(known.iter().any(|m| m.contains("forked head at size 6")), "{known:?}");
    assert!(known.iter().any(|m| m.contains("forked head at size 3")), "{known:?}");
    let labels: Vec<&String> = report.checks.iter().map(|(_, l)| l).collect();
    assert!(labels.iter().any(|l| l.contains("commit-log-2: STH[4] signature")));
    assert!(labels.iter().any(|l| l.contains("genesis")));
    assert!(labels.iter().any(|l| l.contains("all 3 served heads on the consistent chain")));
}

#[test]
fn an_unlisted_fork_in_the_retired_tree_fails() {
    let f = fixture();
    let root = f.dir.path();
    // A head nobody listed: the freeze was broken after the statement was signed.
    let mut other = VerifiableLog::new();
    for s in 80..87 {
        other.append(leaves(&[row(s, "x", 0)])[0].clone()).unwrap();
    }
    let rogue = Sth::create(&key(), 7, other.root(), TS + 500);
    write_json(root, "v1/sth/7.json", &rogue);
    let mut index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("v1/index.json")).unwrap()).unwrap();
    index["sth_sizes"].as_array_mut().unwrap().push(7.into());
    write_json(root, "v1/index.json", &index);

    let report = verify(root);
    assert!(!report.ok);
    assert!(
        failed(&report).iter().any(|l| l.contains("NOT described") && l.contains("size 7")),
        "{:?}",
        failed(&report)
    );
}

#[test]
fn a_listed_size_served_with_another_root_fails() {
    let f = fixture();
    let root = f.dir.path();
    // Chain size 5, re-signed over a different root.
    write_json(root, "v1/sth/5.json", &Sth::create(&key(), 5, [1u8; 32], TS));
    let report = verify(root);
    assert!(!report.ok);
    assert!(failed(&report).iter().any(|l| l.contains("sizes [5]")), "{:?}", failed(&report));
}

#[test]
fn a_tampered_or_missing_reset_statement_fails() {
    let f = fixture();
    let root = f.dir.path();
    let path = root.join(layout::RESET_STATEMENT_PATH);
    let mut st: ResetStatement =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    st.statement.retired.forks.pop();
    write_json(root, layout::RESET_STATEMENT_PATH, &st);
    assert!(!verify(root).ok, "a statement edited after signing must not verify");

    std::fs::remove_file(&path).unwrap();
    let report = verify(root);
    assert!(!report.ok);
    assert!(failed(&report).iter().any(|l| l.contains("no reset statement")));
}

#[test]
fn a_statement_signed_by_another_key_fails() {
    let f = fixture();
    let root = f.dir.path();
    let stranger = SigningKey::from_seed(&[77u8; 32].into());
    let body = build_body(&f.facts, &f.successor.entries, TS).unwrap();
    let st = ResetStatement::create(&stranger, body).unwrap();
    write_json(root, layout::RESET_STATEMENT_PATH, &st);
    assert!(!verify(root).ok);
    assert_ne!(
        key_id_for(&stranger.verifying_key()),
        key_id_for(&key().verifying_key())
    );
}

#[test]
fn any_inconsistency_in_the_successor_fails() {
    let f = fixture();
    let root = f.dir.path();
    // Rewrite a published successor leaf.
    let mut e: Entry = f.successor.entries[1].clone();
    e.data.push(b' ');
    write_json(root, "v1/commit-log-2/entries/1.json", &e);
    let report = verify(root);
    assert!(!report.ok);
    assert!(failed(&report).iter().any(|l| l.starts_with("commit-log-2: ")));

    // A successor head signed under the RETIRED context is not a successor head.
    let f = fixture();
    let root = f.dir.path();
    let mut h = f.successor.sths.last().unwrap().clone();
    h = Sth::create(&key(), h.tree_size, h.root_bytes().unwrap(), h.timestamp);
    write_json(root, "v1/commit-log-2/sth/latest.json", &h);
    write_json(root, &format!("v1/commit-log-2/sth/{}.json", h.tree_size), &h);
    let report = verify(root);
    assert!(!report.ok);
    assert!(failed(&report).iter().any(|l| l.contains("commit-log-2: STH[4] signature")));
}

#[test]
fn a_successor_whose_first_leaf_is_not_the_named_genesis_fails() {
    let f = fixture();
    let root = f.dir.path();
    let other: Vec<CommitRow> = (70..74).map(|s| row(s, "t", s - 70)).collect();
    let b: Bundle = serde_json::from_value(
        serde_json::to_value(build_commit_log_2_bundle(&leaves(&other), &key(), TS).unwrap())
            .unwrap(),
    )
    .unwrap();
    layout::generate_commit_log_2(&b, root).unwrap();
    let report = verify(root);
    assert!(!report.ok);
    assert!(failed(&report).iter().any(|l| l.contains("genesis")), "{:?}", failed(&report));
}

/// The group verifier reads the successor once it is served, under its context.
#[test]
fn group_verification_reads_the_successor_tree() {
    let f = fixture();
    let server = DevServer::spawn(f.dir.path().to_path_buf(), 0).unwrap();
    let report = verifiable_log_serve::verify_group(&server.base_url(), "s").unwrap();
    server.shutdown();
    assert!(report.found);
    assert_eq!(report.commits.len(), 4);
    assert!(report.chain_valid, "{:?}", report.violations);
}
