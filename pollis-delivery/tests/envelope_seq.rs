//! #2180: the database assigns every envelope's delivery sequence (migration
//! 000058), on local libsql. The same scenarios run against a real sqld over
//! hrana in `remote_sqld.rs`.

use std::sync::Arc;

mod common;
#[path = "support/seq_invariants.rs"]
mod seq_invariants;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("seq.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db
}

#[tokio::test]
async fn an_insert_takes_the_next_counter_value() {
    seq_invariants::an_insert_takes_the_next_counter_value(&**fresh().await).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_sends_to_one_conversation_are_dense_and_unique() {
    let db = fresh().await;
    seq_invariants::concurrent_sends_to_one_conversation_are_dense_and_unique(db.arc(), 24).await;
}

#[tokio::test]
async fn the_counter_outlives_gc_emptying_a_conversation() {
    seq_invariants::the_counter_outlives_gc_emptying_a_conversation(&**fresh().await).await;
}

#[tokio::test]
async fn a_teardown_restarts_the_sequence_with_its_cursors() {
    seq_invariants::a_teardown_restarts_the_sequence_with_its_cursors(&**fresh().await).await;
}

#[tokio::test]
async fn a_failed_insert_consumes_no_seq() {
    seq_invariants::a_failed_insert_consumes_no_seq(&**fresh().await).await;
}

#[tokio::test]
async fn an_explicit_seq_must_be_the_counter() {
    seq_invariants::an_explicit_seq_must_be_the_counter(&**fresh().await).await;
}

#[tokio::test]
async fn a_multi_row_insert_gets_consecutive_seqs() {
    seq_invariants::a_multi_row_insert_gets_consecutive_seqs(&**fresh().await).await;
}

#[tokio::test]
async fn without_the_triggers_a_send_fails_closed() {
    seq_invariants::without_the_triggers_a_send_fails_closed(&**fresh().await).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn old_and_new_ds_writers_interleave_safely() {
    let db = fresh().await;
    seq_invariants::old_and_new_ds_writers_interleave_safely(Arc::clone(&db.arc()), 12).await;
}
