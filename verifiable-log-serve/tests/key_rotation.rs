//! Rotation-overlap acceptance across **every** verify path (#875).
//!
//! The rotation design (`docs/sth-signing-key-custody.md` §5) says a signing-key
//! changeover must be invisible: during the overlap window the log publishes the
//! new active key *and* the retiring one (with its `not_after`), and a head
//! signed by either is honest. `public_key.json` and `sth/latest.json` are
//! separate artifacts on separate cache policies, and the rotation runbook (§7
//! step 5) syncs them in two passes — so there is a real, expected window in
//! which the served key document already names the successor while the served
//! head is still signed by the retiring key. That is exactly the state modelled
//! here.
//!
//! Four verify paths must agree about it:
//!
//! * `remote::verify_remote`      — the `pollis-verify verify` whole-log audit
//! * `group::verify_group`        — `pollis-verify group` / the live endpoint
//! * `account::verify_account`    — the in-app self/peer key audit
//! * `release::verify_release`    — the in-app "verify my build" check
//!
//! Before #875 only the first applied the overlap set: the other three rebuilt a
//! `Bundle` with `retired_keys: Vec::new()` and verified against the single
//! active key, so an honest mid-rotation log made a shipped client raise a hard
//! `AuditStatus::Alarm` — a client screaming "tampering" at an honest server.
//!
//! The negatives are the teeth: a key whose overlap window has **closed**, and a
//! key that was **never published at all**, must still be rejected by all four.
//! A fix that makes everything pass would be worse than the bug.

use std::path::Path;

use ml_dsa::Keypair;
use verifiable_log::{key_id_for, SigningKey};
use verifiable_log_builder::binaries::{BinaryRecord, Layer, Toolchain};
use verifiable_log_builder::source::{AccountKeyRow, CommitRow};
use verifiable_log_builder::{build_account_bundle, build_binaries_bundle, build_bundle};
use verifiable_log_serve::bundle::{Bundle, PublicKeyEntry};
use verifiable_log_serve::{account, group, layout, release, remote, DevServer, PublicKeyDoc};

const TS: u64 = 1_700_000_000_000;

/// The key the fixture trees are actually signed by — the one *retiring*.
fn old_key() -> SigningKey {
    SigningKey::from_seed(&[9u8; 32].into())
}

/// The successor the served key document names as active. It signs nothing in
/// these fixtures: that asymmetry IS the rotation window.
fn new_key() -> SigningKey {
    SigningKey::from_seed(&[7u8; 32].into())
}

/// A third key that was never published by this log — the control for "accepts
/// anything with a `keys` array".
fn stranger_key() -> SigningKey {
    SigningKey::from_seed(&[3u8; 32].into())
}

fn hex_of(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().encode())
}

fn key_id_of(key: &SigningKey) -> String {
    key_id_for(&key.verifying_key())
}

fn commit_rows() -> Vec<CommitRow> {
    vec![
        CommitRow {
            seq: 1,
            conversation_id: "conv-1".to_string(),
            generation: 0,
            epoch: 0,
            sender_id: "u-alice".to_string(),
            commit_sha256: hex::encode([0x11u8; 32]),
        },
        CommitRow {
            seq: 2,
            conversation_id: "conv-1".to_string(),
            generation: 0,
            epoch: 1,
            sender_id: "u-bob".to_string(),
            commit_sha256: hex::encode([0x12u8; 32]),
        },
    ]
}

fn account_rows() -> Vec<AccountKeyRow> {
    vec![
        AccountKeyRow {
            seq: 1,
            user_id: "u-alice".to_string(),
            identity_version: 1,
            account_id_pub: hex::encode([0xa1u8; 32]),
        },
        AccountKeyRow {
            seq: 2,
            user_id: "u-alice".to_string(),
            identity_version: 2,
            account_id_pub: hex::encode([0xa2u8; 32]),
        },
    ]
}

fn toolchain() -> Toolchain {
    Toolchain {
        rustc: "1.83.0".to_string(),
        node: "20.11.1".to_string(),
        pnpm: "9.1.0".to_string(),
        runner_image: "ubuntu-24.04@sha256:abc".to_string(),
        source_date_epoch: 1_700_000_000,
    }
}

fn binary_records() -> Vec<BinaryRecord> {
    vec![BinaryRecord {
        release_tag: "v1.3.0".to_string(),
        commit: "f".repeat(40),
        platform: "linux".to_string(),
        arch: "x86_64".to_string(),
        bundle: "appimage".to_string(),
        artifact_name: "pollis-v1.3.0-linux.appimage".to_string(),
        layer: Layer::Payload,
        payload_sha256: hex::encode([0x22u8; 32]),
        artifact_sha256: hex::encode([0x22u8; 32]),
        toolchain: toolchain(),
        provenance_uri: "cdn.pollis.com/releases/v1.3.0/linux.intoto.jsonl".to_string(),
    }]
}

fn to_serve_bundle(b: &verifiable_log_builder::Bundle) -> Bundle {
    serde_json::from_slice(&serde_json::to_vec(b).unwrap()).unwrap()
}

/// Generate all three trees, every head signed by `signer`.
fn generate_all_trees(root: &Path, signer: &SigningKey) {
    layout::generate(&to_serve_bundle(&build_bundle(&commit_rows(), signer, TS).unwrap()), root)
        .unwrap();
    layout::generate_account(
        &to_serve_bundle(&build_account_bundle(&account_rows(), signer, TS).unwrap()),
        root,
    )
    .unwrap();
    layout::generate_binaries(
        &to_serve_bundle(&build_binaries_bundle(&binary_records(), signer, TS).unwrap()),
        root,
    )
    .unwrap();
}

/// The three `public_key.json` documents a served log carries — one per tree.
fn public_key_paths(root: &Path) -> Vec<std::path::PathBuf> {
    let v1 = root.join("v1");
    vec![
        v1.join("public_key.json"),
        v1.join("account-keys").join("public_key.json"),
        v1.join("binaries").join("public_key.json"),
    ]
}

/// Rewrite every served `public_key.json` to announce `active` as the current
/// signer with `retiring` still inside an overlap window that ends at
/// `not_after`. This is the published state of a log mid-rotation; the heads on
/// disk are untouched and stay signed by whoever signed them.
fn publish_key_set(root: &Path, active: &SigningKey, retiring: &SigningKey, not_after: u64) {
    let doc = PublicKeyDoc {
        public_key: hex_of(active),
        keys: vec![
            PublicKeyEntry {
                key_id: key_id_of(active),
                algorithm: "ML-DSA-44".to_string(),
                public_key: hex_of(active),
                not_after: None,
            },
            PublicKeyEntry {
                key_id: key_id_of(retiring),
                algorithm: "ML-DSA-44".to_string(),
                public_key: hex_of(retiring),
                not_after: Some(not_after),
            },
        ],
    };
    for path in public_key_paths(root) {
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// A window that is comfortably open.
fn window_open() -> u64 {
    now_ms() + 90 * 24 * 60 * 60 * 1000
}

/// A window that closed a day ago.
fn window_closed() -> u64 {
    now_ms() - 24 * 60 * 60 * 1000
}

/// Serve a log whose heads are signed by the OLD key while the published key
/// document has already moved to the successor and lists the old one as
/// retiring-but-live. This is an honest log mid-rotation.
fn with_mid_rotation_log(f: impl FnOnce(&str)) {
    let dir = tempfile::tempdir().unwrap();
    generate_all_trees(dir.path(), &old_key());
    publish_key_set(dir.path(), &new_key(), &old_key(), window_open());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    f(&server.base_url());
    server.shutdown();
}

// ── The decisive case, one test per verify path ──────────────────────────────
//
// Split per path deliberately: before #875 the first of these passed and the
// other three failed, and that asymmetry IS the bug. A single combined test
// would hide which paths disagree.

/// The whole-log audit (`pollis-verify verify`) — already applied the overlap
/// set before #875, and must keep doing so.
#[test]
fn rotation_overlap_is_accepted_by_the_whole_log_audit() {
    with_mid_rotation_log(|base| {
        let report = remote::verify_remote(base).unwrap();
        assert!(
            report.ok,
            "the whole-log audit must accept a head inside the overlap window; checks: {:?}",
            report.checks
        );
    });
}

/// The per-group verifier (`pollis-verify group`, the static endpoint and the
/// live endpoint all end here).
#[test]
fn rotation_overlap_is_accepted_by_group_verify() {
    with_mid_rotation_log(|base| {
        let report = group::verify_group(base, "conv-1").unwrap();
        assert!(report.found);
        assert!(
            report.chain_valid,
            "group verify must accept a head inside the overlap window; violations: {:?}",
            report.violations
        );
    });
}

/// The per-account verifier — the one the shipped client runs for
/// `self_audit_account_key` / `audit_peer_account_key`. A false negative here is
/// a hard `AuditStatus::Alarm` against an honest log.
#[test]
fn rotation_overlap_is_accepted_by_account_verify() {
    with_mid_rotation_log(|base| {
        let report = account::verify_account(base, "u-alice").unwrap();
        assert!(report.found);
        assert!(
            report.chain_valid,
            "account verify must accept a head inside the overlap window; violations: {:?}",
            report.violations
        );
    });
}

/// The per-release verifier — the one the shipped client runs for
/// `verify_own_build`. A false negative here tells a user their genuine binary
/// is not one Pollis published.
#[test]
fn rotation_overlap_is_accepted_by_release_verify() {
    with_mid_rotation_log(|base| {
        let report = release::verify_release(base, "v1.3.0").unwrap();
        assert!(report.found);
        assert!(
            report.chain_valid,
            "release verify must accept a head inside the overlap window; violations: {:?}",
            report.violations
        );
    });
}

// ── The negatives: the overlap set must not become a blank cheque ────────────

/// The same log once the overlap window has CLOSED. Expiry is a deadline, not a
/// suggestion: every path must now refuse the head.
#[test]
fn a_head_signed_by_a_key_past_its_window_is_rejected_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    generate_all_trees(dir.path(), &old_key());
    publish_key_set(dir.path(), &new_key(), &old_key(), window_closed());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let base = server.base_url();

    let remote_report = remote::verify_remote(&base).unwrap();
    assert!(
        !remote_report.ok,
        "an expired overlap key must not verify a head in the whole-log audit"
    );

    let group_report = group::verify_group(&base, "conv-1").unwrap();
    assert!(
        !group_report.chain_valid,
        "an expired overlap key must not verify a head for group verify"
    );
    assert!(group_report
        .violations
        .iter()
        .any(|v| v.contains("signature") && v.contains("trustworthy")));

    let account_report = account::verify_account(&base, "u-alice").unwrap();
    assert!(
        !account_report.chain_valid,
        "an expired overlap key must not verify a head for account verify"
    );

    let release_report = release::verify_release(&base, "v1.3.0").unwrap();
    assert!(
        !release_report.chain_valid,
        "an expired overlap key must not verify a head for release verify"
    );

    server.shutdown();
}

/// A head signed by a key the log never published at all — the control that the
/// overlap machinery still anchors trust in the *published* set and has not
/// degenerated into "accept any signature".
#[test]
fn a_head_signed_by_an_unpublished_key_is_rejected_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    // Signed by a stranger...
    generate_all_trees(dir.path(), &stranger_key());
    // ...while the published set names two entirely different keys, both live.
    publish_key_set(dir.path(), &new_key(), &old_key(), window_open());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let base = server.base_url();

    assert!(
        !remote::verify_remote(&base).unwrap().ok,
        "an unpublished signer must fail the whole-log audit"
    );
    assert!(
        !group::verify_group(&base, "conv-1").unwrap().chain_valid,
        "an unpublished signer must fail group verify"
    );
    assert!(
        !account::verify_account(&base, "u-alice").unwrap().chain_valid,
        "an unpublished signer must fail account verify"
    );
    assert!(
        !release::verify_release(&base, "v1.3.0").unwrap().chain_valid,
        "an unpublished signer must fail release verify"
    );

    server.shutdown();
}

/// No rotation in flight: the ordinary single-key log must keep verifying
/// everywhere, unchanged. The overlap plumbing must not cost the common case.
#[test]
fn a_log_with_no_rotation_still_verifies_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    generate_all_trees(dir.path(), &old_key());

    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let base = server.base_url();

    assert!(remote::verify_remote(&base).unwrap().ok);
    assert!(group::verify_group(&base, "conv-1").unwrap().chain_valid);
    assert!(account::verify_account(&base, "u-alice").unwrap().chain_valid);
    assert!(release::verify_release(&base, "v1.3.0").unwrap().chain_valid);

    server.shutdown();
}

// ── Serde shape: a dropped field is how this happened ────────────────────────

/// The served key document must round-trip its `keys` array — entries, order and
/// `not_after` intact. A stand-in struct that keeps only `public_key` silently
/// discards the overlap set, which is precisely the failure #875 is about.
#[test]
fn public_key_doc_round_trips_its_key_set() {
    let doc = PublicKeyDoc {
        public_key: hex_of(&new_key()),
        keys: vec![
            PublicKeyEntry {
                key_id: key_id_of(&new_key()),
                algorithm: "ML-DSA-44".to_string(),
                public_key: hex_of(&new_key()),
                not_after: None,
            },
            PublicKeyEntry {
                key_id: key_id_of(&old_key()),
                algorithm: "ML-DSA-44".to_string(),
                public_key: hex_of(&old_key()),
                not_after: Some(1_800_000_000_000),
            },
        ],
    };

    let json = serde_json::to_string(&doc).unwrap();
    let back: PublicKeyDoc = serde_json::from_str(&json).unwrap();

    assert_eq!(back.public_key, doc.public_key);
    assert_eq!(back.keys, doc.keys);
    assert_eq!(back.keys[1].not_after, Some(1_800_000_000_000));

    // The active key carries no expiry and the retiring one does — the shape the
    // overlap window depends on.
    assert!(back.keys[0].not_after.is_none());
}

/// The generated artifact really does carry the key set on disk (not only in
/// memory), so a client that parses it sees the overlap.
#[test]
fn the_served_document_carries_the_key_set_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    generate_all_trees(dir.path(), &old_key());
    publish_key_set(dir.path(), &new_key(), &old_key(), window_open());

    for path in public_key_paths(dir.path()) {
        let doc: PublicKeyDoc =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc.keys.len(), 2, "{path:?} must publish both keys");
        assert_eq!(doc.public_key, hex_of(&new_key()));
        assert!(doc
            .keys
            .iter()
            .any(|k| k.public_key == hex_of(&old_key()) && k.not_after.is_some()));
    }
}

/// A document written before the key set existed (`keys` absent) still resolves
/// to its single `public_key` — the backward-compatibility path every shipped
/// verifier depends on.
#[test]
fn a_legacy_document_without_keys_resolves_to_its_single_key() {
    let json = format!(r#"{{"public_key":"{}"}}"#, hex_of(&old_key()));
    let doc: PublicKeyDoc = serde_json::from_str(&json).unwrap();

    assert!(doc.keys.is_empty());
    let active = doc.active_keys(now_ms());
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].public_key, hex_of(&old_key()));
    assert!(active[0].not_after.is_none());
}

/// A bundle rebuilt on the verification side from a served document must carry
/// the overlap set through to the verdict core, and hand back the same key set
/// when asked for the document again. This is the round trip the three
/// `verify_*_via` paths perform on every call.
#[test]
fn a_verification_side_bundle_preserves_the_overlap_set() {
    let doc = PublicKeyDoc {
        public_key: hex_of(&new_key()),
        keys: vec![
            PublicKeyEntry {
                key_id: key_id_of(&new_key()),
                algorithm: "ML-DSA-44".to_string(),
                public_key: hex_of(&new_key()),
                not_after: None,
            },
            PublicKeyEntry {
                key_id: key_id_of(&old_key()),
                algorithm: "ML-DSA-44".to_string(),
                public_key: hex_of(&old_key()),
                not_after: Some(window_open()),
            },
        ],
    };

    let overlap = doc.overlap_keys();
    assert_eq!(overlap.len(), 1, "only the non-active entries are the overlap");
    assert_eq!(overlap[0].public_key, hex_of(&old_key()));

    let bundle = Bundle {
        public_key: doc.public_key.clone(),
        retired_keys: overlap,
        sths: Vec::new(),
        entries: Vec::new(),
        enforce_unique: Vec::new(),
        inclusion: Vec::new(),
        consistency: Vec::new(),
    };

    // Both keys are candidates while the window is open...
    let candidates = bundle.key_candidates(now_ms());
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().any(|(id, _)| *id == key_id_of(&old_key())));
    assert!(candidates.iter().any(|(id, _)| *id == key_id_of(&new_key())));

    // ...and only the active one once it has closed.
    let after = bundle.key_candidates(window_open() + 1);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].0, key_id_of(&new_key()));
}
