//! #2178 — a remote read is ONE HTTP request to Turso.
//!
//! Upstream libsql sends a `/v3/pipeline` describe before every query's
//! `/v3/cursor`, doubling the round trips of every DS read. `patches/libsql`
//! drops it (see its Cargo.toml header). These tests count the requests a read
//! makes, so a libsql upgrade, or deleting the patch, that brings the describe
//! back fails here instead of silently doubling the DS's latency.

#[path = "support/hrana_double.rs"]
mod hrana_double;
use hrana_double::{remote_db, start_double};

/// A SELECT on a fresh connection: one `/v3/cursor`, nothing before it.
#[tokio::test]
async fn a_remote_select_is_one_cursor_request() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    let mut rows = conn
        .query("SELECT 1 WHERE ?1 = 1", libsql::params![1])
        .await
        .expect("query");
    assert!(rows.next().await.expect("step").is_none());

    let served = double.served();
    assert_eq!(
        served.iter().map(|s| s.path).collect::<Vec<_>>(),
        ["/v3/cursor"],
        "a remote SELECT must be exactly one /v3/cursor request, with no /v3/pipeline \
         describe before it (#2178): {served:?}"
    );
}

/// Every read after the first continues the same stream with one request each.
#[tokio::test]
async fn consecutive_remote_selects_cost_one_request_each() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    for i in 0..5 {
        conn.query("SELECT ?1", libsql::params![i]).await.expect("query");
    }

    let served = double.served();
    assert_eq!(served.len(), 5, "{served:?}");
    assert!(served.iter().all(|s| s.path == "/v3/cursor"), "{served:?}");
    assert!(
        served.iter().all(|s| !s.kinds.contains(&"describe")),
        "{served:?}"
    );
}

/// Inside a write transaction (the commit path's Rejected branch reads there):
/// BEGIN, then each SELECT as one cursor on the transaction's stream, no describe.
#[tokio::test]
async fn a_select_inside_a_transaction_is_one_cursor_request() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    let tx = pollis_delivery::db::write_tx(&conn).await.expect("begin");
    double.clear_served();
    tx.query("SELECT 1", ()).await.expect("query");
    tx.query("SELECT 2", ()).await.expect("query");

    let served = double.served();
    assert_eq!(
        served.iter().map(|s| s.path).collect::<Vec<_>>(),
        ["/v3/cursor", "/v3/cursor"],
        "{served:?}"
    );
    assert!(served.iter().all(|s| s.had_baton), "on the transaction's stream: {served:?}");
    tx.rollback().await.expect("rollback");
}

/// `prepare` alone sends nothing; the statement runs when it is executed.
#[tokio::test]
async fn a_remote_prepare_sends_nothing() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    let stmt = conn.prepare("SELECT 1").await.expect("prepare");
    assert!(double.served().is_empty(), "{:?}", double.served());
    stmt.query(()).await.expect("query");
    assert_eq!(double.served().len(), 1, "{:?}", double.served());
}
