//! Delivery Service serialization invariants, against a real (local) libsql DB:
//! at most one commit per epoch (no fork), only head-extending commits accepted
//! (no gap), and concurrent submitters yield exactly one winner.

use std::sync::Arc;

use base64::Engine as _;
use pollis_delivery::commit::{fetch_commits, head_epoch, submit_commit, SubmitBody, SubmitResponse};
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
);";

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
