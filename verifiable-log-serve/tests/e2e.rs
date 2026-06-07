//! End-to-end gate for the serve layer:
//! (a) the layout generator produces the documented files for a fixture bundle;
//! (b) the dev server serves them and `verify_remote` verifies the whole log
//!     over HTTP (signatures + inclusion + consistency);
//! (c) tampering with a served artifact makes remote verification FAIL.
//!
//! The fixture is built directly from slice 1's primitives (this mirrors what
//! the builder does — it does not reimplement any verification).

use std::path::Path;

use ed25519_dalek::SigningKey;
use verifiable_log::{Entry, Sth, UniqueDataInvariant, VerifiableLog};
use verifiable_log_serve::bundle::{Bundle, ConsistencyCheck, InclusionCheck};
use verifiable_log_serve::{layout, remote, DevServer};

/// Build a small, valid signed bundle in the frozen wire shape.
fn build_fixture() -> Bundle {
    let signing_key = SigningKey::from_bytes(&[9u8; 32]);
    let mut log = VerifiableLog::new();
    log.register_invariant("commits", Box::new(UniqueDataInvariant));

    let entries = vec![
        Entry::new("commits", b"group-a/epoch-0".to_vec()),
        Entry::new("commits", b"group-a/epoch-1".to_vec()),
        Entry::new("accounts", b"alice/key-v1".to_vec()),
        Entry::new("commits", b"group-b/epoch-0".to_vec()),
    ];
    for e in &entries {
        log.append(e.clone()).unwrap();
    }

    let n = log.size();
    let m = n / 2;
    let ts = 1_700_000_000_000u64;

    let mid = Sth::create(&signing_key, m as u64, log.root_at(m).unwrap(), ts);
    let full = log.signed_tree_head(&signing_key, ts);
    let sths = vec![mid, full];
    let final_index = sths.len() - 1;

    let inclusion = (0..n)
        .map(|i| InclusionCheck {
            entry: entries[i].clone(),
            proof: log.inclusion_proof(i).unwrap(),
            sth_index: final_index,
        })
        .collect();

    let consistency = vec![ConsistencyCheck {
        old_index: 0,
        new_index: final_index,
        proof: log.consistency_proof(m, n).unwrap(),
    }];

    Bundle {
        public_key: hex::encode(signing_key.verifying_key().to_bytes()),
        sths,
        entries,
        enforce_unique: vec!["commits".to_string()],
        inclusion,
        consistency,
    }
}

/// Generate the fixture's static tree into `root`, returning the bundle used.
fn generate_into(root: &Path) -> Bundle {
    let bundle = build_fixture();
    layout::generate(&bundle, root).unwrap();
    bundle
}

#[test]
fn layout_generator_writes_documented_files() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = generate_into(dir.path());
    let v1 = dir.path().join("v1");

    // The two top-level documents.
    assert!(v1.join("public_key.json").is_file());
    assert!(v1.join("index.json").is_file());

    // STHs: latest plus one per size.
    assert!(v1.join("sth").join("latest.json").is_file());
    for sth in &bundle.sths {
        assert!(v1
            .join("sth")
            .join(format!("{}.json", sth.tree_size))
            .is_file());
    }

    // Entries: the ordered list and every per-entry file.
    assert!(v1.join("entries.json").is_file());
    for i in 0..bundle.entries.len() {
        assert!(v1.join("entries").join(format!("{i}.json")).is_file());
    }

    // Inclusion proofs at /v1/proof/inclusion/<tree_size>/<leaf_index>.json.
    for check in &bundle.inclusion {
        assert!(v1
            .join("proof")
            .join("inclusion")
            .join(check.proof.tree_size.to_string())
            .join(format!("{}.json", check.proof.leaf_index))
            .is_file());
    }

    // Consistency proofs at /v1/proof/consistency/<first>-<second>.json.
    for check in &bundle.consistency {
        assert!(v1
            .join("proof")
            .join("consistency")
            .join(format!(
                "{}-{}.json",
                check.proof.first_size, check.proof.second_size
            ))
            .is_file());
    }

    // The manifest advertises exactly what was written.
    let manifest: verifiable_log_serve::Manifest =
        serde_json::from_str(&std::fs::read_to_string(v1.join("index.json")).unwrap()).unwrap();
    assert_eq!(manifest.entry_count, bundle.entries.len() as u64);
    assert_eq!(manifest.inclusion.len(), bundle.inclusion.len());
    assert_eq!(manifest.consistency.len(), bundle.consistency.len());
    assert_eq!(manifest.latest_tree_size, Some(bundle.entries.len() as u64));
}

#[test]
fn fetch_over_http_and_verify_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    generate_into(dir.path());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();

    assert!(
        report.ok,
        "remote verification should pass; checks: {:?}",
        report.checks
    );
    // Sanity: it actually ran a meaningful number of checks, not zero.
    assert!(report.checks.len() >= 8, "too few checks ran");
    server.shutdown();
}

#[test]
fn tampered_entry_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    generate_into(dir.path());

    // Corrupt one per-entry artifact: swap its payload for a different one.
    let entry_path = dir.path().join("v1").join("entries").join("0.json");
    let tampered = r#"{"tenant":"commits","data":"6861786f7264"}"#;
    std::fs::write(&entry_path, tampered).unwrap();

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();
    assert!(!report.ok, "tampered entry must fail verification");
    server.shutdown();
}

#[test]
fn tampered_sth_signature_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = generate_into(dir.path());

    // Flip a hex nibble in the largest STH's signature.
    let size = bundle.entries.len();
    let sth_path = dir.path().join("v1").join("sth").join(format!("{size}.json"));
    let mut sth: Sth =
        serde_json::from_str(&std::fs::read_to_string(&sth_path).unwrap()).unwrap();
    let mut sig: Vec<char> = sth.signature.chars().collect();
    sig[0] = if sig[0] == 'a' { 'b' } else { 'a' };
    sth.signature = sig.into_iter().collect();
    std::fs::write(&sth_path, serde_json::to_string_pretty(&sth).unwrap()).unwrap();

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();
    assert!(!report.ok, "tampered STH signature must fail verification");
    server.shutdown();
}

#[test]
fn tampered_entries_list_breaks_root() {
    let dir = tempfile::tempdir().unwrap();
    generate_into(dir.path());

    // Corrupt the ordered entries list: replay roots will no longer match the
    // signed STHs.
    let entries_path = dir.path().join("v1").join("entries.json");
    let tampered =
        r#"[{"tenant":"commits","data":"00"},{"tenant":"commits","data":"01"},{"tenant":"accounts","data":"02"},{"tenant":"commits","data":"03"}]"#;
    std::fs::write(&entries_path, tampered).unwrap();

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();
    assert!(!report.ok, "tampered entries list must fail verification");
    server.shutdown();
}
