//! #1500 — a pooled connection that hit a transport failure is never reused.
//!
//! libsql keeps a hrana stream's baton when a request on it fails at the HTTP
//! layer, so once Turso has expired the stream (or the request carrying a new
//! baton was lost), every later statement on that `Connection` sends the dead
//! baton and fails with `stream not found`. The pool used to park such a
//! connection again, so the next request to draw it failed the same way.
//!
//! Its own binary because recording a fault discards every connection checked
//! out at that moment, process-wide: run beside `conn_pool.rs`'s tests, it
//! would make their connection counts nondeterministic.

use axum::http::StatusCode;
use axum::response::IntoResponse as _;
use pollis_delivery::error::{is_transient_db_error, AppError};

#[path = "support/hrana_double.rs"]
mod hrana_double;
use hrana_double::{remote_db, start_double};

#[tokio::test]
async fn a_connection_whose_stream_died_is_not_parked_again() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    conn.prepare("SELECT 1").await.expect("prepare");

    // Turso expires the stream; the connection still holds its baton.
    double.expire_streams();
    let Err(err) = conn.prepare("SELECT 2").await else {
        panic!("a statement on the expired stream must fail");
    };
    let err = anyhow::Error::from(err);
    assert!(
        format!("{err:#}").contains("stream not found"),
        "unexpected error: {err:#}"
    );
    assert!(is_transient_db_error(&err), "{err:#}");

    // What a handler's `?` does, while the connection is still checked out.
    let response = AppError::from(err).into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(conn);

    // The next request gets a new connection, and it works.
    let conn = db.conn().await.expect("checkout");
    conn.execute("INSERT INTO t VALUES (1)", ())
        .await
        .expect("a fresh connection is not poisoned");
    assert_eq!(
        double.connections(),
        2,
        "the connection with the dead stream must be dropped, not reused"
    );
}

/// The fault discards only what was checked out when it happened: a connection
/// released before it is reused as normal.
#[tokio::test]
async fn a_connection_released_before_a_fault_is_still_reused() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    conn.execute("INSERT INTO t VALUES (1)", ())
        .await
        .expect("execute");
    drop(conn);

    pollis_delivery::db::note_transport_fault();

    let conn = db.conn().await.expect("checkout");
    conn.execute("INSERT INTO t VALUES (2)", ())
        .await
        .expect("execute");
    assert_eq!(double.connections(), 1, "a parked connection is not touched by a later fault");
}
