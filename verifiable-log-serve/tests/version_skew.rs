//! Mixed-version gate (#701): the served bundle carries a `format_version`, and a
//! verifier decides "am I too old for this log?" *before* it deserializes or
//! verifies anything. This is the test that stops the next breaking wire change
//! from surfacing to an auditor as a serde/verification error instead of an
//! honest "upgrade your verifier".
//!
//! It pins three behaviours of the CURRENT verifier against served manifests of
//! different shapes:
//!
//! 1. the **current** bundle (`format_version == FORMAT_VERSION`) verifies;
//! 2. a **future** bundle (`format_version > FORMAT_VERSION`) is rejected as
//!    [`ServeError::VersionSkew`] — the reviewer's "who stops working" case,
//!    simulated by making *this* verifier the stale one — not a verification
//!    failure and not a serde error;
//! 3. a **legacy** bundle (no `format_version`, with the pre-#701 `conversations`
//!    field) still deserializes and verifies — the field was always
//!    `#[serde(default)]`, so its removal was never a missing-field error. This is
//!    the correction to the review note's exact mechanism, encoded as a test.
//!
//! The same gate guards `verify_group`, which decodes commit leaves whose encoding
//! changed at the republish — checked here too.

use std::path::Path;

use ml_dsa::Keypair;
use serde_json::Value;
use verifiable_log::SigningKey;
use verifiable_log::{Entry, Sth, UniqueDataInvariant, VerifiableLog};
use verifiable_log_serve::bundle::{Bundle, ConsistencyCheck, InclusionCheck, FORMAT_VERSION};
use verifiable_log_serve::error::ServeError;
use verifiable_log_serve::{group, layout, remote, DevServer};

/// A small, valid signed bundle in the frozen wire shape (mirrors the e2e
/// fixture — the serve layer never reimplements verification).
fn build_fixture() -> Bundle {
    let signing_key = SigningKey::from_seed(&[9u8; 32].into());
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
        public_key: hex::encode(signing_key.verifying_key().encode()),
        retired_keys: Vec::new(),
        sths,
        entries,
        enforce_unique: vec!["commits".to_string()],
        inclusion,
        consistency,
    }
}

/// Generate the fixture's static tree into `root`.
fn generate_into(root: &Path) {
    layout::generate(&build_fixture(), root).unwrap();
}

/// Rewrite the served `/v1/index.json` by mutating it as a JSON object — so a test
/// can forge a future/legacy manifest shape without touching any other artifact.
fn rewrite_index(root: &Path, mutate: impl FnOnce(&mut serde_json::Map<String, Value>)) {
    let path = root.join("v1").join("index.json");
    let mut value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    mutate(value.as_object_mut().expect("index.json is a JSON object"));
    std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
}

/// The generator stamps the current format version into the manifest, and it is
/// the number the gate keys on.
#[test]
fn generated_manifest_carries_the_current_format_version() {
    let dir = tempfile::tempdir().unwrap();
    generate_into(dir.path());
    let index = std::fs::read_to_string(dir.path().join("v1").join("index.json")).unwrap();
    let value: Value = serde_json::from_str(&index).unwrap();
    assert_eq!(
        value.get("format_version").and_then(Value::as_u64),
        Some(FORMAT_VERSION as u64),
        "index.json must advertise the current wire format version"
    );
}

/// (1) The current bundle verifies end to end — the gate is a no-op when the log
/// is at a version we understand.
#[test]
fn current_bundle_verifies() {
    let dir = tempfile::tempdir().unwrap();
    generate_into(dir.path());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url()).unwrap();
    assert!(report.ok, "current-format bundle must verify; checks: {:?}", report.checks);
    server.shutdown();
}

/// (2) A log published in a format NEWER than this verifier understands is
/// rejected as version skew — a distinct error, before any check runs — not an
/// `Ok(Report{ok:false})` and not a serde error. This is the "stale verifier the
/// day after a republish" case, with the roles reversed so the current binary
/// plays the stale one.
#[test]
fn future_format_is_reported_as_version_skew_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    generate_into(dir.path());
    let future = FORMAT_VERSION + 1;
    rewrite_index(dir.path(), |m| {
        m.insert("format_version".to_string(), Value::from(future));
    });

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    // `Report` is not `Debug`, so match rather than `expect_err`.
    match remote::verify_remote(&server.base_url()) {
        Err(ServeError::VersionSkew { served, supported }) => {
            assert_eq!(served, future);
            assert_eq!(supported, FORMAT_VERSION);
        }
        Ok(_) => panic!("a future-format log must not verify as a normal report"),
        Err(other) => panic!("expected VersionSkew, got {other:?}"),
    }
    server.shutdown();
}

/// (2b) The same gate guards `verify_group`, which decodes commit leaves whose
/// encoding changed at the republish. A future-format log is skew here too —
/// never a silent "Found: no".
#[test]
fn future_format_is_version_skew_for_group_too() {
    let dir = tempfile::tempdir().unwrap();
    generate_into(dir.path());
    rewrite_index(dir.path(), |m| {
        m.insert("format_version".to_string(), Value::from(FORMAT_VERSION + 1));
    });

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let err = group::verify_group(&server.base_url(), "01KP443BSBXS3W1SZNTV5MXQ9C")
        .expect_err("a future-format log must be skew for group, not an empty result");
    assert!(
        matches!(err, ServeError::VersionSkew { .. }),
        "expected VersionSkew, got {err:?}"
    );
    server.shutdown();
}

/// (3) A LEGACY manifest — no `format_version` (→ 0, treated as pre-#701) and the
/// old `conversations` field present — still deserializes and still verifies.
/// This refutes the review note's exact mechanism: the removed `conversations`
/// field was `#[serde(default)]`, so its absence was never a missing-field serde
/// error, and a lower/absent version is backward-compatible, not skew. It matters
/// for the transition window, when a new verifier may hit the not-yet-republished
/// (legacy) log.
#[test]
fn legacy_manifest_still_verifies_and_is_not_skew() {
    let dir = tempfile::tempdir().unwrap();
    generate_into(dir.path());
    rewrite_index(dir.path(), |m| {
        m.remove("format_version");
        m.insert(
            "conversations".to_string(),
            Value::from(vec!["conv-a", "conv-b"]),
        );
    });

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let report = remote::verify_remote(&server.base_url())
        .expect("a legacy manifest must not error — it is backward-compatible");
    assert!(report.ok, "legacy-shape bundle must still verify; checks: {:?}", report.checks);
    server.shutdown();
}
