//! End-to-end gate suite for the `monitor` CLI — the thing an auditor actually
//! runs.
//!
//! Two failures this covers were, until #875, "verify returns false" for logs
//! that are perfectly honest:
//!
//! * The CLI hard-coded the commit-log STH context, so an **account-keys** or
//!   **binaries** bundle — both of which the builder's README tells you to check
//!   with this exact command — reported FAIL on every head. Two builder tests had
//!   even frozen that as expected behaviour by re-implementing the loop
//!   themselves.
//! * Its private bundle struct had no `retired_keys` field, so serde dropped the
//!   rotation-overlap key set and every head minted under a **retiring** key
//!   failed to verify. A key rotation would have made the log look forged.

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

/// A complete, well-formed bundle over `tenant`, signed by `sk` under
/// `context` — the shape the builder emits for whichever tree it just built.
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
    // Surfaced on failure so a broken run reads as a report, not an exit code.
    if !out.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&out.stdout));
    }
    out.status.success()
}

/// The account-key tree is domain-separated from the commit log, so the monitor
/// must be *told* which tree it is checking. It verifies under the right one and
/// — the property the separation exists for — refuses under the wrong one.
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

/// Same for the binaries tree — the one the builder README's `build-binaries`
/// example tells an auditor to check with this command.
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

/// The commit log stays the default, so the command an auditor has always run
/// keeps working with no new flag.
#[test]
fn commit_log_remains_the_default_tree() {
    let dir = tempfile::tempdir().unwrap();
    let b = bundle_for("commits", STH_CONTEXT_COMMIT_LOG, &key(7));
    assert!(monitor_verify(dir.path(), "commits.json", &b, &[]));
}

/// An unknown `--tree` is rejected rather than silently falling back to the
/// commit log, which would report FAIL for an honest account-key bundle and read
/// as tampering.
#[test]
fn an_unknown_tree_name_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let b = bundle_for("commits", STH_CONTEXT_COMMIT_LOG, &key(7));
    assert!(!monitor_verify(dir.path(), "commits.json", &b, &["--tree", "nope"]));
}

/// A rotation in progress: the bundle publishes the NEW key as active and the
/// outgoing key in `retired_keys` with its window end, while the heads on disk
/// are still the ones the OLD key signed. Inside the window the monitor must
/// accept them — an honest log mid-rotation is the common case, and reporting it
/// as tampering is the failure mode a verifier must never have.
#[test]
fn monitor_accepts_a_head_from_a_retiring_key_inside_the_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let outgoing = key(3);
    let incoming = key(5);
    let window_end = TS + 30 * 86_400_000;

    // Heads signed by the OUTGOING key; the bundle advertises the INCOMING one.
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

/// The overlap set is not a free pass: a head from a key that is neither active
/// nor published as retiring still fails.
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

/// `gen-example` still round-trips through the (now shared) bundle type.
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
