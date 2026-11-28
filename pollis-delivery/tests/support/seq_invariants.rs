//! The delivery-sequence invariants (#1087) as migration 000058's triggers
//! enforce them (#2180), written once over a [`Db`] so the same scenarios run
//! on local libsql (`envelope_seq.rs`, in CI) and on a real sqld over hrana
//! (`remote_sqld.rs`), where concurrency is Turso's, not a local file's.
//!
//! Each scenario takes a database with the full single-DB schema and nothing
//! else, and uses conversation ids of its own.

#![allow(dead_code)]

use std::sync::Arc;

use pollis_delivery::db::Db;
use pollis_delivery::messages::{apply_send_message, SendMessageBody};
use pollis_delivery::writes::WriteOutcome;

/// `base64(SHA-256("fixture-capability"))`: any non-empty capability will do.
const CAP: &str = "MB8lyVjsEQamFuSzLTRZBVKGFQQ8nXoBjHFF7kvNuAo=";

pub fn send(id: &str, conversation_id: &str) -> SendMessageBody {
    SendMessageBody {
        id: id.to_string(),
        conversation_id: conversation_id.to_string(),
        sender_id: Some("sealed".to_string()),
        ciphertext: "mls:00".to_string(),
        reply_to_id: None,
        sent_at: chrono::Utc::now().to_rfc3339(),
        sealed: 1,
        generation: None,
        epoch: None,
        push_to: None,
        push_mentions: None,
        delete_token_hash: Some(CAP.to_string()),
    }
}

/// A send through the DS's real write path (the no-auth form, which skips
/// only the membership check).
pub async fn send_one(db: &Db, id: &str, conversation_id: &str) {
    let conn = db.conn().await.expect("conn");
    let out = apply_send_message(&conn, None, &send(id, conversation_id))
        .await
        .expect("send");
    assert!(matches!(out, WriteOutcome::Ok), "{id}: {out:?}");
}

async fn scalar(db: &Db, sql: &str, conversation_id: &str) -> Option<i64> {
    let conn = db.conn().await.expect("conn");
    let mut rows = conn
        .query(sql, libsql::params![conversation_id.to_string()])
        .await
        .expect("query");
    rows.next()
        .await
        .expect("row")
        .and_then(|r| r.get::<Option<i64>>(0).expect("value"))
}

pub async fn counter(db: &Db, conversation_id: &str) -> Option<i64> {
    scalar(
        db,
        "SELECT next_seq FROM conversation_seq WHERE conversation_id = ?1",
        conversation_id,
    )
    .await
}

/// Every stored envelope's seq in `conversation_id`, ascending (NULL as 0).
pub async fn seqs(db: &Db, conversation_id: &str) -> Vec<i64> {
    let conn = db.conn().await.expect("conn");
    let mut rows = conn
        .query(
            "SELECT COALESCE(seq, 0) FROM message_envelope WHERE conversation_id = ?1 ORDER BY seq",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("query");
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("row") {
        out.push(row.get::<i64>(0).expect("seq"));
    }
    out
}

/// A send with no seq takes the counter's next value, starting at 1.
pub async fn an_insert_takes_the_next_counter_value(db: &Db) {
    send_one(db, "next-1", "next").await;
    send_one(db, "next-2", "next").await;
    assert_eq!(seqs(db, "next").await, [1, 2]);
    assert_eq!(counter(db, "next").await, Some(2));
}

/// `n` sends to ONE conversation at once, each on its own connection: the
/// seqs are exactly 1..=n, none repeated, none skipped, and the counter is n.
pub async fn concurrent_sends_to_one_conversation_are_dense_and_unique(db: Arc<Db>, n: usize) {
    let barrier = Arc::new(tokio::sync::Barrier::new(n));
    let mut tasks = Vec::new();
    for i in 0..n {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            let conn = db.conn().await.expect("conn");
            barrier.wait().await;
            // Turso serializes writers; a writer that finds the lock busy is
            // told so (a 503 to a client, which retries). Retry it here the
            // same way: nothing may be lost or doubled either way.
            loop {
                match apply_send_message(&conn, None, &send(&format!("dense-{i}"), "dense")).await {
                    Ok(WriteOutcome::Ok) => break,
                    Ok(other) => panic!("send {i}: {other:?}"),
                    Err(e) if pollis_delivery::error::is_transient_db_error(&e) => {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    Err(e) => panic!("send {i}: {e:#}"),
                }
            }
        }));
    }
    for task in tasks {
        task.await.expect("task");
    }
    let got = seqs(db.as_ref(), "dense").await;
    let want: Vec<i64> = (1..=n as i64).collect();
    assert_eq!(got, want, "concurrent sends must get exactly 1..={n}");
    assert_eq!(counter(db.as_ref(), "dense").await, Some(n as i64));
}

/// Envelope GC deletes rows, never the counter: after it empties a
/// conversation, the next send continues above every cursor (#692).
pub async fn the_counter_outlives_gc_emptying_a_conversation(db: &Db) {
    for i in 1..=3 {
        send_one(db, &format!("gc-{i}"), "gc").await;
    }
    // What GC does once every device has fetched past them.
    db.conn()
        .await
        .unwrap()
        .execute("DELETE FROM message_envelope WHERE conversation_id = 'gc'", ())
        .await
        .unwrap();
    send_one(db, "gc-4", "gc").await;
    assert_eq!(seqs(db, "gc").await, [4], "never 1 again: that is below every cursor");
}

/// A teardown removes the counter with the cursors, so the id starts over.
pub async fn a_teardown_restarts_the_sequence_with_its_cursors(db: &Db) {
    send_one(db, "td-1", "td").await;
    send_one(db, "td-2", "td").await;
    let conn = db.conn().await.unwrap();
    pollis_delivery::teardown::purge_conversation_rows(&conn, "td")
        .await
        .expect("teardown");
    drop(conn);
    assert_eq!(counter(db, "td").await, None);
    send_one(db, "td-3", "td").await;
    assert_eq!(seqs(db, "td").await, [1]);
}

/// An insert that fails (here a duplicate id) consumes no seq: the trigger's
/// bump is part of the failed statement and rolls back with it.
pub async fn a_failed_insert_consumes_no_seq(db: &Db) {
    send_one(db, "fail-1", "fail").await;
    let conn = db.conn().await.unwrap();
    let dup = apply_send_message(&conn, None, &send("fail-1", "fail")).await;
    assert!(dup.is_err(), "a second row under one id is refused: {dup:?}");
    drop(conn);
    assert_eq!(counter(db, "fail").await, Some(1), "the failed insert took no seq");
    send_one(db, "fail-2", "fail").await;
    assert_eq!(seqs(db, "fail").await, [1, 2], "no gap");
}

/// An explicit seq is accepted only when it is the counter's current value:
/// the shape of the DS deployed before 000058 (bump, then insert what it got).
/// Anything else, a `MAX(seq)+1` or a copied seq, is refused by the database.
pub async fn an_explicit_seq_must_be_the_counter(db: &Db) {
    send_one(db, "ex-1", "ex").await;
    let conn = db.conn().await.unwrap();
    let insert = |id: &str, seq: i64| {
        format!(
            "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at, seq) \
             VALUES ('{id}', 'ex', 'sealed', 'ct', '2026-01-01', {seq})"
        )
    };
    for bad in [3, 0, -1] {
        let refused = conn.execute(&insert(&format!("ex-bad{bad}"), bad), ()).await;
        assert!(refused.is_err(), "seq {bad} is not the counter (1): stored anyway");
    }
    // The counter's value, already taken by ex-1: the unique index's refusal.
    let refused = conn.execute(&insert("ex-dup", 1), ()).await;
    assert!(refused.is_err(), "a seq already in use is refused (idx_envelope_conv_seq)");
    // A conversation with no counter row refuses any explicit seq.
    let refused = conn
        .execute(
            "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at, seq) \
             VALUES ('ex-none', 'ex-nocounter', 'sealed', 'ct', '2026-01-01', 1)",
            (),
        )
        .await;
    assert!(refused.is_err(), "no counter, no explicit seq");
    // The old DS's shape: bump, then insert exactly that value.
    conn.execute(
        "UPDATE conversation_seq SET next_seq = next_seq + 1 WHERE conversation_id = 'ex'",
        (),
    )
    .await
    .unwrap();
    conn.execute(&insert("ex-2", 2), ()).await.expect("the counter's value is accepted");
    drop(conn);
    send_one(db, "ex-3", "ex").await;
    assert_eq!(seqs(db, "ex").await, [1, 2, 3]);
}

/// One INSERT of several rows gives each its own consecutive seq.
pub async fn a_multi_row_insert_gets_consecutive_seqs(db: &Db) {
    send_one(db, "multi-0", "multi").await;
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES \
             ('multi-1', 'multi', 's', 'ct', '2026-01-01'), \
             ('multi-2', 'multi', 's', 'ct', '2026-01-01'), \
             ('multi-3', 'multi', 's', 'ct', '2026-01-01')",
            (),
        )
        .await
        .unwrap();
    assert_eq!(seqs(db, "multi").await, [1, 2, 3, 4]);
}

/// Without either trigger a send stores NOTHING and errors (so the client keeps
/// the message and retries), and the startup check refuses. Never an envelope
/// with no seq, acknowledged and then delivered to nobody.
pub async fn without_the_triggers_a_send_fails_closed(db: &Db) {
    for trigger in ["message_envelope_assign_seq", "message_envelope_seq_from_counter"] {
        let conn = db.conn().await.unwrap();
        pollis_delivery::messages::require_seq_triggers(&conn)
            .await
            .expect("the schema has both");
        let (_, _, migration) = pollis_schema::POST_BASELINE_MIGRATIONS
            .iter()
            .find(|(v, _, _)| *v == 58)
            .expect("000058");
        conn.execute(&format!("DROP TRIGGER {trigger}"), ()).await.unwrap();

        let conversation = format!("closed-{trigger}");
        let out = apply_send_message(&conn, None, &send(&format!("{conversation}-m"), &conversation)).await;
        assert!(out.is_err(), "without {trigger} a send must fail: {out:?}");
        assert!(
            pollis_delivery::messages::require_seq_triggers(&conn).await.is_err(),
            "without {trigger} the DS must refuse to start"
        );
        drop(conn);
        assert!(seqs(db, &conversation).await.is_empty(), "without {trigger} nothing is stored");
        assert_eq!(counter(db, &conversation).await, None);

        db.conn().await.unwrap().execute_batch(migration).await.expect("restore");
    }
}

/// The DS deployed before 000058 (its own BEGIN IMMEDIATE, counter bump,
/// explicit-seq INSERT) and this one (one statement, trigger-assigned), both
/// writing one conversation at once: every seq is unique and dense.
pub async fn old_and_new_ds_writers_interleave_safely(db: Arc<Db>, each: usize) {
    let mut tasks = Vec::new();
    for i in 0..each {
        let db_old = Arc::clone(&db);
        tasks.push(tokio::spawn(async move {
            let conn = db_old.conn().await.unwrap();
            loop {
                match old_ds_send(&conn, &format!("old-{i}"), "mixed").await {
                    Ok(()) => break,
                    Err(e) if pollis_delivery::error::is_transient_db_error(&e) => {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    Err(e) => panic!("old DS send {i}: {e:#}"),
                }
            }
        }));
        let db_new = Arc::clone(&db);
        tasks.push(tokio::spawn(async move {
            let conn = db_new.conn().await.unwrap();
            loop {
                match apply_send_message(&conn, None, &send(&format!("new-{i}"), "mixed")).await {
                    Ok(WriteOutcome::Ok) => break,
                    Ok(other) => panic!("new DS send {i}: {other:?}"),
                    Err(e) if pollis_delivery::error::is_transient_db_error(&e) => {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    Err(e) => panic!("new DS send {i}: {e:#}"),
                }
            }
        }));
    }
    for task in tasks {
        task.await.expect("task");
    }
    let total = 2 * each as i64;
    assert_eq!(seqs(db.as_ref(), "mixed").await, (1..=total).collect::<Vec<_>>());
    assert_eq!(counter(db.as_ref(), "mixed").await, Some(total));
}

/// What `apply_send_message` did before #2180, verbatim in shape.
async fn old_ds_send(conn: &libsql::Connection, id: &str, conversation_id: &str) -> anyhow::Result<()> {
    let tx = pollis_delivery::db::write_tx(conn).await?;
    let seq: i64 = {
        let mut rows = tx
            .query(
                "INSERT INTO conversation_seq (conversation_id, next_seq) VALUES (?1, 1) \
                 ON CONFLICT(conversation_id) DO UPDATE SET next_seq = next_seq + 1 \
                 RETURNING next_seq",
                libsql::params![conversation_id.to_string()],
            )
            .await?;
        let row = rows.next().await?.expect("counter row");
        row.get(0)?
    };
    tx.execute(
        "INSERT INTO message_envelope (conversation_id, seq, id, sender_id, ciphertext, sent_at, sealed, delete_token_hash) \
         VALUES (?1, ?2, ?3, 'sealed', 'mls:00', ?4, 1, ?5)",
        libsql::params![conversation_id.to_string(), seq, id.to_string(), chrono::Utc::now().to_rfc3339(), CAP],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
