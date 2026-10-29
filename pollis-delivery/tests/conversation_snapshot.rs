//! #987 acceptance gates P9 (snapshot atomicity) and P10 (batch contiguity).
//! P9: the join gate is meaningful only if GroupInfo and Welcome come from one snapshot.
//! P10: a hole must be a fact, distinguishable from a retention prune (#539).
//! The log DB refuses forward gaps on INSERT, so fixtures build holes by removing a row.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pollis_api::reads::ConversationStateQuery;
use pollis_delivery::reads::{snapshot, Reader};

mod common;

const CONV: &str = "conv-p9";
const ME: &str = "u-me";
const MY_DEVICE: &str = "d-me";

async fn log_db() -> common::TempDb {
    let db = common::TempDb::open("log.db").await;
    let conn = db.conn().await.expect("conn");
    pollis_schema::apply::log_db(&conn).await.expect("log schema");
    db
}

fn who() -> Reader {
    Reader {
        user_id: ME.to_string(),
        device_id: MY_DEVICE.to_string(),
    }
}

fn cheap_query(conversation_id: &str) -> ConversationStateQuery {
    ConversationStateQuery {
        conversation_id: conversation_id.to_string(),
        generation: None,
        since: None,
        want_group_info_blob: false,
        commit_at_epoch: None,
    }
}

fn batch_query(conversation_id: &str, since: i64) -> ConversationStateQuery {
    ConversationStateQuery {
        conversation_id: conversation_id.to_string(),
        generation: Some(0),
        since: Some(since),
        want_group_info_blob: false,
        commit_at_epoch: None,
    }
}

async fn insert_commit(conn: &libsql::Connection, conversation_id: &str, epoch: i64) {
    conn.execute(
        "INSERT INTO mls_commit_log \
           (conversation_id, epoch, generation, sender_id, commit_data, created_at) \
         VALUES (?1, ?2, 0, 'sealed', X'00', '2026-08-19T00:00:00+00:00')",
        libsql::params![conversation_id.to_string(), epoch],
    )
    .await
    .expect("insert commit");
}

/// Publish a GroupInfo and this device's Welcome in one transaction, as `submit_commit` does.
async fn publish_pair(conn: &libsql::Connection, epoch: i64) {
    let tx = conn.transaction().await.expect("tx");
    tx.execute(
        "INSERT INTO mls_group_info \
           (conversation_id, generation, epoch, group_info, updated_by_device_id) \
         VALUES (?1, 0, ?2, X'00', 'd-writer') \
         ON CONFLICT(conversation_id) DO UPDATE SET epoch = excluded.epoch",
        libsql::params![CONV.to_string(), epoch],
    )
    .await
    .expect("group info row");
    tx.execute(
        "INSERT INTO mls_welcome \
           (id, conversation_id, recipient_id, recipient_device_id, welcome_data, delivered) \
         VALUES (?1, ?2, ?3, ?4, X'00', 0) \
         ON CONFLICT(conversation_id, recipient_id, recipient_device_id) DO NOTHING",
        libsql::params![
            format!("w-{epoch}"),
            CONV.to_string(),
            ME.to_string(),
            MY_DEVICE.to_string()
        ],
    )
    .await
    .expect("welcome row");
    tx.commit().await.expect("commit tx");
}

/// Retract both halves in one transaction: any observation of one alone is a torn read.
async fn retract_pair(conn: &libsql::Connection) {
    let tx = conn.transaction().await.expect("tx");
    tx.execute(
        "DELETE FROM mls_group_info WHERE conversation_id = ?1",
        libsql::params![CONV.to_string()],
    )
    .await
    .expect("clear group info");
    tx.execute(
        "DELETE FROM mls_welcome WHERE conversation_id = ?1",
        libsql::params![CONV.to_string()],
    )
    .await
    .expect("clear welcome");
    tx.commit().await.expect("commit tx");
}

async fn group_info_present(conn: &libsql::Connection) -> bool {
    let mut rows = conn
        .query(
            "SELECT 1 FROM mls_group_info WHERE conversation_id = ?1",
            libsql::params![CONV.to_string()],
        )
        .await
        .expect("group info probe");
    rows.next().await.expect("row").is_some()
}

async fn welcome_pending(conn: &libsql::Connection) -> bool {
    let mut rows = conn
        .query(
            "SELECT 1 FROM mls_welcome \
             WHERE conversation_id = ?1 AND recipient_id = ?2 AND delivered = 0 \
               AND (recipient_device_id = ?3 OR recipient_device_id IS NULL) \
             LIMIT 1",
            libsql::params![CONV.to_string(), ME.to_string(), MY_DEVICE.to_string()],
        )
        .await
        .expect("welcome probe");
    rows.next().await.expect("row").is_some()
}

/// Two separate reads with a write between them observe a torn pair (the pre-#987 client).
#[tokio::test]
async fn a_split_read_can_observe_a_torn_pair() {
    let db = log_db().await;
    let conn = db.conn().await.expect("conn");
    insert_commit(&conn, CONV, 0).await;

    publish_pair(&conn, 1).await;

    let saw_group_info = group_info_present(&conn).await;
    assert!(saw_group_info, "fixture: the pair was published");

    retract_pair(&conn).await;

    let saw_welcome = welcome_pending(&conn).await;

    assert!(
        saw_group_info && !saw_welcome,
        "the split read did not tear, so this test is no longer demonstrating \
         the hazard the one-snapshot endpoint exists to remove"
    );
}

/// `snapshot` never tears; the reader samples until it sees both states, so a stalled writer fails at the cap (#1145).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn torn_group_info_and_welcome_is_not_observable() {
    let db = log_db().await;
    {
        let conn = db.conn().await.expect("conn");
        insert_commit(&conn, CONV, 0).await;
    }

    // The reader decides when the run ends, so it always samples.
    let done = Arc::new(AtomicBool::new(false));

    let writer_db = Arc::clone(&*db);
    let writer_done = Arc::clone(&done);
    let writer = tokio::spawn(async move {
        let conn = writer_db.conn().await.expect("writer conn");
        let mut epoch = 1i64;
        while !writer_done.load(Ordering::SeqCst) {
            publish_pair(&conn, epoch).await;
            // Yield while published, or a contended runner may never sample that window (#1145).
            tokio::task::yield_now().await;
            retract_pair(&conn).await;
            epoch += 1;
        }
    });

    let reader_db = Arc::clone(&*db);
    let reader = tokio::spawn(async move {
        let q = cheap_query(CONV);
        let me = who();
        let conn = reader_db.conn().await.expect("reader conn");
        let mut saw_published = 0usize;
        let mut saw_retracted = 0usize;
        // Minimum checks; past it, sample until both states are seen, up to a cap (#1145).
        const MIN_SAMPLES: usize = 2_000;
        const MAX_SAMPLES: usize = 200_000;
        let mut taken = 0usize;
        while taken < MIN_SAMPLES
            || ((saw_published == 0 || saw_retracted == 0) && taken < MAX_SAMPLES)
        {
            taken += 1;
            let snap = snapshot(&conn, &q, &me, Some(true))
                .await
                .expect("snapshot");
            let has_group_info = snap.group_info.is_some();
            if has_group_info {
                saw_published += 1;
            } else {
                saw_retracted += 1;
            }
            assert_eq!(
                has_group_info, snap.welcome_pending,
                "TORN SNAPSHOT (#987 P9): observed group_info={has_group_info} with \
                 welcome_pending={}. The writer only ever publishes or retracts BOTH \
                 halves in one transaction, so seeing one without the other means the \
                 read did not come from a single snapshot. A client in this state \
                 external-joins into its own inbound Welcome and is stranded \
                 permanently — while the Kani property `ExternalJoin => \
                 !welcome_pending` stays true and becomes vacuous.",
                snap.welcome_pending
            );
            tokio::task::yield_now().await;
        }
        done.store(true, Ordering::SeqCst);
        (saw_published, saw_retracted)
    });

    let (published, retracted) = reader.await.expect("reader");
    writer.await.expect("writer");
    assert!(
        published > 0 && retracted > 0,
        "the reader only ever saw one of the two states ({published} published, \
         {retracted} retracted) across every sample it was allowed, so it never \
         sampled the window a tear lives in — the fixture, not the property, is \
         what passed. Reaching this now means the writer stopped making progress, \
         not that the scheduler was unlucky: the reader keeps sampling until it \
         has seen both states."
    );
}

/// The Welcome flag is per device; a delivered Welcome is spent.
#[tokio::test]
async fn the_welcome_flag_is_scoped_to_the_asking_device() {
    let db = log_db().await;
    let conn = db.conn().await.expect("conn");
    insert_commit(&conn, CONV, 0).await;
    conn.execute(
        "INSERT INTO mls_welcome \
           (id, conversation_id, recipient_id, recipient_device_id, welcome_data, delivered) \
         VALUES ('w-other', ?1, 'u-other', 'd-other', X'00', 0)",
        libsql::params![CONV.to_string()],
    )
    .await
    .expect("other welcome");

    let snap = snapshot(&conn, &cheap_query(CONV), &who(), Some(true))
        .await
        .expect("snapshot");
    assert!(
        !snap.welcome_pending,
        "a Welcome addressed to another device must not gate THIS device's join"
    );

    conn.execute(
        "INSERT INTO mls_welcome \
           (id, conversation_id, recipient_id, recipient_device_id, welcome_data, delivered) \
         VALUES ('w-mine', ?1, ?2, ?3, X'00', 1)",
        libsql::params![CONV.to_string(), ME.to_string(), MY_DEVICE.to_string()],
    )
    .await
    .expect("delivered welcome");
    let snap = snapshot(&conn, &cheap_query(CONV), &who(), Some(true))
        .await
        .expect("snapshot");
    assert!(
        !snap.welcome_pending,
        "a Welcome already marked delivered is spent — gating on it would deadlock \
         a device that has already consumed its Welcome"
    );
}

#[tokio::test]
async fn a_contiguous_batch_is_served() {
    let db = log_db().await;
    let conn = db.conn().await.expect("conn");
    for epoch in 0..5 {
        insert_commit(&conn, CONV, epoch).await;
    }

    let snap = snapshot(&conn, &batch_query(CONV, 0), &who(), Some(true))
        .await
        .expect("snapshot");
    assert_eq!(
        snap.commits.iter().map(|c| c.epoch).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
    assert_eq!(snap.hole, None, "a contiguous batch reports no hole");
    assert_eq!(snap.pruned_below, Some(0), "nothing pruned, floor is epoch 0");
}

/// A hole is reported with both epochs, not refused.
#[tokio::test]
async fn a_hole_in_the_batch_is_reported() {
    let db = log_db().await;
    let conn = db.conn().await.expect("conn");
    for epoch in 0..6 {
        insert_commit(&conn, CONV, epoch).await;
    }
    // Remove a middle row: inserting past the head is impossible.
    conn.execute(
        "DELETE FROM mls_commit_log WHERE conversation_id = ?1 AND epoch = 3",
        libsql::params![CONV.to_string()],
    )
    .await
    .expect("punch a hole");

    let snap = snapshot(&conn, &batch_query(CONV, 0), &who(), Some(true))
        .await
        .expect("a damaged log must stay READABLE — refusing strands it forever");

    assert_eq!(
        snap.hole,
        Some(pollis_api::reads::CommitHole {
            expected: 3,
            found: 4
        }),
        "HOLE UNREPORTED (#987 P10): the batch skips epoch 3 and the snapshot did \
         not say so. Both epochs are named so an operator reading the warning can \
         find the gap without a query."
    );
    // The batch below the gap is still served.
    assert_eq!(
        snap.commits.iter().map(|c| c.epoch).collect::<Vec<_>>(),
        vec![0, 1, 2, 4, 5]
    );
    assert_eq!(
        snap.pruned_below,
        Some(0),
        "the floor did not move — this is damage, not a prune, and the two must \
         stay distinguishable"
    );
}

/// A retention prune leaves a contiguous batch starting above `since`, reported so the client rejoins.
#[tokio::test]
async fn a_retention_prune_surfaces_as_pruned_below_not_as_an_error() {
    let db = log_db().await;
    let conn = db.conn().await.expect("conn");
    for epoch in 0..8 {
        insert_commit(&conn, CONV, epoch).await;
    }
    conn.execute(
        "DELETE FROM mls_commit_log WHERE conversation_id = ?1 AND epoch < 5",
        libsql::params![CONV.to_string()],
    )
    .await
    .expect("prune");

    let snap = snapshot(&conn, &batch_query(CONV, 0), &who(), Some(true))
        .await
        .expect("a prune is not a torn read — it must be reported, not refused");
    assert_eq!(
        snap.commits.iter().map(|c| c.epoch).collect::<Vec<_>>(),
        vec![5, 6, 7],
        "the surviving batch is contiguous; only its floor moved"
    );
    assert_eq!(
        snap.hole, None,
        "a prune leaves a CONTIGUOUS batch — reporting a hole here would tell the \
         client its log is damaged when retention simply moved the floor"
    );
    assert_eq!(
        snap.pruned_below,
        Some(5),
        "PRUNE INVISIBLE (#987 P10): the batch starts at 5 while the caller asked \
         from 0, and without `pruned_below` the client cannot tell that from a \
         torn read — one means 'rejoin', the other means 'retry'"
    );
    assert_eq!(snap.head, 8, "head is MAX(epoch) + 1, unaffected by the prune");
}

/// An empty lineage is not a hole: empty batch, `pruned_below: None`.
#[tokio::test]
async fn an_empty_lineage_is_neither_a_hole_nor_a_prune() {
    let db = log_db().await;
    let conn = db.conn().await.expect("conn");

    let snap = snapshot(&conn, &batch_query("conv-nothing", 0), &who(), Some(true))
        .await
        .expect("an empty log is a valid, empty answer");
    assert!(snap.commits.is_empty());
    assert_eq!(snap.hole, None);
    assert_eq!(snap.pruned_below, None);
    assert_eq!(snap.head, 0);
}
