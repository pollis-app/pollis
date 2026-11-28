//! #1500 — a pooled connection that hit a transport failure is never reused.
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

/// A fault is process-wide, so these tests take turns: one test's fault must not
/// drop another's checked-out connection.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A read runs as one `/v3/cursor` (#2178), so a stream that expires under a
/// read surfaces there: the error is transient (503), and the connection is
/// dropped, not parked again with its dead baton.
#[tokio::test]
async fn a_connection_whose_stream_died_under_a_read_is_not_parked_again() {
    let _serial = SERIAL.lock().await;
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    // A cursor leaves its stream open, as on Turso.
    conn.query("SELECT 1", ()).await.expect("query");

    double.expire_streams();
    double.clear_served();
    let Err(err) = conn.query("SELECT 2", ()).await else {
        panic!("a read on the expired stream must fail");
    };
    let served = double.work();
    assert_eq!(served.len(), 1, "{served:?}");
    assert_eq!(served[0].path, "/v3/cursor", "the read failed on its cursor: {served:?}");
    assert!(served[0].had_baton, "{served:?}");
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

    double.clear_served();
    let conn = db.conn().await.expect("checkout");
    conn.query("SELECT 3", ())
        .await
        .expect("a fresh connection is not poisoned");
    let served = double.work();
    assert!(
        !served[0].had_baton,
        "the connection with the dead stream must be dropped, not reused: {served:?}"
    );
}

/// The same on the `/v3/pipeline` path a write takes.
#[tokio::test]
async fn a_connection_whose_stream_died_under_a_write_is_not_parked_again() {
    let _serial = SERIAL.lock().await;
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    conn.query("SELECT 1", ()).await.expect("query");

    double.expire_streams();
    let Err(err) = conn.execute("INSERT INTO t VALUES (1)", ()).await else {
        panic!("a write on the expired stream must fail");
    };
    let err = anyhow::Error::from(err);
    assert!(is_transient_db_error(&err), "{err:#}");
    let response = AppError::from(err).into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(conn);

    double.clear_served();
    let conn = db.conn().await.expect("checkout");
    conn.execute("INSERT INTO t VALUES (2)", ())
        .await
        .expect("a fresh connection is not poisoned");
    let served = double.work();
    assert!(!served[0].had_baton, "{served:?}");
}

/// The fault discards only connections checked out when it happened.
#[tokio::test]
async fn a_connection_released_before_a_fault_is_still_reused() {
    let _serial = SERIAL.lock().await;
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    conn.query("SELECT 1", ()).await.expect("query");
    drop(conn);

    pollis_delivery::db::note_transport_fault();

    double.clear_served();
    let conn = db.conn().await.expect("checkout");
    conn.query("SELECT 2", ()).await.expect("query");
    let served = double.work();
    assert!(
        served[0].had_baton,
        "a parked connection is not touched by a later fault: {served:?}"
    );
}
