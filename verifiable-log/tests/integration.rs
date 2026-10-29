//! Deterministic gate suite for the verifiable append-only log.

use std::process::Command;

use ml_dsa::Keypair;
use verifiable_log::SigningKey;
use verifiable_log::{
    is_equivocation, proof, Entry, InvariantViolation, Sth, TenantInvariant, UniqueDataInvariant,
    VerifiableLog,
};

fn test_key() -> SigningKey {
    SigningKey::from_seed(&[42u8; 32].into())
}

fn build_log(n: usize) -> VerifiableLog {
    let mut log = VerifiableLog::new();
    for i in 0..n {
        log.append(Entry::new("t", format!("entry-{i}").into_bytes()))
            .unwrap();
    }
    log
}

#[test]
fn valid_inclusion_proofs_pass_for_every_leaf() {
    let key = test_key();
    for n in 1..=9usize {
        let log = build_log(n);
        let sth = log.signed_tree_head(&key, 1000);
        for i in 0..n {
            let entry = log.entry(i).unwrap().clone();
            let p = log.inclusion_proof(i).unwrap();
            assert!(
                proof::verify_inclusion_proof(&entry, &p, &sth),
                "leaf {i} of {n} should verify"
            );
        }
    }
}

#[test]
fn sth_signature_verifies_and_rejects_wrong_key() {
    let log = build_log(4);
    let sth = log.signed_tree_head(&test_key(), 1234);
    assert!(sth.verify(&test_key().verifying_key()));

    let other = SigningKey::from_seed(&[7u8; 32].into());
    assert!(!sth.verify(&other.verifying_key()));
}

#[test]
fn tampered_leaf_fails_verification() {
    let log = build_log(5);
    let sth = log.signed_tree_head(&test_key(), 1);
    let p = log.inclusion_proof(2).unwrap();

    let forged = Entry::new("t", b"not-the-real-entry".to_vec());
    assert!(!proof::verify_inclusion_proof(&forged, &p, &sth));
}

#[test]
fn tampered_root_fails_verification() {
    let log = build_log(5);
    let mut sth = log.signed_tree_head(&test_key(), 1);
    let entry = log.entry(2).unwrap().clone();
    let p = log.inclusion_proof(2).unwrap();

    let mut root = sth.root_hash.clone();
    let first = root.remove(0);
    root.insert(0, if first == 'a' { 'b' } else { 'a' });
    sth.root_hash = root;

    assert!(!proof::verify_inclusion_proof(&entry, &p, &sth));
    assert!(!sth.verify(&test_key().verifying_key()));
}

#[test]
fn tampered_proof_fails_verification() {
    let log = build_log(6);
    let sth = log.signed_tree_head(&test_key(), 1);
    let entry = log.entry(4).unwrap().clone();
    let mut p = log.inclusion_proof(4).unwrap();

    assert!(!p.audit_path.is_empty());
    let mut h = p.audit_path[0].clone();
    let c = h.remove(0);
    h.insert(0, if c == '0' { '1' } else { '0' });
    p.audit_path[0] = h;

    assert!(!proof::verify_inclusion_proof(&entry, &p, &sth));
}

#[test]
fn consistency_holds_across_appends() {
    let key = test_key();
    let mut log = VerifiableLog::new();
    let mut sths: Vec<(usize, Sth)> = Vec::new();
    for i in 0..8usize {
        log.append(Entry::new("t", format!("e{i}").into_bytes()))
            .unwrap();
        sths.push((log.size(), log.signed_tree_head(&key, i as u64)));
    }

    for a in 0..sths.len() {
        for b in a..sths.len() {
            let (first, ref old) = sths[a];
            let (second, ref new) = sths[b];
            let cp = log.consistency_proof(first, second).unwrap();
            assert!(
                proof::verify_consistency_proof(old, new, &cp),
                "consistency {first}->{second} should hold"
            );
        }
    }
}

#[test]
fn forged_consistency_proof_fails() {
    let key = test_key();
    let mut log = VerifiableLog::new();
    for i in 0..4usize {
        log.append(Entry::new("t", format!("e{i}").into_bytes()))
            .unwrap();
    }
    let old = log.signed_tree_head(&key, 1);
    let old_root = old.tree_size; // capture size before more appends
    assert_eq!(old_root, 4);

    for i in 4..7usize {
        log.append(Entry::new("t", format!("e{i}").into_bytes()))
            .unwrap();
    }
    let new = log.signed_tree_head(&key, 2);

    let mut cp = log.consistency_proof(4, 7).unwrap();
    assert!(proof::verify_consistency_proof(&old, &new, &cp));

    let mut h = cp.path[0].clone();
    let c = h.remove(0);
    h.insert(0, if c == '0' { '1' } else { '0' });
    cp.path[0] = h;
    assert!(!proof::verify_consistency_proof(&old, &new, &cp));

    let mut forged_new = new.clone();
    let mut r = forged_new.root_hash.clone();
    let c = r.remove(0);
    r.insert(0, if c == 'a' { 'b' } else { 'a' });
    forged_new.root_hash = r;
    let good = log.consistency_proof(4, 7).unwrap();
    assert!(!proof::verify_consistency_proof(&old, &forged_new, &good));
}

#[test]
fn equivocation_is_detected() {
    let key = test_key();

    let mut log_a = VerifiableLog::new();
    let mut log_b = VerifiableLog::new();
    for i in 0..3usize {
        log_a
            .append(Entry::new("t", format!("a{i}").into_bytes()))
            .unwrap();
        log_b
            .append(Entry::new("t", format!("b{i}").into_bytes()))
            .unwrap();
    }
    let sth_a = log_a.signed_tree_head(&key, 1);
    let sth_b = log_b.signed_tree_head(&key, 1);

    assert_eq!(sth_a.tree_size, sth_b.tree_size);
    assert_ne!(sth_a.root_hash, sth_b.root_hash);
    assert!(is_equivocation(&sth_a, &sth_b));

    let sth_a2 = log_a.signed_tree_head(&key, 999);
    assert!(!is_equivocation(&sth_a, &sth_a2));
}

#[test]
fn tenant_invariant_rejects_violating_append() {
    let mut log = VerifiableLog::new();
    log.register_invariant("commits", Box::new(UniqueDataInvariant));

    assert!(log.append(Entry::new("commits", b"epoch-0".to_vec())).is_ok());
    let err = log
        .append(Entry::new("commits", b"epoch-0".to_vec()))
        .unwrap_err();
    assert!(matches!(err, verifiable_log::Error::Invariant(_)));
    assert_eq!(log.size(), 1);

    // No hook on this tenant, so the same payload is accepted.
    assert!(log.append(Entry::new("other", b"epoch-0".to_vec())).is_ok());
    assert_eq!(log.size(), 2);
}

#[test]
fn custom_tenant_invariant_hook() {
    // Rejects entries whose payload does not start with "ok-".
    struct PrefixInvariant;
    impl TenantInvariant for PrefixInvariant {
        fn check(&self, _existing: &[&Entry], candidate: &Entry) -> Result<(), InvariantViolation> {
            if candidate.data.starts_with(b"ok-") {
                Ok(())
            } else {
                Err(InvariantViolation::new(
                    candidate.tenant.clone(),
                    "payload must start with ok-",
                ))
            }
        }
    }

    let mut log = VerifiableLog::new();
    log.register_invariant("strict", Box::new(PrefixInvariant));
    assert!(log.append(Entry::new("strict", b"ok-1".to_vec())).is_ok());
    assert!(log.append(Entry::new("strict", b"bad".to_vec())).is_err());
    assert_eq!(log.size(), 1);
}

fn monitor_bin() -> &'static str {
    env!("CARGO_BIN_EXE_monitor")
}

fn unique_path(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("verifiable-log-{}-{}.json", std::process::id(), tag));
    p
}

#[test]
fn cli_verifies_known_good_fixture_and_rejects_tampered() {
    let good = unique_path("good");

    let status = Command::new(monitor_bin())
        .arg("gen-example")
        .arg(&good)
        .status()
        .expect("run gen-example");
    assert!(status.success(), "gen-example should succeed");

    let status = Command::new(monitor_bin())
        .arg("verify")
        .arg(&good)
        .status()
        .expect("run verify");
    assert!(status.success(), "known-good fixture must exit 0");

    let raw = std::fs::read_to_string(&good).unwrap();
    let mut bundle: serde_json::Value = serde_json::from_str(&raw).unwrap();
    bundle["entries"][0]["data"] = serde_json::Value::String("deadbeef".to_string());
    let tampered = unique_path("tampered");
    std::fs::write(&tampered, serde_json::to_string_pretty(&bundle).unwrap()).unwrap();

    let status = Command::new(monitor_bin())
        .arg("verify")
        .arg(&tampered)
        .status()
        .expect("run verify tampered");
    assert!(!status.success(), "tampered fixture must exit non-zero");

    let _ = std::fs::remove_file(&good);
    let _ = std::fs::remove_file(&tampered);
}

#[test]
fn cli_rejects_equivocating_fixture() {
    let good = unique_path("equiv-good");
    let status = Command::new(monitor_bin())
        .arg("gen-example")
        .arg(&good)
        .status()
        .expect("run gen-example");
    assert!(status.success());

    // Equivocation is a root/size conflict; the forged STH need not be signed.
    let raw = std::fs::read_to_string(&good).unwrap();
    let mut bundle: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let mut clash = bundle["sths"][1].clone();
    clash["root_hash"] =
        serde_json::Value::String("00".repeat(32));
    bundle["sths"].as_array_mut().unwrap().push(clash);
    let path = unique_path("equiv");
    std::fs::write(&path, serde_json::to_string_pretty(&bundle).unwrap()).unwrap();

    let status = Command::new(monitor_bin())
        .arg("verify")
        .arg(&path)
        .status()
        .expect("run verify");
    assert!(!status.success(), "equivocating fixture must exit non-zero");

    let _ = std::fs::remove_file(&good);
    let _ = std::fs::remove_file(&path);
}
