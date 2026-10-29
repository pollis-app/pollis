//! End-to-end gate suite for the `monitor` CLI — the thing an auditor actually
//! runs.

use std::path::Path;
use std::process::Command;

use ml_dsa::Keypair;
use verifiable_log::bundle::{Bundle, ConsistencyCheck, InclusionCheck, PublicKeyEntry};
use verifiable_log::{
    key_id_for, Entry, SigningKey, Sth, UniqueDataInvariant, VerifiableLog,
    STH_CONTEXT_ACCOUNT_KEYS, STH_CONTEXT_BINARIES, STH_CONTEXT_COMMIT_LOG,
};

const TS: u64 = 1_700_000_000_000;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_seed(&[seed; 32].into())
}

fn hex_key(sk: &SigningKey) -> String {
    hex::encode(sk.verifying_key().encode())
}

/// A complete bundle over `tenant`, signed by `sk` under `context`.
fn bundle_for(tenant: &str, context: &[u8], sk: &SigningKey) -> Bundle {
    let mut log = VerifiableLog::new();
    log.register_invariant(tenant, Box::new(UniqueDataInvariant));

    let entries: Vec<Entry> = (0..4u8)
        .map(|i| Entry::new(tenant, format!("leaf-{i}").into_bytes()))
        .collect();
    for e in &entries {
        log.append(e.clone()).expect("append");
    }

    let mid = 2usize;
    let sth_mid = Sth::create_with_context(sk, mid as u64, log.root_at(mid).unwrap(), TS, context);
    let sth_full =
        Sth::create_with_context(sk, log.size() as u64, log.root(), TS + 1_000, context);

    let inclusion = (0..log.size())
        .map(|i| InclusionCheck {
            entry: entries[i].clone(),
            proof: log.inclusion_proof(i).unwrap(),
            sth_index: 1,
        })
        .collect();

    let consistency = vec![ConsistencyCheck {
        old_index: 0,
        new_index: 1,
        proof: log.consistency_proof(mid, log.size()).unwrap(),
    }];

    Bundle {
        public_key: hex_key(sk),
        retired_keys: Vec::new(),
        sths: vec![sth_mid, sth_full],
        entries,
        enforce_unique: vec![tenant.to_string()],
        inclusion,
        consistency,
    }
}

/// Run `monitor verify` over a bundle and return whether it exited zero.
fn monitor_verify(dir: &Path, name: &str, bundle: &Bundle, args: &[&str]) -> bool {
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_string_pretty(bundle).unwrap()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_monitor"))
        .arg("verify")
        .arg(&path)
        .args(args)
        .output()
        .expect("failed to spawn monitor");
    if !out.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&out.stdout));
    }
    out.status.success()
}

/// The account-key tree verifies under its own context and fails under the commit log.
#[test]
fn monitor_verifies_an_account_keys_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let b = bundle_for("account-key", STH_CONTEXT_ACCOUNT_KEYS, &key(9));

    assert!(
        monitor_verify(dir.path(), "account.json", &b, &["--tree", "account-keys"]),
        "an account-keys bundle must verify under its own tree"
    );
    assert!(
        !monitor_verify(dir.path(), "account.json", &b, &["--tree", "commit-log"]),
        "an account-keys head must NOT verify as a commit-log head"
    );
    assert!(
        !monitor_verify(dir.path(), "account.json", &b, &["--tree", "binaries"]),
        "an account-keys head must NOT verify as a binaries head"
    );
}

#[test]
fn monitor_verifies_a_binaries_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let b = bundle_for("binaries", STH_CONTEXT_BINARIES, &key(11));

    assert!(
        monitor_verify(dir.path(), "binaries.json", &b, &["--tree", "binaries"]),
        "a binaries bundle must verify under its own tree"
    );
    assert!(
        !monitor_verify(dir.path(), "binaries.json", &b, &["--tree", "commit-log"]),
        "a binaries head must NOT verify as a commit-log head"
    );
}

#[test]
fn commit_log_remains_the_default_tree() {
    let dir = tempfile::tempdir().unwrap();
    let b = bundle_for("commits", STH_CONTEXT_COMMIT_LOG, &key(7));
    assert!(monitor_verify(dir.path(), "commits.json", &b, &[]));
}

/// An unknown `--tree` is rejected rather than falling back to the commit log.
#[test]
fn an_unknown_tree_name_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let b = bundle_for("commits", STH_CONTEXT_COMMIT_LOG, &key(7));
    assert!(!monitor_verify(dir.path(), "commits.json", &b, &["--tree", "nope"]));
}

/// Mid-rotation: heads from the outgoing key verify inside its `retired_keys` window.
#[test]
fn monitor_accepts_a_head_from_a_retiring_key_inside_the_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let outgoing = key(3);
    let incoming = key(5);
    let window_end = TS + 30 * 86_400_000;

    let mut b = bundle_for("commits", STH_CONTEXT_COMMIT_LOG, &outgoing);
    b.public_key = hex_key(&incoming);
    b.retired_keys = vec![PublicKeyEntry {
        key_id: key_id_for(&outgoing.verifying_key()),
        algorithm: "ML-DSA-44".to_string(),
        public_key: hex_key(&outgoing),
        not_after: Some(window_end),
    }];

    assert!(
        monitor_verify(
            dir.path(),
            "rotating.json",
            &b,
            &["--now-ms", &(window_end - 1).to_string()]
        ),
        "a head from a retiring key must verify inside its overlap window"
    );

    assert!(
        !monitor_verify(
            dir.path(),
            "rotating.json",
            &b,
            &["--now-ms", &(window_end + 1).to_string()]
        ),
        "past its not_after the retired key must stop being accepted — the \
         overlap window is a deadline, not a suggestion"
    );
}

/// The overlap set is no free pass: an unpublished key still fails.
#[test]
fn a_head_from_an_unpublished_key_still_fails() {
    let dir = tempfile::tempdir().unwrap();
    let stranger = key(42);
    let mut b = bundle_for("commits", STH_CONTEXT_COMMIT_LOG, &stranger);
    b.public_key = hex_key(&key(5));
    b.retired_keys = vec![PublicKeyEntry {
        key_id: key_id_for(&key(3).verifying_key()),
        algorithm: "ML-DSA-44".to_string(),
        public_key: hex_key(&key(3)),
        not_after: Some(TS + 1_000_000),
    }];
    assert!(!monitor_verify(dir.path(), "forged.json", &b, &["--now-ms", &TS.to_string()]));
}

#[test]
fn gen_example_round_trips_through_verify() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("example.json");
    let status = Command::new(env!("CARGO_BIN_EXE_monitor"))
        .args(["gen-example", path.to_str().unwrap()])
        .status()
        .expect("spawn monitor");
    assert!(status.success());

    let status = Command::new(env!("CARGO_BIN_EXE_monitor"))
        .args(["verify", path.to_str().unwrap()])
        .status()
        .expect("spawn monitor");
    assert!(status.success());
}
