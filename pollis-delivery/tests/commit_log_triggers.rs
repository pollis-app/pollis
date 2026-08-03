//! Schema-layer enforcement of invariant I1 (issue #691, phase 1 of
//! docs/backend-core-invariants.md). These tests attempt each I1 violation **by
//! direct SQL, bypassing the Rust `pollis_delivery::commit` layer entirely** —
//! that is the only way to prove the DATABASE rejects the invalid state, rather
//! than the protocol-layer CAS insert. A test that went through `submit_commit`
//! would only re-prove the CAS.
//!
//! The schema under test is the REAL one: the log-DB migrations are `include_str!`
//! d straight from `pollis-core/src/db/migrations-log/` and applied with libsql's
//! native `execute_batch` (which parses `CREATE TRIGGER ... BEGIN ...; END`
//! correctly), so what these tests exercise is byte-for-byte what ships.
//!
//! The counterpart obligation — "a trigger that fires on legitimate traffic is
//! worse than no trigger" — is discharged by `legitimate_traffic_*`: the real
//! head-append and retention-prune paths run against the triggered schema and are
//! asserted to succeed. The full integration suite (flows / tui) piles on more,
//! since it applies migration 000005 via POST_BASELINE_LOG_MIGRATIONS and then
//! runs every real submit + prune through it.

use pollis_delivery::commit::{
    delete_commits_below, delete_generations_below, submit_commit, SubmitBody, SubmitResponse,
};
use pollis_delivery::db::Db;

// The real log-DB schema, in version order (mirrors POST_BASELINE_LOG_MIGRATIONS
// + LOG_DB_SCHEMA). 000005 is the trigger migration under test.
const LOG_MIGRATIONS: &[&str] = &[
    include_str!("../../pollis-core/src/db/migrations-log/000001_commit_log_db.sql"),
    include_str!("../../pollis-core/src/db/migrations-log/000002_mls_welcome_unique_recipient.sql"),
    include_str!("../../pollis-core/src/db/migrations-log/000003_mls_commit_since.sql"),
    include_str!("../../pollis-core/src/db/migrations-log/000004_commit_generation.sql"),
    include_str!("../../pollis-core/src/db/migrations-log/000005_mls_commit_log_triggers.sql"),
];

/// A fresh local log DB with the real migrations (triggers included) applied.
async fn fresh_log() -> Db {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("log.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    let conn = db.conn().unwrap();
    for sql in LOG_MIGRATIONS {
        // Native batch: parses trigger bodies correctly, unlike a naive `;` split.
        conn.execute_batch(sql).await.expect("apply migration");
    }
    db
}

/// Insert a commit by DIRECT SQL — the bypass. Returns the raw libsql result so a
/// test can assert the trigger rejected it.
async fn raw_insert(db: &Db, conv: &str, gen: i64, epoch: i64) -> Result<u64, libsql::Error> {
    db.conn()
        .unwrap()
        .execute(
            "INSERT INTO mls_commit_log (conversation_id, generation, epoch, sender_id, commit_data) \
             VALUES (?1, ?2, ?3, 'sender', ?4)",
            libsql::params![conv, gen, epoch, vec![epoch as u8]],
        )
        .await
}

/// Seed a contiguous chain `0..=up_to` in `gen` via direct SQL. Every insert is
/// at the head, so the forward-gap trigger passes — this doubles as evidence the
/// trigger does not fire on legitimate append.
async fn seed(db: &Db, conv: &str, gen: i64, up_to: i64) {
    for e in 0..=up_to {
        raw_insert(db, conv, gen, e).await.expect("legit head append");
    }
}

async fn epochs(db: &Db, conv: &str, gen: i64) -> Vec<i64> {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT epoch FROM mls_commit_log WHERE conversation_id = ?1 AND generation = ?2 \
             ORDER BY epoch ASC",
            libsql::params![conv, gen],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<i64>(0).unwrap());
    }
    out
}

// ── I1: no forward gap (kills F1) ────────────────────────────────────────────

#[tokio::test]
async fn insert_forward_gap_is_rejected() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 2).await; // head epoch = 3 (MAX=2, so next is 3)

    // A forward jump to epoch 4 (head is 3) skips epoch 3 → the F1 gap. Rejected.
    let gap = raw_insert(&db, "c1", 0, 4).await;
    assert!(gap.is_err(), "forward-gap insert must be rejected by the trigger");
    assert!(
        gap.unwrap_err().to_string().contains("forward gap"),
        "error should name the I1 forward-gap violation"
    );

    // The database was not mutated by the rejected insert.
    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1, 2]);

    // The head insert (epoch 3) is legitimate and still accepted.
    raw_insert(&db, "c1", 0, 3).await.expect("head append accepted");
    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1, 2, 3]);
}

#[tokio::test]
async fn first_insert_must_be_epoch_zero() {
    let db = fresh_log().await;
    // Empty lineage: head is 0, so epoch 5 is a forward gap from nothing.
    assert!(
        raw_insert(&db, "c1", 0, 5).await.is_err(),
        "first commit at epoch 5 is a forward gap and must be rejected"
    );
    // Epoch 0 opens the lineage — accepted.
    raw_insert(&db, "c1", 0, 0).await.expect("opening epoch 0 accepted");
    // A successor generation legitimately opens at epoch 0 (empty NEW.generation).
    raw_insert(&db, "c1", 1, 0).await.expect("generation-open epoch 0 accepted");
    assert_eq!(epochs(&db, "c1", 1).await, vec![0]);
}

// ── I1: history is immutable (kills the rewrite half of F2) ───────────────────

#[tokio::test]
async fn update_of_history_is_rejected() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 2).await;
    let conn = db.conn().unwrap();

    // Re-point an applied commit to a different epoch — history rewrite. Rejected.
    let repoint = conn
        .execute(
            "UPDATE mls_commit_log SET epoch = 9 WHERE conversation_id = 'c1' AND epoch = 1",
            (),
        )
        .await;
    assert!(repoint.is_err(), "changing epoch must be rejected");

    // Edit the committed bytes — tampering. Rejected.
    let tamper = conn
        .execute(
            "UPDATE mls_commit_log SET commit_data = X'00' WHERE conversation_id = 'c1' AND epoch = 1",
            (),
        )
        .await;
    assert!(tamper.is_err(), "changing commit_data must be rejected");

    // The chain is untouched by either rejected update.
    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1, 2]);

    // A non-identity metadata update is NOT guarded (nothing legitimately relies
    // on it being frozen, and the DS never issues it anyway).
    conn.execute(
        "UPDATE mls_commit_log SET added_device_ids = 'd1' WHERE conversation_id = 'c1' AND epoch = 1",
        (),
    )
    .await
    .expect("benign metadata update is allowed");
}

// ── I1: the head cannot be deleted WHILE EARLIER COMMITS REMAIN (the ELECTRON
// half of F2). The guard is narrowed (#691 review) to exactly the harm it
// prevents: a head deleted out from under members who applied it while earlier
// commits are still present (they see a chain that regressed and cannot
// reconcile). Deleting the head when it is the LAST row of the conversation is a
// different operation — whole-conversation removal, trivially gapless — and is
// allowed; the two tests below (`delete_head_when_only_row_is_allowed`,
// `bulk_wipe_empties_the_table`) pin that boundary.

#[tokio::test]
async fn delete_of_head_is_rejected() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 3).await; // epochs 0..=3, head commit at epoch 3

    // Deleting the head commit while epochs 0..2 remain is the ELECTRON shape —
    // it wedges every current member. Rejected.
    let del_head = db
        .conn()
        .unwrap()
        .execute(
            "DELETE FROM mls_commit_log WHERE conversation_id = 'c1' AND generation = 0 AND epoch = 3",
            (),
        )
        .await;
    assert!(del_head.is_err(), "deleting the head commit must be rejected");
    assert!(
        del_head.unwrap_err().to_string().contains("head"),
        "error should name the I1 head-protection violation"
    );
    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1, 2, 3]);

    // A below-head (older) commit may be deleted — that is legitimate retention.
    db.conn()
        .unwrap()
        .execute(
            "DELETE FROM mls_commit_log WHERE conversation_id = 'c1' AND generation = 0 AND epoch = 0",
            (),
        )
        .await
        .expect("deleting an old commit is allowed");
    assert_eq!(epochs(&db, "c1", 0).await, vec![1, 2, 3]);
}

#[tokio::test]
async fn delete_of_generation_head_is_rejected() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 2).await; // closed lineage gen 0
    seed(&db, "c1", 1, 1).await; // head lineage gen 1, head epoch 1

    // The head of the HEAD generation is protected.
    assert!(
        db.conn()
            .unwrap()
            .execute(
                "DELETE FROM mls_commit_log WHERE conversation_id = 'c1' AND generation = 1 AND epoch = 1",
                (),
            )
            .await
            .is_err(),
        "deleting the head-generation head must be rejected"
    );

    // But a CLOSED generation (gen 0) is not the head generation, so retiring it
    // wholesale — including its own local head — is allowed.
    db.conn()
        .unwrap()
        .execute(
            "DELETE FROM mls_commit_log WHERE conversation_id = 'c1' AND generation < 1",
            (),
        )
        .await
        .expect("retiring a closed generation is allowed");
    assert_eq!(epochs(&db, "c1", 0).await, Vec::<i64>::new());
    assert_eq!(epochs(&db, "c1", 1).await, vec![0, 1]);
}

/// The narrowed boundary, half one: deleting the head when it is the ONLY row of
/// the conversation is allowed — the chain becomes empty (trivially gapless) and
/// there is nothing left to be inconsistent with. This is the case the original
/// guard wrongly blocked, which took down the whole flows suite (every scenario's
/// `wipe_remote` issues a bare `DELETE FROM mls_commit_log`).
#[tokio::test]
async fn delete_head_when_only_row_is_allowed() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 0).await; // a single commit — head-and-only-row

    db.conn()
        .unwrap()
        .execute(
            "DELETE FROM mls_commit_log WHERE conversation_id = 'c1' AND generation = 0 AND epoch = 0",
            (),
        )
        .await
        .expect("deleting the last remaining commit is allowed (empty chain is gapless)");
    assert_eq!(epochs(&db, "c1", 0).await, Vec::<i64>::new());
}

/// The narrowed boundary, half two: the exact operation that took down the flows
/// suite — a bare, unqualified `DELETE FROM mls_commit_log` (the test-harness
/// `wipe_remote` reset) — must now succeed and empty the table. `seq` is the
/// AUTOINCREMENT PK, so within each conversation the head has the highest `seq`
/// and SQLite's ascending-rowid delete order visits it LAST, by which point its
/// siblings are gone and the guard's `EXISTS (other row)` clause is false.
/// Conversations are interleaved here so the head of one is NOT the globally-last
/// row — proving the per-conversation ordering, not a global fluke, is what makes
/// this safe.
#[tokio::test]
async fn bulk_wipe_empties_the_table() {
    let db = fresh_log().await;
    let conn = db.conn().unwrap();
    // Interleave two conversations' appends so their seqs alternate; each still
    // appends at its own head, so the forward-gap trigger never fires.
    for e in 0..=3 {
        for c in ["c1", "c2"] {
            raw_insert(&db, c, 0, e).await.expect("interleaved head append");
        }
    }
    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1, 2, 3]);
    assert_eq!(epochs(&db, "c2", 0).await, vec![0, 1, 2, 3]);

    // The bare wipe the harness issues. Must not trip the head guard on any row.
    conn.execute("DELETE FROM mls_commit_log", ())
        .await
        .expect("bare DELETE FROM mls_commit_log must empty the table, not abort on the head");

    let count: i64 = {
        let mut rows = conn.query("SELECT COUNT(*) FROM mls_commit_log", ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    };
    assert_eq!(count, 0, "the wipe left rows behind — the head guard fired mid-wipe");
}

/// The same erasure reached the other way: a per-row bottom-up prune of a single
/// conversation to empty. Every step deletes the current lowest epoch; when only
/// the head remains it is also the only row, so the final delete is allowed. This
/// is the shape a future delete-account / erase-conversation path will use.
#[tokio::test]
async fn bottom_up_prune_to_empty_succeeds() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 3).await; // epochs 0..=3
    let conn = db.conn().unwrap();

    for e in 0..=3 {
        conn.execute(
            "DELETE FROM mls_commit_log WHERE conversation_id = 'c1' AND generation = 0 AND epoch = ?1",
            libsql::params![e],
        )
        .await
        .unwrap_or_else(|err| panic!("bottom-up delete of epoch {e} must succeed, got {err}"));
    }
    assert_eq!(epochs(&db, "c1", 0).await, Vec::<i64>::new());
}

/// The #680 fault-injection harness (`src-tauri/tests/flows/harness.rs::
/// corrupt_commit_row`) used to corrupt an interior commit's bytes with a straight
/// `UPDATE ... SET commit_data`, which `trg_mls_commit_log_immutable` now aborts.
/// The immutability guard is correct and must not be weakened, so the harness was
/// rewritten to reproduce the same end state — one interior row with a stub payload,
/// `seq`/`epoch` and the head unchanged — via DELETE-then-INSERT. This test proves
/// that replacement shape passes BOTH triggers against the real triggered schema,
/// so the mechanism the flows suite depends on is covered by a test that runs here
/// (the flows suite cannot run in this sandbox).
#[tokio::test]
async fn interior_delete_then_reinsert_corruption_passes() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 3).await; // epochs 0..=3, head at epoch 3
    let conn = db.conn().unwrap();

    // Snapshot the interior row (epoch 1) — the harness preserves seq so the row
    // keeps its place in the seq-ordered catch-up replay.
    let seq: i64 = {
        let mut rows = conn
            .query(
                "SELECT seq FROM mls_commit_log WHERE conversation_id = 'c1' AND generation = 0 AND epoch = 1",
                (),
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    };

    // DELETE the interior row: the head guard only protects epoch 3, so this passes.
    let deleted = conn
        .execute("DELETE FROM mls_commit_log WHERE seq = ?1", libsql::params![seq])
        .await
        .expect("interior delete passes the head guard");
    assert_eq!(deleted, 1);

    // Re-INSERT at the same seq/epoch with a 2-byte stub payload: epoch 1 is below
    // the head (3), so the forward-gap guard passes; it is an INSERT, so the
    // immutability guard (UPDATE-only) never applies.
    conn.execute(
        "INSERT INTO mls_commit_log (seq, conversation_id, generation, epoch, sender_id, commit_data) \
         VALUES (?1, 'c1', 0, 1, 'sender', X'0001')",
        libsql::params![seq],
    )
    .await
    .expect("interior re-insert passes both triggers");

    // Same observable state: chain intact, head unchanged, epoch-1 payload is now
    // the 2-byte stub, and the row kept its seq.
    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1, 2, 3]);
    let mut rows = conn
        .query(
            "SELECT seq, LENGTH(commit_data) FROM mls_commit_log \
             WHERE conversation_id = 'c1' AND generation = 0 AND epoch = 1",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), seq, "seq preserved");
    assert_eq!(row.get::<i64>(1).unwrap(), 2, "payload truncated to the stub");
}

// ── The other half of the bar: legitimate traffic must NOT trip the triggers ──

#[tokio::test]
async fn legitimate_traffic_survives_triggers() {
    let db = fresh_log().await;
    let conn = db.conn().unwrap();

    // A long contiguous append — every insert is at the head, none trips the gap
    // trigger.
    seed(&db, "c1", 0, 20).await;

    // The real retention prefix-prune (a multi-row DELETE of epochs `< floor`)
    // runs clean against the triggered schema and keeps the head. This is the
    // order-independence proof in practice: whatever order the engine visits the
    // 10 deleted rows, the head (epoch 20) is never among them.
    let removed = delete_commits_below(&conn, "c1", 0, 10).await.expect("prefix prune");
    assert_eq!(removed, 10);
    assert_eq!(epochs(&db, "c1", 0).await, (10..=20).collect::<Vec<_>>());

    // Whole-generation retirement of a closed lineage also runs clean.
    seed(&db, "c2", 0, 3).await; // closed gen 0
    seed(&db, "c2", 1, 4).await; // head gen 1
    let gens = delete_generations_below(&conn, "c2", 1).await.expect("generation prune");
    assert_eq!(gens, 4);
    assert_eq!(epochs(&db, "c2", 0).await, Vec::<i64>::new());
    assert_eq!(epochs(&db, "c2", 1).await, (0..=4).collect::<Vec<_>>());
}

/// Drive the REAL CAS path (`submit_commit`, `INSERT ... SELECT ... WHERE ... ON
/// CONFLICT DO NOTHING`) against the triggered schema. `serialize.rs` proves the
/// CAS in isolation but against an untriggered schema; this proves the CAS and
/// the `BEFORE INSERT` trigger coexist — a legitimate head-append is Accepted
/// (the trigger does not spuriously abort it) and a stale submit is Rejected by
/// the CAS's WHERE (no row is produced, so the trigger never even fires).
#[tokio::test]
async fn real_submit_path_coexists_with_triggers() {
    let db = fresh_log().await;
    let conn = db.conn().unwrap();

    // base64 of a 3-byte commit blob — avoids pulling in the base64 crate.
    let body = |based_on_epoch: i64| SubmitBody {
        conversation_id: "c1".into(),
        generation: 0,
        based_on_epoch,
        closes_epoch: None,
        sender_id: "sender".into(),
        commit: "AQID".into(),
        added_user_id: None,
        added_device_ids: None,
        group_info: None,
        welcomes: vec![],
    };

    // Opening commit (head is 0 on an empty lineage) → Accepted.
    match submit_commit(&conn, &body(0)).await.expect("submit 0") {
        SubmitResponse::Accepted { epoch, .. } => assert_eq!(epoch, 0),
        other => panic!("expected Accepted, got {other:?}"),
    }
    // Next head (head is now 1) → Accepted.
    match submit_commit(&conn, &body(1)).await.expect("submit 1") {
        SubmitResponse::Accepted { epoch, .. } => assert_eq!(epoch, 1),
        other => panic!("expected Accepted, got {other:?}"),
    }
    // Stale resubmit at epoch 1 (head is now 2): the CAS WHERE is false, so no
    // row is produced and nothing is inserted — Rejected, and the trigger is a
    // no-op because it only fires on a row that is actually being inserted.
    match submit_commit(&conn, &body(1)).await.expect("stale submit") {
        SubmitResponse::Rejected { head, .. } => assert_eq!(head, 2),
        other => panic!("expected Rejected, got {other:?}"),
    }

    // The chain is contiguous and intact — the CAS built it, the triggers let it.
    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1]);
}
