//! #1579: the commit log is append-only over its PERSISTED leaves, not rebuilt
//! from the DS table the DS prunes.
#![cfg(feature = "db-source")]

use std::path::Path;
use std::process::{Command, Output};

use verifiable_log::{verify_consistency_proof, Entry, SigningKey, Sth, VerifiableLog};
use verifiable_log_builder::builder::Bundle;
use verifiable_log_builder::commit_leaves::{append_rows, check_reproduces};
use verifiable_log_builder::{build_bundle, source, BuilderError, CommitRow, PSEUDONYM_WINDOW_SIZE};

const TS: u64 = 1_700_000_000_000;
const KEY: [u8; 32] = [9u8; 32];
const KEY_HEX: &str = "0909090909090909090909090909090909090909090909090909090909090909";

fn signing_key() -> SigningKey {
    SigningKey::from_seed(&KEY.into())
}

/// One DS row as the reader returns it.
fn row(seq: i64, conv: &str, epoch: i64) -> CommitRow {
    CommitRow {
        seq,
        conversation_id: conv.to_string(),
        generation: 0,
        epoch,
        sender_id: "u-sender".to_string(),
        commit_sha256: source::sha256_hex(format!("{conv}-{epoch}-{seq}").as_bytes()),
    }
}

fn leaves_of(rows: &[CommitRow]) -> Vec<Entry> {
    rows.iter().map(|r| r.to_leaf().to_entry().unwrap()).collect()
}

/// The final (largest) STH of a bundle built over `leaves`.
fn head_over(leaves: &[Entry]) -> Sth {
    verifiable_log_builder::build_bundle_from_leaves(leaves, &signing_key(), TS)
        .unwrap()
        .sths
        .last()
        .unwrap()
        .clone()
}

async fn open(path: &Path) -> libsql::Connection {
    libsql::Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

async fn create(path: &Path) {
    open(path)
        .await
        .execute(
            "CREATE TABLE mls_commit_log (\
                seq INTEGER PRIMARY KEY AUTOINCREMENT, \
                conversation_id TEXT NOT NULL, \
                generation INTEGER NOT NULL DEFAULT 0, \
                epoch INTEGER NOT NULL, \
                sender_id TEXT NOT NULL, \
                commit_data BLOB NOT NULL, \
                created_at TEXT NOT NULL, \
                added_user_id TEXT, added_device_ids TEXT)",
            (),
        )
        .await
        .unwrap();
}

async fn insert(path: &Path, rows: &[(i64, &str, i64)]) {
    let conn = open(path).await;
    for (seq, conv, epoch) in rows {
        conn.execute(
            "INSERT INTO mls_commit_log \
                (seq, conversation_id, epoch, sender_id, commit_data, created_at) \
             VALUES (?1, ?2, ?3, 'u-sender', ?4, '2026-01-01T00:00:00Z')",
            libsql::params![*seq, conv.to_string(), *epoch, format!("{conv}-{epoch}").into_bytes()],
        )
        .await
        .unwrap();
    }
}

/// What the DS retention floor does (#539): delete rows below a seq.
async fn prune_below(path: &Path, seq: i64) {
    open(path)
        .await
        .execute("DELETE FROM mls_commit_log WHERE seq < ?1", libsql::params![seq])
        .await
        .unwrap();
}

async fn live_rows(path: &Path) -> Vec<CommitRow> {
    source::read_commit_log(&open(path).await).await.unwrap()
}

fn builder(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_builder"))
        .args(args)
        .env("VLOG_SIGNING_KEY", KEY_HEX)
        .env_remove("LOG_DB_URL")
        .env_remove("TURSO_DATABASE_URL")
        .env_remove("LOG_DB_AUTH_TOKEN")
        .env_remove("TURSO_AUTH_TOKEN")
        .output()
        .expect("failed to spawn builder")
}

fn append_cli(db: &Path, leaves: &Path, extra: &[&str]) -> Output {
    let mut args = vec![
        "append-commit-log",
        "--log-db",
        db.to_str().unwrap(),
        "--leaves",
        leaves.to_str().unwrap(),
        "--out",
        leaves.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    builder(&args)
}

fn build_cli(db: &Path, leaves: &Path, out: &Path) -> Bundle {
    let o = builder(&[
        "build",
        "--db",
        db.to_str().unwrap(),
        "--commit-leaves",
        leaves.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--timestamp",
        &TS.to_string(),
    ]);
    assert!(o.status.success(), "build failed: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
}

fn read_leaves(path: &Path) -> Vec<Entry> {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[tokio::test]
async fn pruning_rows_after_an_sth_keeps_consistency() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("log.db");
    let leaves = dir.path().join("commit-log-leaves.json");
    create(&db).await;
    insert(
        &db,
        &[(1, "a", 0), (2, "b", 0), (3, "a", 1), (4, "b", 1), (5, "a", 2), (6, "b", 2)],
    )
    .await;

    let o = append_cli(&db, &leaves, &["--seed-from-live"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let first = build_cli(&db, &leaves, &dir.path().join("b1.json"));
    let sth1 = first.sths.last().unwrap().clone();
    assert_eq!(sth1.tree_size, 6);
    let sth1_path = dir.path().join("sth1.json");
    std::fs::write(&sth1_path, serde_json::to_string(&sth1).unwrap()).unwrap();

    prune_below(&db, 5).await;
    insert(&db, &[(7, "a", 3), (8, "b", 3)]).await;

    // Rebuilding from the live table cannot reproduce STH 1 (#1579).
    let rebuilt = build_bundle(&live_rows(&db).await, &signing_key(), TS).unwrap();
    assert_eq!(rebuilt.entries.len(), 4);
    assert_ne!(
        rebuilt.entries[0], first.entries[0],
        "rebuilding from a pruned table puts different leaves at published positions"
    );

    let o = append_cli(&db, &leaves, &["--published-sth", sth1_path.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    let after = read_leaves(&leaves);
    assert_eq!(after.len(), 8);
    assert_eq!(&after[..6], &first.entries[..], "a prune must never change a published leaf");

    let second = build_cli(&db, &leaves, &dir.path().join("b2.json"));
    let sth2 = second.sths.last().unwrap().clone();
    assert_eq!(sth2.tree_size, 8);

    let mut log = VerifiableLog::new();
    for e in &second.entries {
        log.append(e.clone()).unwrap();
    }
    let proof = log.consistency_proof(6, 8).unwrap();
    assert!(verify_consistency_proof(&sth1, &sth2, &proof), "6 -> 8 must be consistent");

    let sth2_path = dir.path().join("sth2.json");
    std::fs::write(&sth2_path, serde_json::to_string(&sth2).unwrap()).unwrap();
    prune_below(&db, 8).await;
    let o = append_cli(&db, &leaves, &["--published-sth", sth2_path.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(read_leaves(&leaves), after);
}

#[tokio::test]
async fn the_first_run_seeds_the_persisted_leaves_from_the_live_table() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("log.db");
    let leaves = dir.path().join("commit-log-leaves.json");
    create(&db).await;
    insert(&db, &[(10, "a", 4), (11, "b", 0), (13, "a", 5)]).await;

    let o = append_cli(&db, &leaves, &["--seed-from-live"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(read_leaves(&leaves), leaves_of(&live_rows(&db).await));

    let o = append_cli(&db, &leaves, &["--seed-from-live"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("already exists"), "{}", stderr(&o));
}

#[tokio::test]
async fn seeding_from_recovered_leaves_appends_the_live_rows_above_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("log.db");
    let leaves = dir.path().join("commit-log-leaves.json");
    let recovered = dir.path().join("recovered-entries.json");
    create(&db).await;
    let old = leaves_of(&[row(1, "a", 0), row(2, "a", 1)]);
    std::fs::write(&recovered, serde_json::to_string(&old).unwrap()).unwrap();
    let published = dir.path().join("sth.json");
    std::fs::write(&published, serde_json::to_string(&head_over(&old)).unwrap()).unwrap();
    insert(&db, &[(3, "a", 2)]).await;

    let o = append_cli(
        &db,
        &leaves,
        &[
            "--seed-from",
            recovered.to_str().unwrap(),
            "--published-sth",
            published.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let got = read_leaves(&leaves);
    assert_eq!(got.len(), 3);
    assert_eq!(&got[..2], &old[..]);
}

#[tokio::test]
async fn a_missing_leaves_file_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("log.db");
    let leaves = dir.path().join("commit-log-leaves.json");
    create(&db).await;
    insert(&db, &[(1, "a", 0)]).await;

    let o = append_cli(&db, &leaves, &[]);
    assert!(!o.status.success(), "a missing log of record must never read as empty");
    assert!(stderr(&o).contains("does not exist"), "{}", stderr(&o));
    assert!(!leaves.exists(), "nothing may be written on refusal");

    let o = builder(&[
        "build",
        "--db",
        db.to_str().unwrap(),
        "--commit-leaves",
        leaves.to_str().unwrap(),
        "--out",
        dir.path().join("b.json").to_str().unwrap(),
        "--timestamp",
        "1",
    ]);
    assert!(!o.status.success());
}

#[tokio::test]
async fn a_shrunk_leaves_file_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("log.db");
    let leaves = dir.path().join("commit-log-leaves.json");
    create(&db).await;
    insert(&db, &[(1, "a", 0), (2, "a", 1), (3, "a", 2), (4, "a", 3)]).await;
    assert!(append_cli(&db, &leaves, &["--seed-from-live"]).status.success());
    let full = read_leaves(&leaves);
    let published = dir.path().join("sth.json");
    std::fs::write(&published, serde_json::to_string(&head_over(&full)).unwrap()).unwrap();

    std::fs::write(&leaves, serde_json::to_string(&full[..2]).unwrap()).unwrap();
    prune_below(&db, 5).await;

    let o = append_cli(&db, &leaves, &["--published-sth", published.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("shrank"), "{}", stderr(&o));
    assert_eq!(read_leaves(&leaves).len(), 2, "the refused run must not rewrite the file");
}

#[test]
fn a_rewritten_leaves_file_fails_closed() {
    let rows = vec![row(1, "a", 0), row(2, "b", 0), row(3, "a", 1)];
    let leaves = leaves_of(&rows);
    let published = head_over(&leaves);

    let mut tampered = leaves.clone();
    tampered[1] = row(2, "b", 7).to_leaf().to_entry().unwrap();
    let err = check_reproduces(&tampered, &published).unwrap_err();
    assert!(matches!(err, BuilderError::PersistedLeaves(_)), "got: {err}");
    assert!(err.to_string().contains("do not reproduce"), "got: {err}");

    let err = append_rows(&[], &rows, Some(&published)).unwrap_err();
    assert!(err.to_string().contains("shrank"), "got: {err}");
}

#[test]
fn a_live_row_that_changed_below_the_head_is_refused() {
    let leaves = leaves_of(&[row(1, "a", 0), row(2, "a", 1)]);
    let mut changed = row(2, "a", 1);
    changed.commit_sha256 = source::sha256_hex(b"different bytes");
    let err = append_rows(&leaves, &[row(1, "a", 0), changed], None).unwrap_err();
    assert!(err.to_string().contains("no longer matches"), "got: {err}");
}

#[test]
fn a_live_row_that_appears_below_the_head_is_refused() {
    let leaves = leaves_of(&[row(1, "a", 0), row(3, "a", 1)]);
    let err = append_rows(&leaves, &[row(2, "b", 0), row(3, "a", 1)], None).unwrap_err();
    assert!(err.to_string().contains("never logged"), "got: {err}");
}

#[test]
fn persisted_leaves_out_of_seq_order_are_refused() {
    let leaves = leaves_of(&[row(2, "a", 0), row(1, "b", 0)]);
    let err = append_rows(&leaves, &[], None).unwrap_err();
    assert!(err.to_string().contains("strictly increasing"), "got: {err}");
}

#[test]
fn a_fork_against_pruned_history_is_rejected() {
    let leaves = leaves_of(&[row(1, "a", 4), row(2, "a", 5)]);
    let err = append_rows(&leaves, &[row(3, "a", 5)], None).unwrap_err();
    assert!(matches!(err, BuilderError::Invariant(_) | BuilderError::Log(_)), "got: {err}");
}

#[test]
fn a_regression_across_a_window_against_pruned_history_is_rejected_at_full_strength() {
    // Window-0 rows are pruned; an epoch-5 fork in window 1 is caught by re-keying.
    let w = PSEUDONYM_WINDOW_SIZE as i64;
    let leaves = leaves_of(&[row(w - 2, "a", 4), row(w - 1, "a", 5)]);
    let err = append_rows(&leaves, &[row(w + 1, "a", 5)], None).unwrap_err();
    assert!(matches!(err, BuilderError::Invariant(_) | BuilderError::Log(_)), "got: {err}");

    let ok = append_rows(&leaves, &[row(w + 1, "a", 6)], None).unwrap();
    assert_eq!(ok.appended, 1);
    assert_eq!(ok.leaves.len(), 3);
}

fn build_cli_v2(db: &Path, leaves: &Path, out: &Path) -> Bundle {
    let o = builder(&[
        "build",
        "--db",
        db.to_str().unwrap(),
        "--commit-leaves",
        leaves.to_str().unwrap(),
        "--commit-tree",
        "commit-log-2",
        "--out",
        out.to_str().unwrap(),
        "--timestamp",
        &TS.to_string(),
    ]);
    assert!(o.status.success(), "build failed: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
}

#[tokio::test]
async fn commit_log_2_seeds_from_the_live_table_once_then_appends_under_its_own_context() {
    use verifiable_log::{STH_CONTEXT_COMMIT_LOG, STH_CONTEXT_COMMIT_LOG_2};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("log.db");
    let leaves = dir.path().join("commit-log-2-leaves.json");
    create(&db).await;
    insert(&db, &[(40, "a", 7), (41, "b", 3), (42, "a", 8)]).await;

    // Seed with no published head to reproduce.
    let o = append_cli(&db, &leaves, &["--seed-from-live"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let first = build_cli_v2(&db, &leaves, &dir.path().join("b1.json"));
    let sth1 = first.sths.last().unwrap().clone();
    assert_eq!(sth1.tree_size, 3);
    let vk = ml_dsa::Keypair::verifying_key(&signing_key());
    assert!(sth1.verify_with_context(&vk, STH_CONTEXT_COMMIT_LOG_2));
    assert!(
        !sth1.verify_with_context(&vk, STH_CONTEXT_COMMIT_LOG),
        "a successor head must never verify as a head of the retired tree"
    );

    let o = append_cli(&db, &leaves, &["--seed-from-live"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("already exists"), "{}", stderr(&o));

    let sth1_path = dir.path().join("sth1.json");
    std::fs::write(&sth1_path, serde_json::to_string(&sth1).unwrap()).unwrap();
    prune_below(&db, 42).await;
    insert(&db, &[(43, "b", 4), (44, "a", 9)]).await;
    let o = append_cli(&db, &leaves, &["--published-sth", sth1_path.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stderr(&o));
    let second = build_cli_v2(&db, &leaves, &dir.path().join("b2.json"));
    assert_eq!(second.entries.len(), 5);
    assert_eq!(&second.entries[..3], &first.entries[..]);
    let sth2 = second.sths.last().unwrap();
    assert!(sth2.verify_with_context(&vk, STH_CONTEXT_COMMIT_LOG_2));

    let log = verifiable_log_builder::commit_leaves::replay_public(&read_leaves(&leaves)).unwrap();
    let proof = log.consistency_proof(3, 5).unwrap();
    assert!(verify_consistency_proof(&sth1, sth2, &proof));
    let v1 = build_cli(&db, &leaves, &dir.path().join("b-v1.json"));
    assert_eq!(v1.sths.last().unwrap().root_hash, sth2.root_hash);
    assert_ne!(v1.sths.last().unwrap().signature, sth2.signature);
}

#[test]
fn commit_log_2_without_persisted_leaves_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let o = builder(&[
        "build",
        "--db",
        dir.path().join("x.db").to_str().unwrap(),
        "--commit-tree",
        "commit-log-2",
        "--out",
        dir.path().join("b.json").to_str().unwrap(),
        "--timestamp",
        "1",
    ]);
    assert!(!o.status.success(), "the successor is never built from the DS table");
}
