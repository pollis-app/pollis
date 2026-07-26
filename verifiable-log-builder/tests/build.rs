//! Deterministic gate suite for the commit-log builder.
//!
//! Seeds a LOCAL libSQL/SQLite fixture file (no network, no prod DB), builds a
//! bundle, and verifies it through the slice-1 monitor path (the public
//! `verifiable_log::proof::verify_*` / `Sth::verify` functions, replayed exactly
//! as the `monitor` CLI does). Also exercises fork/regression rejection,
//! tamper detection, and keygen round-trip.

use ed25519_dalek::SigningKey;
use verifiable_log::{
    is_equivocation, verify_consistency_proof, verify_inclusion_proof, verifying_key_from_hex,
    UniqueDataInvariant, VerifiableLog,
};
use verifiable_log_builder::builder::Bundle;
use verifiable_log_builder::{build_bundle, keys, source};

const TS: u64 = 1_700_000_000_000;
// Deterministic dev key (custody is a later slice).
const KEY: [u8; 32] = [9u8; 32];

/// A synthetic commit row to seed the fixture DB with.
struct Row {
    seq: i64,
    conv: &'static str,
    generation: i64,
    epoch: i64,
    sender: &'static str,
    data: Vec<u8>,
}

fn row(seq: i64, conv: &'static str, epoch: i64, data: &str) -> Row {
    row_at(seq, conv, 0, epoch, data)
}

fn row_at(seq: i64, conv: &'static str, generation: i64, epoch: i64, data: &str) -> Row {
    Row {
        seq,
        conv,
        generation,
        epoch,
        sender: "u-sender",
        data: data.as_bytes().to_vec(),
    }
}

/// Create a fresh local libSQL file with the **pre-#454-P4** `mls_commit_log`
/// shape — no `generation` column — and the given rows. No UNIQUE index, so
/// fork/regression rows can be injected exactly as a buggy/malicious server
/// might have written them.
///
/// Most tests seed this shape deliberately: the builder deploys independently of
/// the Delivery Service that migrates the log DB, so reading a log DB that
/// predates `000004_commit_generation.sql` is a real operating state, not a
/// legacy curiosity.
async fn seed_db(path: &std::path::Path, rows: &[Row]) {
    seed(path, rows, false).await
}

/// [`seed_db`] with the `generation` column migration `000004` adds.
async fn seed_db_with_generation(path: &std::path::Path, rows: &[Row]) {
    seed(path, rows, true).await
}

async fn seed(path: &std::path::Path, rows: &[Row], with_generation: bool) {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let generation_col = if with_generation {
        "generation INTEGER NOT NULL DEFAULT 0, "
    } else {
        ""
    };
    conn.execute(
        &format!(
            "CREATE TABLE mls_commit_log (\
                seq INTEGER PRIMARY KEY AUTOINCREMENT, \
                conversation_id TEXT NOT NULL, \
                {generation_col}\
                epoch INTEGER NOT NULL, \
                sender_id TEXT NOT NULL, \
                commit_data BLOB NOT NULL, \
                created_at TEXT NOT NULL, \
                added_user_id TEXT, added_device_ids TEXT)"
        ),
        (),
    )
    .await
    .unwrap();
    for r in rows {
        let sql = if with_generation {
            "INSERT INTO mls_commit_log \
                (seq, conversation_id, epoch, sender_id, commit_data, created_at, generation) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        } else {
            "INSERT INTO mls_commit_log \
                (seq, conversation_id, epoch, sender_id, commit_data, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        };
        let mut params = vec![
            libsql::Value::from(r.seq),
            libsql::Value::from(r.conv.to_string()),
            libsql::Value::from(r.epoch),
            libsql::Value::from(r.sender.to_string()),
            libsql::Value::from(r.data.clone()),
            libsql::Value::from("2026-01-01T00:00:00Z".to_string()),
        ];
        if with_generation {
            params.push(libsql::Value::from(r.generation));
        }
        conn.execute(sql, params).await.unwrap();
    }
}

/// Faithful in-process re-implementation of `monitor verify`, built ONLY on the
/// public slice-1 verification functions — the same checks the CLI runs on the
/// emitted file: STH signatures, equivocation, entry replay through tenant
/// invariants + per-STH root agreement, inclusion proofs, consistency proofs.
fn monitor_verify(bundle: &Bundle) -> bool {
    let vk = match verifying_key_from_hex(&bundle.public_key) {
        Ok(k) => k,
        Err(_) => return false,
    };

    for sth in &bundle.sths {
        if !sth.verify(&vk) {
            return false;
        }
    }
    for i in 0..bundle.sths.len() {
        for j in (i + 1)..bundle.sths.len() {
            if is_equivocation(&bundle.sths[i], &bundle.sths[j]) {
                return false;
            }
        }
    }
    if !bundle.entries.is_empty() {
        let mut log = VerifiableLog::new();
        for tenant in &bundle.enforce_unique {
            log.register_invariant(tenant.clone(), Box::new(UniqueDataInvariant));
        }
        for entry in &bundle.entries {
            if log.append(entry.clone()).is_err() {
                return false;
            }
        }
        for sth in &bundle.sths {
            let size = sth.tree_size as usize;
            let root = match log.root_at(size) {
                Ok(r) => r,
                Err(_) => return false,
            };
            match sth.root_bytes() {
                Ok(r) if r == root => {}
                _ => return false,
            }
        }
    }
    for check in &bundle.inclusion {
        let sth = match bundle.sths.get(check.sth_index) {
            Some(s) => s,
            None => return false,
        };
        if !verify_inclusion_proof(&check.entry, &check.proof, sth) {
            return false;
        }
    }
    for check in &bundle.consistency {
        let old = bundle.sths.get(check.old_index);
        let new = bundle.sths.get(check.new_index);
        match (old, new) {
            (Some(o), Some(n)) => {
                if !verify_consistency_proof(o, n, &check.proof) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&KEY)
}

#[tokio::test]
async fn valid_bundle_verifies_through_monitor_path() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("commits.db");
    // Two conversations, sequential epochs, interleaved in seq order.
    let rows = vec![
        row(1, "conv-a", 0, "a-commit-0"),
        row(2, "conv-b", 0, "b-commit-0"),
        row(3, "conv-a", 1, "a-commit-1"),
        row(4, "conv-b", 1, "b-commit-1"),
        row(5, "conv-a", 2, "a-commit-2"),
        row(6, "conv-b", 2, "b-commit-2"),
    ];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_commit_log(&conn).await.unwrap();
    assert_eq!(read.len(), 6);
    // The raw blob is never surfaced — only its sha256 hex.
    assert_eq!(read[0].commit_sha256, source::sha256_hex(b"a-commit-0"));

    let bundle = build_bundle(&read, &signing_key(), TS).unwrap();

    // Shape: an inclusion proof per entry, a midpoint + final STH, one
    // consistency proof.
    assert_eq!(bundle.entries.len(), 6);
    assert_eq!(bundle.inclusion.len(), 6);
    assert_eq!(bundle.sths.len(), 2);
    assert_eq!(bundle.consistency.len(), 1);
    assert_eq!(bundle.enforce_unique, vec!["mls-commit-log".to_string()]);

    assert!(monitor_verify(&bundle), "freshly built bundle must verify");

    // Round-trips through the on-disk JSON shape and still verifies.
    let json = serde_json::to_string_pretty(&bundle).unwrap();
    let reparsed: Bundle = serde_json::from_str(&json).unwrap();
    assert!(monitor_verify(&reparsed), "bundle must verify after JSON round-trip");
}

#[tokio::test]
async fn fork_row_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("fork.db");
    // Same (conversation_id, epoch) twice with different commit bytes.
    let rows = vec![
        row(1, "conv-a", 0, "a-commit-0"),
        row(2, "conv-a", 0, "a-commit-0-EVIL"),
    ];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_commit_log(&conn).await.unwrap();

    let err = build_bundle(&read, &signing_key(), TS).unwrap_err();
    assert!(
        err.to_string().contains("fork"),
        "expected a fork violation, got: {err}"
    );
}

#[tokio::test]
async fn epoch_regression_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("regress.db");
    // conv-a goes 0 -> 5 -> 3 (backwards) in seq order.
    let rows = vec![
        row(1, "conv-a", 0, "a-commit-0"),
        row(2, "conv-a", 5, "a-commit-5"),
        row(3, "conv-a", 3, "a-commit-3"),
    ];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_commit_log(&conn).await.unwrap();

    let err = build_bundle(&read, &signing_key(), TS).unwrap_err();
    assert!(
        err.to_string().contains("regression"),
        "expected an epoch regression violation, got: {err}"
    );
}

/// A log DB that predates migration `000004_commit_generation.sql` reads as
/// generation 0 throughout — which is not a lossy fallback but the truth: such a
/// DB cannot contain a migrated conversation, because nothing could have written
/// one. The builder must not require a schema upgrade to keep running.
#[tokio::test]
async fn a_pre_p4_log_db_reads_as_the_classic_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("pre-p4.db");
    let rows = vec![row(1, "conv-a", 0, "a-commit-0"), row(2, "conv-a", 1, "a-commit-1")];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_commit_log(&conn).await.unwrap();
    assert!(read.iter().all(|r| r.generation == 0));
    assert!(read.iter().all(|r| r.to_leaf().generation == 0));
}

/// The #454 P4 acceptance criterion for the transparency log: a conversation
/// that migrates to the hybrid suite restarts at MLS epoch 0 in a successor
/// lineage, and the builder must publish that as valid history. Before the
/// monotone key widened to `(conversation, generation, epoch)`, this build
/// aborted — every honest migration would have taken the transparency log down.
#[tokio::test]
async fn a_suite_migration_builds_and_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("migrate.db");
    let rows = vec![
        row_at(1, "conv-a", 0, 0, "a-g0-e0"),
        row_at(2, "conv-a", 0, 1, "a-g0-e1"),
        // The migration: same conversation, successor lineage, epoch restarts.
        row_at(3, "conv-a", 1, 0, "a-g1-e0"),
        row_at(4, "conv-a", 1, 1, "a-g1-e1"),
        // A conversation that never migrated is untouched by any of this.
        row_at(5, "conv-b", 0, 0, "b-g0-e0"),
    ];
    seed_db_with_generation(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_commit_log(&conn).await.unwrap();
    assert_eq!(read.iter().map(|r| r.generation).collect::<Vec<_>>(), vec![0, 0, 1, 1, 0]);

    let bundle = build_bundle(&read, &signing_key(), TS).unwrap();
    assert_eq!(bundle.entries.len(), 5);
    assert!(monitor_verify(&bundle), "a migrated history must verify");
}

/// Widening the key must not have bought the server a way to launder a fork as a
/// migration: bumping the generation while picking an arbitrary epoch is caught
/// because a successor lineage may only ever *open*, and it opens at epoch 0.
#[tokio::test]
async fn a_lineage_opening_mid_stream_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bad-open.db");
    let rows = vec![
        row_at(1, "conv-a", 0, 0, "a-g0-e0"),
        row_at(2, "conv-a", 0, 1, "a-g0-e1"),
        row_at(3, "conv-a", 1, 7, "a-g1-e7"),
    ];
    seed_db_with_generation(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_commit_log(&conn).await.unwrap();

    let err = build_bundle(&read, &signing_key(), TS).unwrap_err();
    assert!(
        err.to_string().contains("must start at epoch 0"),
        "expected a lineage-opening violation, got: {err}"
    );
}

/// Once a conversation has migrated, its old lineage is closed — every honest
/// device has deleted that suite's key material. A commit appearing there is the
/// server rewriting history, and it stays a regression under the widened key.
#[tokio::test]
async fn a_commit_on_a_retired_lineage_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("retired.db");
    let rows = vec![
        row_at(1, "conv-a", 0, 5, "a-g0-e5"),
        row_at(2, "conv-a", 1, 0, "a-g1-e0"),
        row_at(3, "conv-a", 0, 6, "a-g0-e6"),
    ];
    seed_db_with_generation(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_commit_log(&conn).await.unwrap();

    let err = build_bundle(&read, &signing_key(), TS).unwrap_err();
    assert!(
        err.to_string().contains("regression"),
        "expected a retired-lineage regression, got: {err}"
    );
}

#[tokio::test]
async fn tampered_entry_fails_verification() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("tamper.db");
    let rows = vec![
        row(1, "conv-a", 0, "a-commit-0"),
        row(2, "conv-a", 1, "a-commit-1"),
        row(3, "conv-a", 2, "a-commit-2"),
    ];
    seed_db(&db_path, &rows).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_commit_log(&conn).await.unwrap();
    let mut bundle = build_bundle(&read, &signing_key(), TS).unwrap();
    assert!(monitor_verify(&bundle));

    // Flip a byte in one committed entry's leaf data: its leaf hash no longer
    // matches the STH root the inclusion proof reconstructs.
    bundle.entries[0].data[0] ^= 0xff;
    assert!(
        !monitor_verify(&bundle),
        "a tampered entry must fail the monitor"
    );
}

#[tokio::test]
async fn empty_commit_log_yields_empty_commit_log_error_not_no_db_source() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("empty.db");
    // A connectable DB with the right table but ZERO rows — the freshly-cut-over
    // log DB case. The DB is fine; it is just empty.
    seed_db(&db_path, &[]).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_commit_log(&conn).await.unwrap();
    assert!(read.is_empty());

    let err = source::ensure_non_empty(&read).unwrap_err();
    // It must be the empty-table signal, NOT the missing-`--db` signal.
    assert!(
        matches!(err, verifiable_log_builder::BuilderError::EmptyCommitLog),
        "expected EmptyCommitLog, got: {err}"
    );
    let msg = err.to_string();
    assert!(msg.contains("db connected OK"), "got: {msg}");
    assert!(
        !msg.contains("--db") && !msg.contains("TURSO_DATABASE_URL"),
        "must not look like a missing-db-source error: {msg}"
    );
}

#[tokio::test]
async fn empty_commit_log_still_builds_a_valid_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("empty.db");
    seed_db(&db_path, &[]).await;

    let conn = source::connect(db_path.to_str().unwrap()).await.unwrap();
    let read = source::read_commit_log(&conn).await.unwrap();

    // A zero-row commit log produces a valid empty bundle (one STH over tree
    // size 0, no entries). This is what keeps `serve generate --bundle ...`
    // working when the commit log happens to be empty: the file always exists
    // and parses, rather than the builder aborting and leaving it missing.
    let bundle = build_bundle(&read, &signing_key(), TS).unwrap();
    assert_eq!(bundle.entries.len(), 0);
    assert_eq!(bundle.inclusion.len(), 0);
    assert_eq!(bundle.consistency.len(), 0);
    assert_eq!(bundle.sths.len(), 1);
    assert_eq!(bundle.sths[0].tree_size, 0);
    assert!(monitor_verify(&bundle), "empty bundle must still verify");

    // And it round-trips through the on-disk JSON shape `serve generate` reads.
    let json = serde_json::to_string_pretty(&bundle).unwrap();
    let reparsed: Bundle = serde_json::from_str(&json).unwrap();
    assert!(monitor_verify(&reparsed));
}

#[test]
fn keygen_output_roundtrips() {
    let g = keys::generate();
    let secret = hex::decode(&g.secret_hex).unwrap();
    let arr: [u8; 32] = secret.as_slice().try_into().unwrap();
    let signing_key = SigningKey::from_bytes(&arr);

    // An STH signed by the emitted private key verifies against the emitted
    // public key.
    let root = [3u8; 32];
    let sth = verifiable_log::Sth::create(&signing_key, 1, root, TS);
    let vk = verifying_key_from_hex(&g.public_hex).unwrap();
    assert!(sth.verify(&vk), "emitted public key must verify the STH");
}
