//! Schema-layer enforcement of invariant I1 (#691), attempted by direct SQL to bypass the CAS.
//! The real log-DB migrations, `include_str!`'d and applied with native `execute_batch`.

use pollis_delivery::commit::{delete_commits_below, delete_generations_below};
use pollis_delivery::db::Db;

mod common;

// The real log-DB schema in version order; 000005 is the trigger migration.
const LOG_MIGRATIONS: &[&str] = &[
    include_str!("../../pollis-schema/migrations-log/000001_commit_log_db.sql"),
    include_str!("../../pollis-schema/migrations-log/000002_mls_welcome_unique_recipient.sql"),
    include_str!("../../pollis-schema/migrations-log/000003_mls_commit_since.sql"),
    include_str!("../../pollis-schema/migrations-log/000004_commit_generation.sql"),
    include_str!("../../pollis-schema/migrations-log/000005_mls_commit_log_triggers.sql"),
];

async fn fresh_log() -> common::TempDb {
    let db = common::TempDb::open("log.db").await;
    let conn = db.conn().await.unwrap();
    for sql in LOG_MIGRATIONS {
        conn.execute_batch(sql).await.expect("apply migration");
    }
    db
}

/// Insert a commit by direct SQL, returning the raw result.
async fn raw_insert(db: &Db, conv: &str, gen: i64, epoch: i64) -> Result<u64, libsql::Error> {
    db.conn().await
        .unwrap()
        .execute(
            "INSERT INTO mls_commit_log (conversation_id, generation, epoch, sender_id, commit_data) \
             VALUES (?1, ?2, ?3, 'sender', ?4)",
            libsql::params![conv, gen, epoch, vec![epoch as u8]],
        )
        .await
}

/// Seed a contiguous chain `0..=up_to` in `gen` via direct SQL (head appends only).
async fn seed(db: &Db, conv: &str, gen: i64, up_to: i64) {
    for e in 0..=up_to {
        raw_insert(db, conv, gen, e).await.expect("legit head append");
    }
}

async fn epochs(db: &Db, conv: &str, gen: i64) -> Vec<i64> {
    let conn = db.conn().await.unwrap();
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

#[tokio::test]
async fn insert_forward_gap_is_rejected() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 2).await;

    let gap = raw_insert(&db, "c1", 0, 4).await;
    assert!(gap.is_err(), "forward-gap insert must be rejected by the trigger");
    assert!(
        gap.unwrap_err().to_string().contains("forward gap"),
        "error should name the I1 forward-gap violation"
    );

    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1, 2]);

    raw_insert(&db, "c1", 0, 3).await.expect("head append accepted");
    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1, 2, 3]);
}

#[tokio::test]
async fn first_insert_must_be_epoch_zero() {
    let db = fresh_log().await;
    assert!(
        raw_insert(&db, "c1", 0, 5).await.is_err(),
        "first commit at epoch 5 is a forward gap and must be rejected"
    );
    raw_insert(&db, "c1", 0, 0).await.expect("opening epoch 0 accepted");
    // A successor generation opens at epoch 0.
    raw_insert(&db, "c1", 1, 0).await.expect("generation-open epoch 0 accepted");
    assert_eq!(epochs(&db, "c1", 1).await, vec![0]);
}

#[tokio::test]
async fn update_of_history_is_rejected() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 2).await;
    let conn = db.conn().await.unwrap();

    let repoint = conn
        .execute(
            "UPDATE mls_commit_log SET epoch = 9 WHERE conversation_id = 'c1' AND epoch = 1",
            (),
        )
        .await;
    assert!(repoint.is_err(), "changing epoch must be rejected");

    let tamper = conn
        .execute(
            "UPDATE mls_commit_log SET commit_data = X'00' WHERE conversation_id = 'c1' AND epoch = 1",
            (),
        )
        .await;
    assert!(tamper.is_err(), "changing commit_data must be rejected");

    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1, 2]);

    // A non-identity metadata update is deliberately not guarded.
    conn.execute(
        "UPDATE mls_commit_log SET added_device_ids = 'd1' WHERE conversation_id = 'c1' AND epoch = 1",
        (),
    )
    .await
    .expect("benign metadata update is allowed");
}

// I1: the head cannot be deleted while earlier commits remain; deleting the last row is allowed.

#[tokio::test]
async fn delete_of_head_is_rejected() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 3).await;

    // Deleting the head while earlier epochs remain wedges every member.
    let del_head = db
        .conn().await
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

    db.conn().await
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

    assert!(
        db.conn().await
            .unwrap()
            .execute(
                "DELETE FROM mls_commit_log WHERE conversation_id = 'c1' AND generation = 1 AND epoch = 1",
                (),
            )
            .await
            .is_err(),
        "deleting the head-generation head must be rejected"
    );

    // A closed generation is not the head generation, so it may be retired wholesale.
    db.conn().await
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

/// Deleting the head when it is the only row is allowed (`wipe_remote` relies on it).
#[tokio::test]
async fn delete_head_when_only_row_is_allowed() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 0).await;

    db.conn().await
        .unwrap()
        .execute(
            "DELETE FROM mls_commit_log WHERE conversation_id = 'c1' AND generation = 0 AND epoch = 0",
            (),
        )
        .await
        .expect("deleting the last remaining commit is allowed (empty chain is gapless)");
    assert_eq!(epochs(&db, "c1", 0).await, Vec::<i64>::new());
}

/// A bare `DELETE FROM mls_commit_log` visits each conversation's head last (ascending seq).
#[tokio::test]
async fn bulk_wipe_empties_the_table() {
    let db = fresh_log().await;
    let conn = db.conn().await.unwrap();
    // Interleave two conversations so their seqs alternate.
    for e in 0..=3 {
        for c in ["c1", "c2"] {
            raw_insert(&db, c, 0, e).await.expect("interleaved head append");
        }
    }
    assert_eq!(epochs(&db, "c1", 0).await, vec![0, 1, 2, 3]);
    assert_eq!(epochs(&db, "c2", 0).await, vec![0, 1, 2, 3]);

    conn.execute("DELETE FROM mls_commit_log", ())
        .await
        .expect("bare DELETE FROM mls_commit_log must empty the table, not abort on the head");

    let count: i64 = {
        let mut rows = conn.query("SELECT COUNT(*) FROM mls_commit_log", ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    };
    assert_eq!(count, 0, "the wipe left rows behind — the head guard fired mid-wipe");
}

/// A bottom-up per-row prune to empty: the final head delete is the only row.
#[tokio::test]
async fn bottom_up_prune_to_empty_succeeds() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 3).await;
    let conn = db.conn().await.unwrap();

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

/// The #680 harness's DELETE-then-INSERT corruption shape must pass both triggers.
#[tokio::test]
async fn interior_delete_then_reinsert_corruption_passes() {
    let db = fresh_log().await;
    seed(&db, "c1", 0, 3).await;
    let conn = db.conn().await.unwrap();

    // Keep the seq so the row keeps its place in catch-up replay.
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

    let deleted = conn
        .execute("DELETE FROM mls_commit_log WHERE seq = ?1", libsql::params![seq])
        .await
        .expect("interior delete passes the head guard");
    assert_eq!(deleted, 1);

    // Below the head, and an INSERT, so neither guard fires.
    conn.execute(
        "INSERT INTO mls_commit_log (seq, conversation_id, generation, epoch, sender_id, commit_data) \
         VALUES (?1, 'c1', 0, 1, 'sender', X'0001')",
        libsql::params![seq],
    )
    .await
    .expect("interior re-insert passes both triggers");

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

// Normal traffic must not trip the triggers

#[tokio::test]
async fn legitimate_traffic_survives_triggers() {
    let db = fresh_log().await;
    let conn = db.conn().await.unwrap();

    seed(&db, "c1", 0, 20).await;

    // The retention prefix-prune never visits the head, in any order.
    let removed = delete_commits_below(&conn, "c1", 0, 10).await.expect("prefix prune");
    assert_eq!(removed, 10);
    assert_eq!(epochs(&db, "c1", 0).await, (10..=20).collect::<Vec<_>>());

    seed(&db, "c2", 0, 3).await; // closed gen 0
    seed(&db, "c2", 1, 4).await; // head gen 1
    let gens = delete_generations_below(&conn, "c2", 1).await.expect("generation prune");
    assert_eq!(gens, 4);
    assert_eq!(epochs(&db, "c2", 0).await, Vec::<i64>::new());
    assert_eq!(epochs(&db, "c2", 1).await, (0..=4).collect::<Vec<_>>());
}
