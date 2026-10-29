//! The envelope fetch must use `sent_at` as an index RANGE, not read the
//! conversation and filter.
//!
//! `SEARCH … (conversation_id=? AND sent_at>?)` is the fix; a correlated subquery is the defect (#875).

mod common;

/// The real schema and indexes: the plan is a property of the indexes.
async fn db() -> common::TempDb {
    let db = common::TempDb::open("plan.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db
}

async fn plan(db: &common::TempDb, sql: &str) -> String {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
        .await
        .expect("explain");
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("row") {
        out.push(row.get::<String>(3).expect("detail"));
    }
    out.join(" | ")
}

const FIXED: &str = "SELECT conversation_id, id, sender_id, ciphertext, reply_to_id, \
     target_message_id, sent_at, type \
     FROM message_envelope \
     WHERE (conversation_id = ?1 AND sent_at > ?2) OR (conversation_id = ?3 AND sent_at > ?4) \
     ORDER BY conversation_id ASC, sent_at ASC, id ASC";

/// The pre-#1049 form, the negative control.
const CORRELATED: &str = "SELECT conversation_id, id, sender_id, ciphertext, reply_to_id, \
     target_message_id, sent_at, type \
     FROM message_envelope \
     WHERE conversation_id IN (?3, ?4) \
       AND sent_at > COALESCE((SELECT last_fetched_at FROM conversation_watermark \
            WHERE conversation_id = message_envelope.conversation_id \
              AND user_id = ?1 AND device_id = ?2), '') \
     ORDER BY conversation_id ASC, sent_at ASC, id ASC";

#[tokio::test]
async fn the_envelope_fetch_bounds_sent_at_with_the_index() {
    let db = db().await;
    let p = plan(&db, FIXED).await;

    assert!(
        p.contains("sent_at>?"),
        "the envelope fetch no longer uses sent_at as an index range — it is \
         reading the conversation's whole retained history and filtering. That \
         is billed rows on every 'anything new?' from every client. Plan was:\n  {p}"
    );
    assert!(
        !p.contains("CORRELATED"),
        "the watermark is correlated again; bind each conversation's own \
         watermark instead (see fetch_watermarks). Plan was:\n  {p}"
    );
}

#[tokio::test]
async fn the_correlated_form_is_still_measurably_worse() {
    let db = db().await;
    let correlated = plan(&db, CORRELATED).await;

    assert!(
        correlated.contains("CORRELATED") && !correlated.contains("sent_at>?"),
        "the correlated form now plans as well as the bound one — this guard's \
         premise no longer holds and it needs rewriting. Plan was:\n  {correlated}"
    );
}
