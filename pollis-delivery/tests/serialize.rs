//! Delivery Service serialization invariants, against a real (local) libsql DB:
//! at most one commit per epoch (no fork), only head-extending commits accepted
//! (no gap), and concurrent submitters yield exactly one winner.

use std::sync::Arc;

use base64::Engine as _;
use pollis_delivery::commit::{
    fetch_commits, head_epoch, head_epoch_in, head_generation, submit_commit, SubmitBody,
    SubmitResponse, SubmitVerdict, WelcomeBody,
};
use pollis_delivery::db::Db;

mod common;

/// Drive the real `submit_commit` on a single-DB fixture; a Welcome-authz refusal panics.
async fn submit(
    conn: &libsql::Connection,
    body: &SubmitBody,
) -> anyhow::Result<SubmitResponse> {
    match submit_commit(conn, conn, body).await? {
        SubmitVerdict::Response(r) => Ok(r),
        other => panic!("expected a CAS response, got {other:?}"),
    }
}


fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn body(conv: &str, epoch: i64, sender: &str) -> SubmitBody {
    SubmitBody {
        conversation_id: conv.to_string(),
        generation: 0,
        based_on_epoch: epoch,
        closes_epoch: None,
        sender_id: sender.to_string(),
        commit: b64(format!("commit-{conv}-{epoch}-{sender}").as_bytes()),
        added_user_id: None,
        added_device_ids: None,
        group_info: Some(b64(b"group-info")),
        welcomes: vec![],
    }
}

/// The opening commit of `generation`: epoch 0, naming the head it closes.
fn migration_body(conv: &str, generation: i64, closes_epoch: Option<i64>, sender: &str) -> SubmitBody {
    SubmitBody {
        generation,
        based_on_epoch: 0,
        closes_epoch,
        ..body(conv, 0, sender)
    }
}

/// Like [`body`] with one Welcome, so a test can poison its insert.
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

async fn count_for_conv(db: &Db, table: &str, conv: &str) -> i64 {
    let conn = db.conn().await.unwrap();
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE conversation_id = ?1");
    let mut rows = conn.query(&sql, libsql::params![conv]).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

/// Put `users` in the roster: every bundled Welcome is checked against it (C1).
async fn seed_members(conn: &libsql::Connection, conversation_id: &str, users: &[(&str, &str)]) {
    for (user, role) in users {
        conn.execute(
            "INSERT OR REPLACE INTO group_member (group_id, user_id, role) VALUES (?1, ?2, ?3)",
            libsql::params![conversation_id, *user, *role],
        )
        .await
        .expect("seed member");
    }
}

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db
}

/// The CAS coexists with the 000005 `BEFORE INSERT` trigger: no spurious aborts.
#[tokio::test(flavor = "multi_thread")]
async fn accepts_head_rejects_stale_and_gap() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let c = "conv1";

    match submit(&conn, &body(c, 0, "alice")).await.unwrap() {
        SubmitResponse::Accepted { epoch, .. } => assert_eq!(epoch, 0),
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 1);

    match submit(&conn, &body(c, 0, "bob")).await.unwrap() {
        SubmitResponse::Rejected { head, missing, .. } => {
            assert_eq!(head, 1);
            assert_eq!(missing.len(), 1);
            assert_eq!(missing[0].epoch, 0);
        }
        other => panic!("expected Rejected, got {other:?}"),
    }

    match submit(&conn, &body(c, 1, "alice")).await.unwrap() {
        SubmitResponse::Accepted { epoch, .. } => assert_eq!(epoch, 1),
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 2);

    match submit(&conn, &body(c, 5, "alice")).await.unwrap() {
        SubmitResponse::Rejected { head, .. } => assert_eq!(head, 2),
        other => panic!("expected Rejected, got {other:?}"),
    }

    let commits = fetch_commits(&conn, c, 0, 0).await.unwrap();
    let epochs: Vec<i64> = commits.iter().map(|c| c.epoch).collect();
    assert_eq!(epochs, vec![0, 1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_submitters_yield_exactly_one_winner() {
    let db = fresh_db().await;
    let c = "race";

    let mut handles = Vec::new();
    for i in 0..8 {
        let db = Arc::clone(&db);
        handles.push(tokio::spawn(async move {
            let conn = db.conn().await.unwrap();
            // busy_timeout is per-connection: a local-file artifact; Turso serializes server-side.
            conn.execute_batch("PRAGMA busy_timeout=10000;").await.unwrap();
            let sender = format!("client{i}");
            submit(&conn, &body(c, 0, &sender)).await.unwrap()
        }));
    }

    let mut accepted = 0;
    let mut rejected = 0;
    for h in handles {
        match h.await.unwrap() {
            SubmitResponse::Accepted { epoch, .. } => {
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

    let conn = db.conn().await.unwrap();
    let commits = fetch_commits(&conn, c, 0, 0).await.unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].epoch, 0);
}

/// The submit bundle is one transaction: a failed Welcome rolls back everything (#430 P0).
#[tokio::test(flavor = "multi_thread")]
async fn welcome_failure_rolls_back_commit_and_group_info() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let c = "atomic";
    seed_members(&conn, c, &[("alice", "member"), ("BOOM", "member")]).await;

    // A trigger injects the failure without replacing the shipped table (#875).
    conn.execute_batch(
        "CREATE TRIGGER poison_welcome_to_boom \
           BEFORE INSERT ON mls_welcome \
           FOR EACH ROW WHEN NEW.recipient_id = 'BOOM' \
         BEGIN SELECT RAISE(ABORT, 'poisoned welcome'); END;",
    )
    .await
    .expect("install the poison trigger");

    let res = submit(&conn, &body_with_welcome(c, 0, "alice", "BOOM")).await;
    assert!(res.is_err(), "poisoned Welcome must surface an error");

    assert_eq!(head_epoch(&conn, c).await.unwrap(), 0, "no commit persisted");
    assert!(
        fetch_commits(&conn, c, 0, 0).await.unwrap().is_empty(),
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

    match submit(&conn, &body(c, 0, "alice")).await.unwrap() {
        SubmitResponse::Accepted { epoch, .. } => assert_eq!(epoch, 0),
        other => panic!("expected Accepted after rollback, got {other:?}"),
    }
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 1);
}

/// A re-sent Welcome for the same tuple updates in place (#430 P2).
#[tokio::test(flavor = "multi_thread")]
async fn duplicate_welcome_insert_is_idempotent() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let c = "dupe";
    seed_members(&conn, c, &[("sender", "member"), ("alice", "member")]).await;

    match submit(&conn, &body_with_welcome(c, 0, "sender", "alice"))
        .await
        .unwrap()
    {
        SubmitResponse::Accepted { epoch, .. } => assert_eq!(epoch, 0),
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(count_for_conv(&db, "mls_welcome", c).await, 1);

    match submit(&conn, &body_with_welcome(c, 1, "sender", "alice"))
        .await
        .unwrap()
    {
        SubmitResponse::Accepted { epoch, .. } => assert_eq!(epoch, 1),
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(
        count_for_conv(&db, "mls_welcome", c).await,
        1,
        "re-sent Welcome must update in place, not duplicate"
    );
}

/// The inline GroupInfo write is epoch-monotone like `/v1/group-info`.
#[tokio::test(flavor = "multi_thread")]
async fn inline_group_info_write_is_epoch_monotone() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let c = "monotone";

    conn.execute(
        "INSERT INTO mls_group_info (conversation_id, epoch, group_info, updated_by_device_id) \
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![c, 100_i64, b"newer".to_vec(), "devX"],
    )
    .await
    .unwrap();

    match submit(&conn, &body(c, 0, "alice")).await.unwrap() {
        SubmitResponse::Accepted { epoch, .. } => assert_eq!(epoch, 0),
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

/// Opening a successor needs epoch 0, the next generation, and the correct `closes_epoch`.
#[tokio::test(flavor = "multi_thread")]
async fn opening_a_generation_requires_naming_the_closed_head() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let c = "conv-mig";

    for e in 0..2 {
        assert!(matches!(
            submit(&conn, &body(c, e, "alice")).await.unwrap(),
            SubmitResponse::Accepted { .. }
        ));
    }
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 2);

    // The stale-migrator case: a commit landed after the migrator read the roster.
    match submit(&conn, &migration_body(c, 1, Some(1), "alice")).await.unwrap() {
        SubmitResponse::Rejected { head, head_generation, .. } => {
            assert_eq!(head, 0, "generation 1 does not exist yet, so its head is 0");
            assert_eq!(head_generation, 0);
        }
        other => panic!("expected Rejected, got {other:?}"),
    }

    // A missing `closes_epoch` must not compare away to an accept.
    assert!(matches!(
        submit(&conn, &migration_body(c, 1, None, "alice")).await.unwrap(),
        SubmitResponse::Rejected { .. }
    ));

    assert!(matches!(
        submit(&conn, &migration_body(c, 2, Some(2), "alice")).await.unwrap(),
        SubmitResponse::Rejected { .. }
    ));

    assert_eq!(head_generation(&conn, c).await.unwrap(), 0);
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 2);

    match submit(&conn, &migration_body(c, 1, Some(2), "alice")).await.unwrap() {
        SubmitResponse::Accepted { generation, epoch } => {
            assert_eq!((generation, epoch), (1, 0));
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(head_generation(&conn, c).await.unwrap(), 1);
    assert_eq!(head_epoch_in(&conn, c, 1).await.unwrap(), 1);
    assert_eq!(head_epoch_in(&conn, c, 0).await.unwrap(), 2);
}

/// A generation can never be opened twice, so migrators cannot fork the conversation.
#[tokio::test(flavor = "multi_thread")]
async fn a_generation_can_only_be_opened_once() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let c = "conv-mig2";

    submit(&conn, &body(c, 0, "alice")).await.unwrap();
    assert_eq!(head_epoch(&conn, c).await.unwrap(), 1);

    assert!(matches!(
        submit(&conn, &migration_body(c, 1, Some(1), "alice")).await.unwrap(),
        SubmitResponse::Accepted { generation: 1, epoch: 0 }
    ));
    match submit(&conn, &migration_body(c, 1, Some(1), "bob")).await.unwrap() {
        SubmitResponse::Rejected { head, head_generation, missing } => {
            assert_eq!(head_generation, 1);
            assert_eq!(head, 1, "generation 1 already has its opening commit");
            assert_eq!(missing.len(), 1, "bob is handed the commit that beat him");
            assert_eq!((missing[0].generation, missing[0].epoch), (1, 0));
        }
        other => panic!("expected Rejected, got {other:?}"),
    }

    assert_eq!(fetch_commits(&conn, c, 1, 0).await.unwrap().len(), 1);
}

/// The retired lineage is closed; the rejection reports `head_generation`.
#[tokio::test(flavor = "multi_thread")]
async fn a_closed_lineage_rejects_further_commits() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let c = "conv-mig3";

    submit(&conn, &body(c, 0, "alice")).await.unwrap();
    submit(&conn, &migration_body(c, 1, Some(1), "alice")).await.unwrap();

    match submit(&conn, &body(c, 1, "bob")).await.unwrap() {
        SubmitResponse::Rejected { head, head_generation, missing } => {
            assert_eq!(head, 1, "generation 0's head, the lineage bob is in");
            assert_eq!(head_generation, 1, "…but a successor lineage exists");
            assert!(
                missing.is_empty(),
                "bob is at generation 0's head, so he is missing nothing THERE — \
                 what he needs is the Welcome into generation 1, not a re-base"
            );
        }
        other => panic!("expected Rejected, got {other:?}"),
    }

    assert_eq!(fetch_commits(&conn, c, 0, 0).await.unwrap().len(), 1);
}

/// A successor's GroupInfo replaces the retired one despite a lower epoch.
#[tokio::test(flavor = "multi_thread")]
async fn successor_group_info_replaces_a_numerically_higher_epoch() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let c = "conv-mig4";

    for e in 0..5 {
        submit(&conn, &body(c, e, "alice")).await.unwrap();
    }
    let (gen, epoch) = published_group_info(&db, c).await;
    assert_eq!((gen, epoch), (0, 5));

    submit(&conn, &migration_body(c, 1, Some(5), "alice")).await.unwrap();
    let (gen, epoch) = published_group_info(&db, c).await;
    assert_eq!(
        (gen, epoch),
        (1, 1),
        "the successor's GroupInfo wins on generation, despite the lower epoch"
    );
}

async fn published_group_info(db: &Db, conv: &str) -> (i64, i64) {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT generation, epoch FROM mls_group_info WHERE conversation_id = ?1",
            libsql::params![conv],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    (row.get(0).unwrap(), row.get(1).unwrap())
}
