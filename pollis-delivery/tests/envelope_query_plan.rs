//! The envelope fetch must use `seq` as an index RANGE per conversation, not
//! read each conversation and filter (#875), with each device's watermark a
//! NON-correlated subquery (#2181).
//!
//! The plan is taken of the SQL the production code sends
//! (`directory::envelope_fetch_sql`), not of a copy of it.

use pollis_delivery::directory::envelope_fetch_sql;

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

/// One conversation, two, and a full chunk (`BIND_CHUNK / 2`).
#[tokio::test]
async fn the_envelope_fetch_seeks_each_conversation_by_seq() {
    let db = db().await;
    for conversations in [1, 2, pollis_delivery::util::BIND_CHUNK / 2] {
        let p = plan(&db, &envelope_fetch_sql(conversations)).await;
        let seeks = p
            .matches("USING INDEX idx_envelope_conv_seq (conversation_id=? AND seq>?)")
            .count();
        assert_eq!(
            seeks, conversations,
            "every conversation must be an index range on (conversation_id, seq) — \
             otherwise each 'anything new?' reads the conversation's whole retained \
             history. Plan for {conversations}:\n  {p}"
        );
        assert!(
            !p.contains("CORRELATED"),
            "a watermark is correlated again; it must be a subquery over bound \
             parameters only. Plan for {conversations}:\n  {p}"
        );
        assert!(
            !p.contains("SCAN message_envelope"),
            "the envelope table is scanned. Plan for {conversations}:\n  {p}"
        );
    }
}

/// The pre-#1049 form, the negative control: a watermark correlated with the
/// outer row cannot bound the range, so the guard above can tell the two apart.
#[tokio::test]
async fn the_correlated_form_is_still_measurably_worse() {
    let db = db().await;
    let correlated = plan(
        &db,
        "SELECT id FROM message_envelope \
         WHERE conversation_id IN (?3, ?4) \
           AND seq > COALESCE((SELECT last_seq FROM conversation_watermark \
                WHERE conversation_id = message_envelope.conversation_id \
                  AND user_id = ?1 AND device_id = ?2), 0)",
    )
    .await;
    assert!(
        correlated.contains("CORRELATED") && !correlated.contains("seq>?"),
        "the correlated form now plans as well as the bound one — this guard's \
         premise no longer holds and it needs rewriting. Plan was:\n  {correlated}"
    );
}
