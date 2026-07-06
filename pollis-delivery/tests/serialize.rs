//! Delivery Service serialization invariants, against a real (local) libsql DB:
//! at most one commit per epoch (no fork), only head-extending commits accepted
//! (no gap), and concurrent submitters yield exactly one winner.

use std::sync::Arc;

use base64::Engine as _;
use pollis_delivery::commit::{
    fetch_commits, head_epoch, submit_commit, SubmitBody, SubmitResponse, WelcomeBody,
};
use pollis_delivery::db::Db;

const SCHEMA: &str = "\
CREATE TABLE mls_commit_log (\
  seq INTEGER PRIMARY KEY AUTOINCREMENT,\
  conversation_id TEXT NOT NULL,\
  epoch INTEGER NOT NULL,\
  sender_id TEXT NOT NULL,\
  commit_data BLOB NOT NULL,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  added_user_id TEXT,\
  added_device_ids TEXT\
);\
CREATE UNIQUE INDEX idx_mls_commit_conv_epoch ON mls_commit_log (conversation_id, epoch);\
CREATE TABLE mls_group_info (\
  conversation_id TEXT PRIMARY KEY,\
  epoch INTEGER NOT NULL,\
  group_info BLOB NOT NULL,\
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),\
  updated_by_device_id TEXT NOT NULL\
);\
CREATE TABLE mls_welcome (\
  id TEXT PRIMARY KEY,\
  conversation_id TEXT NOT NULL,\
  recipient_id TEXT NOT NULL,\
  welcome_data BLOB NOT NULL,\
  delivered INTEGER NOT NULL DEFAULT 0,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  recipient_device_id TEXT\
);\
CREATE UNIQUE INDEX idx_mls_welcome_recipient ON mls_welcome (conversation_id, recipient_id, recipient_device_id);";

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn body(conv: &str, epoch: i64, sender: &str) -> SubmitBody {
    SubmitBody {
        conversation_id: conv.to_string(),
        based_on_epoch: epoch,
        sender_id: sender.to_string(),
        commit: b64(format!("commit-{conv}-{epoch}-{sender}").as_bytes()),
        added_user_id: None,
        added_device_ids: None,
        group_info: Some(b64(b"group-info")),
        welcomes: vec![],
    }
}

/// Like [`body`] but carries a single Welcome to `welcome_recipient`, so a test
/// can poison that recipient and drive the Welcome insert to fail.
fn body_with_welcome(conv: &str, epoch: i64, sender: &str, welcome_recipient: &str) -> SubmitBody {
    SubmitBody {
        welcomes: vec![WelcomeBody {
            recipient_id: welcome_recipient.to_string(),
            recipient_device_id: "dev1".to_string(),
            welcome: b64(b"welcome-blob"),
        }],
        ..body(conv, epoch, sender)
    }
}

/// COUNT(*) of `table` scoped to a conversation — small helper so the atomicity
/// assertions read cleanly.
async fn count_for_conv(db: &Db, table: &str, conv: &str) -> i64 {
    let conn = db.conn().unwrap();
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE conversation_id = ?1");
    let mut rows = conn.query(&sql, libsql::params![conv]).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn fresh_db() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("delivery.db");
    // Keep the tempdir alive for the process.
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    db.conn().unwrap().execute_batch(SCHEMA).await.expect("schema");
    Arc::new(db)
}

#[tokio::test(flavor = "multi_thread")]
async fn accepts_head_rejects_stale_and_gap() {
    let db = fresh_db().await;
    let conn = db.conn().unwrap();
    let c = "conv1";

    // Empty group: head is 0. A commit from epoch 0 wins.
    match submit_commit(&conn, &body(c, 0, "alice")).await.unwrap() {
        SubmitResponse::Accepted { epoch } => assert_eq!(epoch, 0),
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 1);

    // A second commit ALSO from epoch 0 is stale → rejected, head is 1, and it
    // gets back the commit it's missing.
    match submit_commit(&conn, &body(c, 0, "bob")).await.unwrap() {
        SubmitResponse::Rejected { head, missing } => {
            assert_eq!(head, 1);
            assert_eq!(missing.len(), 1);
            assert_eq!(missing[0].epoch, 0);
        }
        other => panic!("expected Rejected, got {other:?}"),
    }

    // A commit from the head (epoch 1) wins.
    match submit_commit(&conn, &body(c, 1, "alice")).await.unwrap() {
        SubmitResponse::Accepted { epoch } => assert_eq!(epoch, 1),
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 2);

    // A forward gap (epoch 5 when head is 2) is rejected — no gap can be created.
    match submit_commit(&conn, &body(c, 5, "alice")).await.unwrap() {
        SubmitResponse::Rejected { head, .. } => assert_eq!(head, 2),
        other => panic!("expected Rejected, got {other:?}"),
    }

    // The log is contiguous: epochs 0, 1.
    let commits = fetch_commits(&conn, c, 0).await.unwrap();
    let epochs: Vec<i64> = commits.iter().map(|c| c.epoch).collect();
    assert_eq!(epochs, vec![0, 1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_submitters_yield_exactly_one_winner() {
    let db = fresh_db().await;
    let c = "race";

    // 8 clients all submit a commit from epoch 0 at once. Exactly one may win.
    let mut handles = Vec::new();
    for i in 0..8 {
        let db = Arc::clone(&db);
        handles.push(tokio::spawn(async move {
            let conn = db.conn().unwrap();
            // busy_timeout is per-connection: each writer waits for the local
            // file lock instead of erroring (a local-file test artifact; Turso
            // serializes writes server-side). The conditional INSERT still
            // decides exactly one winner.
            conn.execute_batch("PRAGMA busy_timeout=10000;").await.unwrap();
            let sender = format!("client{i}");
            submit_commit(&conn, &body(c, 0, &sender)).await.unwrap()
        }));
    }

    let mut accepted = 0;
    let mut rejected = 0;
    for h in handles {
        match h.await.unwrap() {
            SubmitResponse::Accepted { epoch } => {
                assert_eq!(epoch, 0);
                accepted += 1;
            }
            SubmitResponse::Rejected { head, .. } => {
                assert_eq!(head, 1, "rejected losers must see the advanced head");
                rejected += 1;
            }
        }
    }

    assert_eq!(accepted, 1, "exactly one commit may win the epoch — no fork");
    assert_eq!(rejected, 7);

    // And the log has exactly one commit at epoch 0.
    let conn = db.conn().unwrap();
    let commits = fetch_commits(&conn, c, 0).await.unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].epoch, 0);
}

// ── Atomicity of the submit bundle (issue #430 P0) ───────────────────────────

/// The commit + GroupInfo + Welcome(s) for one submit are a single transaction:
/// if the Welcome insert fails, the WHOLE bundle rolls back — no commit row and
/// no GroupInfo row is left behind. (Before P0 these were three separate
/// non-transactional writes, so a partial write was possible.)
#[tokio::test(flavor = "multi_thread")]
async fn welcome_failure_rolls_back_commit_and_group_info() {
    let db = fresh_db().await;
    let conn = db.conn().unwrap();
    let c = "atomic";

    // Recreate mls_welcome with a poison-pill CHECK so a Welcome to 'BOOM' fails
    // its INSERT — the last write in the submit bundle. This injects the
    // "commit lands, Welcome fails" partial-write scenario.
    conn.execute_batch(
        "DROP TABLE mls_welcome;\
         CREATE TABLE mls_welcome (\
           id TEXT PRIMARY KEY,\
           conversation_id TEXT NOT NULL,\
           recipient_id TEXT NOT NULL,\
           welcome_data BLOB NOT NULL,\
           delivered INTEGER NOT NULL DEFAULT 0,\
           created_at TEXT NOT NULL DEFAULT (datetime('now')),\
           recipient_device_id TEXT,\
           CHECK (recipient_id <> 'BOOM')\
         );\
         CREATE UNIQUE INDEX idx_mls_welcome_recipient ON mls_welcome (conversation_id, recipient_id, recipient_device_id);",
    )
    .await
    .expect("recreate mls_welcome with poison check");

    // The commit would win epoch 0 and the GroupInfo would be published — but the
    // poisoned Welcome insert fails, so the whole bundle must roll back.
    let res = submit_commit(&conn, &body_with_welcome(c, 0, "alice", "BOOM")).await;
    assert!(res.is_err(), "poisoned Welcome must surface an error");

    // Full rollback: head is still 0 (no commit persisted), and neither the
    // commit log, the GroupInfo, nor the Welcome retained anything.
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 0, "no commit persisted");
    assert!(
        fetch_commits(&conn, c, 0).await.unwrap().is_empty(),
        "commit row must have rolled back"
    );
    assert_eq!(
        count_for_conv(&db, "mls_group_info", c).await,
        0,
        "GroupInfo row must have rolled back"
    );
    assert_eq!(
        count_for_conv(&db, "mls_welcome", c).await,
        0,
        "no Welcome row persisted"
    );

    // The group is left in a clean state: a well-formed resubmit at head 0 now
    // succeeds (the failed submit left no partial state to trip over).
    match submit_commit(&conn, &body(c, 0, "alice")).await.unwrap() {
        SubmitResponse::Accepted { epoch } => assert_eq!(epoch, 0),
        other => panic!("expected Accepted after rollback, got {other:?}"),
    }
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 1);
}

/// A re-sent Welcome for the same (conversation, recipient, device) is
/// idempotent (issue #430 P2): the second submit's inline Welcome insert updates
/// the existing row in place instead of erroring on the UNIQUE tuple or stacking
/// a duplicate row.
#[tokio::test(flavor = "multi_thread")]
async fn duplicate_welcome_insert_is_idempotent() {
    let db = fresh_db().await;
    let conn = db.conn().unwrap();
    let c = "dupe";

    // A commit at head 0 carrying a Welcome to alice/dev1 wins and inserts it.
    match submit_commit(&conn, &body_with_welcome(c, 0, "sender", "alice"))
        .await
        .unwrap()
    {
        SubmitResponse::Accepted { epoch } => assert_eq!(epoch, 0),
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(count_for_conv(&db, "mls_welcome", c).await, 1);

    // A second commit at the new head (epoch 1) carries the SAME recipient/device
    // Welcome. Its inline insert conflicts on the UNIQUE tuple and updates in
    // place — no error, still exactly one Welcome row.
    match submit_commit(&conn, &body_with_welcome(c, 1, "sender", "alice"))
        .await
        .unwrap()
    {
        SubmitResponse::Accepted { epoch } => assert_eq!(epoch, 1),
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(
        count_for_conv(&db, "mls_welcome", c).await,
        1,
        "re-sent Welcome must update in place, not duplicate"
    );
}

/// The inline GroupInfo write in the submit bundle obeys the SAME epoch-monotone
/// guard as the standalone `/v1/group-info` upsert: an accepted commit whose
/// resulting-epoch GroupInfo is OLDER than what's already published must not
/// regress it.
#[tokio::test(flavor = "multi_thread")]
async fn inline_group_info_write_is_epoch_monotone() {
    let db = fresh_db().await;
    let conn = db.conn().unwrap();
    let c = "monotone";

    // A newer GroupInfo (epoch 100) is already published out of band.
    conn.execute(
        "INSERT INTO mls_group_info (conversation_id, epoch, group_info, updated_by_device_id) \
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![c, 100_i64, b"newer".to_vec(), "devX"],
    )
    .await
    .unwrap();

    // A commit accepted at head 0 carries a GroupInfo at resulting epoch 1. The
    // commit still lands, but the inline write must NOT regress the stored
    // GroupInfo from epoch 100 down to 1.
    match submit_commit(&conn, &body(c, 0, "alice")).await.unwrap() {
        SubmitResponse::Accepted { epoch } => assert_eq!(epoch, 0),
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 1, "commit still landed");

    let mut rows = conn
        .query(
            "SELECT epoch FROM mls_group_info WHERE conversation_id = ?1",
            libsql::params![c],
        )
        .await
        .unwrap();
    let stored: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        stored, 100,
        "inline GroupInfo write must not regress a newer epoch"
    );
}
