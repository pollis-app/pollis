//! #1579: minting the reset statement that retires the first commit-log tree.
//!
//! The committed `commit-log-retirement.json` must describe itself consistently,
//! the mint must refuse unless it matches what is served, and the CLI must write
//! a statement that verifies only under the reset domain.
#![cfg(feature = "db-source")]

use std::collections::BTreeMap;
use std::process::Command;

use ml_dsa::Keypair;
use verifiable_log::{
    key_id_for, Entry, ListedHead, ResetStatement, RetiredTree, SigningKey, Sth, VerifiableLog,
    VerifyingKey, STH_CONTEXT_COMMIT_LOG,
};
use verifiable_log_builder::commit_log_reset::{build_body, check_against_served, RetirementFacts};

const TS: u64 = 1_700_000_000_000;
const KEY_HEX: &str = "0909090909090909090909090909090909090909090909090909090909090909";

fn key() -> SigningKey {
    SigningKey::from_seed(&[9u8; 32].into())
}

fn candidates(k: &SigningKey) -> Vec<(String, VerifyingKey)> {
    let vk = k.verifying_key();
    vec![(key_id_for(&vk), vk)]
}

/// A small retired tree: a 6-leaf chain with heads at 3 and 6, a fork at 4 and a
/// superseded head at 2, all served.
fn fixture() -> (RetirementFacts, BTreeMap<u64, Sth>) {
    let k = key();
    let mut chain = VerifiableLog::new();
    for i in 0..6u8 {
        chain.append(Entry::new("mls-commit-log", vec![i])).unwrap();
    }
    let mut fork = VerifiableLog::new();
    for i in 0..4u8 {
        fork.append(Entry::new("mls-commit-log", vec![100 + i])).unwrap();
    }
    let head = |size: u64, root| Sth::create(&k, size, root, TS);
    let h3 = head(3, chain.root_at(3).unwrap());
    let h6 = head(6, chain.root());
    let f4 = head(4, fork.root());
    let old2 = Sth::create(&SigningKey::from_seed(&[1u8; 32].into()), 2, [3u8; 32], TS);
    let facts = RetirementFacts {
        reason: "test".into(),
        retired: RetiredTree {
            tree: "commit-log".into(),
            sth_context: String::from_utf8_lossy(STH_CONTEXT_COMMIT_LOG).into(),
            path: "v1".into(),
            last_consistent: h6.clone(),
            leaf_hashes: (0..6)
                .map(|i| hex::encode(chain.entry(i).unwrap().leaf_hash()))
                .collect(),
            chain_sizes: vec![3, 6],
            forks: vec![ListedHead {
                tree_size: 4,
                root_hash: f4.root_hash.clone(),
                timestamp: TS,
                note: "fork".into(),
            }],
            superseded: vec![ListedHead {
                tree_size: 2,
                root_hash: old2.root_hash.clone(),
                timestamp: TS,
                note: "old".into(),
            }],
        },
    };
    let served = BTreeMap::from([(2, old2), (3, h3), (4, f4), (6, h6)]);
    (facts, served)
}

#[test]
fn the_mint_accepts_facts_that_match_what_is_served() {
    let (facts, served) = fixture();
    check_against_served(&facts, &served, &candidates(&key())).unwrap();
}

#[test]
fn the_mint_refuses_facts_that_disagree_with_what_is_served() {
    let (facts, mut served) = fixture();
    let k = key();
    // A listed head that is not served.
    let mut missing = served.clone();
    missing.remove(&3);
    assert!(check_against_served(&facts, &missing, &candidates(&k)).is_err());
    // A served chain head with another root.
    served.insert(3, Sth::create(&k, 3, [9u8; 32], TS));
    assert!(check_against_served(&facts, &served, &candidates(&k)).is_err());
    // A fork signed by a key the log does not hold.
    let (facts, mut served) = fixture();
    let stranger = SigningKey::from_seed(&[42u8; 32].into());
    let f = served[&4].clone();
    served.insert(4, Sth::create(&stranger, 4, f.root_bytes().unwrap(), TS));
    assert!(check_against_served(&facts, &served, &candidates(&k)).is_err());
    // A served head the facts leave out.
    let (facts, mut served) = fixture();
    served.insert(5, Sth::create(&k, 5, [8u8; 32], TS));
    assert!(check_against_served(&facts, &served, &candidates(&k)).is_err());
}

#[test]
fn the_body_names_the_successor_genesis_and_needs_one() {
    let (facts, _) = fixture();
    assert!(build_body(&facts, &[], TS).is_err(), "no leaves, no genesis");
    let first = Entry::new("mls-commit-log", b"first".to_vec());
    let body = build_body(&facts, std::slice::from_ref(&first), TS).unwrap();
    assert_eq!(body.successor.tree, "commit-log-2");
    assert_eq!(body.successor.path, "v1/commit-log-2");
    assert_eq!(body.successor.genesis_root_hash, hex::encode(first.leaf_hash()));
}

/// The committed facts describe a chain whose leaf hashes reproduce its own last
/// consistent head, and every listed fork is genuinely off that chain.
#[test]
fn the_committed_retirement_facts_are_self_consistent() {
    let raw = include_str!("../../commit-log-retirement.json");
    let facts: RetirementFacts = serde_json::from_str(raw).expect("parse");
    let body = build_body(&facts, &[Entry::new("mls-commit-log", vec![0])], TS).expect("valid");
    let r = &body.retired;
    assert_eq!(r.last_consistent.tree_size, 137);
    assert_eq!(r.leaf_hashes.len(), 137);
    let forks: Vec<u64> = r.forks.iter().map(|f| f.tree_size).collect();
    assert_eq!(forks, vec![56, 113, 129]);
    assert!(!facts.reason.contains('\u{2014}'), "no em dashes in published copy");
}

/// Run against the maintainer's saved copy of what verify.pollis.com serves
/// (`POLLIS_1579_EVIDENCE=<dir with v1/sth/*.json and v1/public_key.json>`):
/// every listed head is served with its root, and every chain and fork head
/// verifies under the served (pinned) log key. CI runs the same check for real,
/// against the live heads, before it signs.
#[test]
#[ignore = "needs the saved #1579 evidence; set POLLIS_1579_EVIDENCE"]
fn the_committed_retirement_facts_match_the_saved_evidence() {
    let dir = std::path::PathBuf::from(std::env::var("POLLIS_1579_EVIDENCE").expect("set it"));
    let facts: RetirementFacts =
        serde_json::from_str(include_str!("../../commit-log-retirement.json")).unwrap();
    let pk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("v1/public_key.json")).unwrap())
            .unwrap();
    let vk = verifiable_log::verifying_key_from_hex(pk["public_key"].as_str().unwrap()).unwrap();
    let mut served = BTreeMap::new();
    for e in std::fs::read_dir(dir.join("v1/sth")).unwrap() {
        let p = e.unwrap().path();
        let sth: Sth = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        if p.file_stem().unwrap() != "latest" {
            served.insert(sth.tree_size, sth);
        }
    }
    check_against_served(&facts, &served, &[(key_id_for(&vk), vk)]).unwrap();
}

#[test]
fn the_cli_mints_a_statement_that_verifies_only_as_a_reset() {
    let (facts, served) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let sths = dir.path().join("sths");
    std::fs::create_dir_all(&sths).unwrap();
    for (size, sth) in &served {
        std::fs::write(sths.join(format!("{size}.json")), serde_json::to_string(sth).unwrap())
            .unwrap();
    }
    let facts_path = dir.path().join("facts.json");
    std::fs::write(&facts_path, serde_json::to_string(&facts).unwrap()).unwrap();
    let leaves = dir.path().join("leaves.json");
    let first = Entry::new("mls-commit-log", b"genesis".to_vec());
    std::fs::write(&leaves, serde_json::to_string(&vec![first.clone()]).unwrap()).unwrap();
    let out = dir.path().join("reset.json");

    let run = |leaves: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_builder"))
            .args([
                "commit-log-reset",
                "--retirement",
                facts_path.to_str().unwrap(),
                "--retired-sths",
                sths.to_str().unwrap(),
                "--leaves",
                leaves.to_str().unwrap(),
                "--issued-at",
                &TS.to_string(),
                "--out",
                out.to_str().unwrap(),
            ])
            .env("VLOG_SIGNING_KEY", KEY_HEX)
            .output()
            .unwrap()
    };
    let o = run(&leaves);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let st: ResetStatement =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    st.verify_any(&candidates(&key())).unwrap();
    assert_eq!(st.statement.successor.genesis_root_hash, hex::encode(first.leaf_hash()));

    // No successor leaves yet: refused, nothing half-written is trusted.
    let empty = dir.path().join("empty.json");
    std::fs::write(&empty, "[]").unwrap();
    assert!(!run(&empty).status.success());
}
