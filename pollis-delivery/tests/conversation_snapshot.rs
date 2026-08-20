//! #987 acceptance gates P9 (snapshot atomicity) and P10 (batch contiguity).
//!
//! These are the two properties that made the MLS control-plane read ONE
//! endpoint rather than four, and neither fails loudly on its own — which is
//! why they get tests rather than a comment.
//!
//! # P9 — a torn GroupInfo/Welcome pair must not be observable
//!
//! `submit_commit` writes the commit, its GroupInfo and its Welcomes in one
//! `IMMEDIATE` transaction. The client's join gate reads
//! `ExternalJoin ⟹ !welcome_pending`: it may external-join only when no Welcome
//! is inbound for it, because joining while one is in flight strands the device
//! permanently — its own Welcome carries an epoch the join has overwritten.
//!
//! That gate is meaningful only if the two facts come from ONE snapshot. Split
//! across two reads, a caller can land between them and see a GroupInfo without
//! the Welcome written beside it. The Kani property does not catch that; it
//! stays *true* and becomes vacuous.
//!
//! [`a_split_read_can_observe_a_torn_pair`] shows the hazard is real, with no
//! race at all: two separate reads with a write in between tear every time.
//! [`torn_group_info_and_welcome_is_not_observable`] then drives `snapshot`
//! against a writer flipping the pair on and off as fast as it can, and asserts
//! the two halves always agree.
//!
//! # P10 — a hole must be a FACT, and distinguishable from a prune
//!
//! The client's response to a non-contiguous batch is
//! `forget_local_mls_group_at`: it drops the device's MLS state for that group
//! and external-joins onto the head. That is the recovery, not the danger — a
//! device cannot advance a local group across an epoch that does not exist, and
//! `flows::adversarial::epoch_gap_recovers_via_external_join` pins exactly this
//! behaviour. What makes it dangerous is a hole that is not real: when the
//! batch, the head and the retention floor came off separate statements, a
//! reader could see a gap the log never had and destroy live state recovering
//! from nothing.
//!
//! So the endpoint reports rather than refuses, and reports from ONE
//! transaction. Refusing would be worse in the other direction: a genuinely
//! damaged conversation would become permanently unreadable, every read erroring
//! forever with no path back.
//!
//! A *retention prune* (#539) is the third case and must not be confused with
//! either: it is a legitimate reason for a batch to start above what was asked
//! for, and `pruned_below` — read in that same transaction — is what separates
//! "the floor moved" from "a row is missing".
//!
//! Note the log DB refuses a forward gap on INSERT
//! (`trg_mls_commit_log_no_forward_gap`, migration 000005), so these fixtures
//! build a hole the only way one can actually appear: by removing a row that was
//! legitimately written — the shape a mis-scoped prune or a partial restore
//! produces.

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

/// Everything but the commit batch: heads, the GroupInfo head, the Welcome flag.
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

/// One commit row at `epoch`. Generation 0 is the pre-migration lineage every
/// conversation starts in.
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

/// Publish a GroupInfo AND this device's Welcome in ONE transaction — the shape
/// `submit_commit` uses.
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

/// Retract both halves, also in ONE transaction. Together with [`publish_pair`]
/// this gives the conversation exactly two legal states — both present, or
/// neither — so ANY observation of one without the other is a torn read.
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

// ── P9 ───────────────────────────────────────────────────────────────────────

/// The hazard, with no race: two SEPARATE reads with a write in between observe
/// a GroupInfo that no longer has its Welcome beside it.
///
/// This is what the client did before #987 — one query for the published
/// GroupInfo, another for the pending-Welcome flag — and it is why the read had
/// to become one endpoint rather than two. A device that lands here
/// external-joins into its own inbound Welcome and is stranded permanently.
///
/// Deterministic on purpose: the point is not that a tear is *likely* but that
/// nothing about split reads prevents it.
#[tokio::test]
async fn a_split_read_can_observe_a_torn_pair() {
    let db = log_db().await;
    let conn = db.conn().await.expect("conn");
    insert_commit(&conn, CONV, 0).await;

    publish_pair(&conn, 1).await;

    // READ ONE: the GroupInfo is published, so an external join looks available
    // as far as this half of the question goes.
    let saw_group_info = group_info_present(&conn).await;
    assert!(saw_group_info, "fixture: the pair was published");

    // The world moves between the two reads.
    retract_pair(&conn).await;

    // READ TWO: no Welcome pending. The caller now holds two facts that were
    // never true at the same instant.
    let saw_welcome = welcome_pending(&conn).await;

    assert!(
        saw_group_info && !saw_welcome,
        "the split read did not tear, so this test is no longer demonstrating \
         the hazard the one-snapshot endpoint exists to remove"
    );
}

/// The gate itself: `snapshot` reads both halves in ONE transaction, so it can
/// never produce the pair above — no matter how fast the writer flips between
/// the two legal states.
///
/// Multi-threaded on purpose. On the default current-thread runtime the writer
/// and reader interleave only at await points, which makes "did the reader
/// actually sample the window?" depend on the scheduler rather than on real
/// concurrency; the assertion at the end refuses to pass without having sampled
/// both states, so a degenerate interleaving fails loudly instead of quietly
/// proving nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn torn_group_info_and_welcome_is_not_observable() {
    let db = log_db().await;
    {
        let conn = db.conn().await.expect("conn");
        insert_commit(&conn, CONV, 0).await;
    }

    // The READER decides when the run ends, so it can never be scheduled after
    // the writer has already finished and sample nothing.
    let done = Arc::new(AtomicBool::new(false));

    let writer_db = Arc::clone(&*db);
    let writer_done = Arc::clone(&done);
    let writer = tokio::spawn(async move {
        let conn = writer_db.conn().await.expect("writer conn");
        let mut epoch = 1i64;
        while !writer_done.load(Ordering::SeqCst) {
            publish_pair(&conn, epoch).await;
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
        for _ in 0..2_000 {
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
         {retracted} retracted), so it never sampled the window a tear lives in — \
         the fixture, not the property, is what passed"
    );
}

/// The Welcome flag is per DEVICE: a Welcome addressed to somebody else must NOT
/// suppress this device's external join, and a Welcome already delivered is
/// spent.
///
/// Asserted because the cheap way to satisfy the test above is to report
/// `welcome_pending: true` whenever any Welcome row exists — which would deadlock
/// every device in a conversation behind one undelivered row.
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

// ── P10 ──────────────────────────────────────────────────────────────────────

/// A contiguous batch is served.
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

/// The P10 regression: a hole is DETECTED and reported, with both epochs.
///
/// Not refused — see the module header. The client rebuilds from a reported
/// hole, which is the correct outcome for a log that really is missing a row;
/// what it must never do is rebuild from a hole that is not there, and the
/// single transaction is what rules that out.
#[tokio::test]
async fn a_hole_in_the_batch_is_reported() {
    let db = log_db().await;
    let conn = db.conn().await.expect("conn");
    for epoch in 0..6 {
        insert_commit(&conn, CONV, epoch).await;
    }
    // Remove a MIDDLE row. Inserting past the head is already impossible
    // (`trg_mls_commit_log_no_forward_gap`), so this is how a hole actually
    // arises: something removed a row that had been legitimately written.
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
    // The batch itself is still served: the client needs the commits BELOW the
    // gap to ingest anything it had not yet seen there before it rebuilds.
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

/// The other half of P10, and the reason "refuse anything that starts high" is
/// the WRONG fix: a genuine retention prune (#539) leaves a contiguous batch
/// that simply begins above `since`, and the client must be told so — that is
/// its cue to drop the local group and rejoin rather than sit on a permanently
/// unfillable gap.
#[tokio::test]
async fn a_retention_prune_surfaces_as_pruned_below_not_as_an_error() {
    let db = log_db().await;
    let conn = db.conn().await.expect("conn");
    for epoch in 0..8 {
        insert_commit(&conn, CONV, epoch).await;
    }
    // Retention collected everything below epoch 5.
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

/// An empty lineage is not a hole. Asked from `since` with no rows at all, the
/// answer is an empty batch and `pruned_below: None` — "nothing here", which is
/// what a brand-new conversation looks like and must not be mistaken for either
/// a prune or a tear.
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
