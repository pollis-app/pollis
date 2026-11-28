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
    let faulted_socket = served[0].peer;
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
    assert_ne!(
        served[0].peer, faulted_socket,
        "after a fault the next request goes out on a new socket too (#2179)"
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
    let socket_before = double.work()[0].peer;
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
    assert_ne!(
        served[0].peer, socket_before,
        "its stream is kept, but it sends on a new socket: the fault may have been the old one's (#2179)"
    );
}

/// #2182: a push-handle batch that fails transiently is written row by row on
/// a NEW stream and socket, not on the one that failed, so every recipient
/// still gets a handle.
#[tokio::test]
async fn a_transient_handle_batch_failure_retries_on_a_fresh_stream() {
    let _serial = SERIAL.lock().await;
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    // A read leaves a stream open, as before a fan-out.
    conn.query("SELECT 1", ()).await.expect("query");
    double.clear_served();
    double.fail_next(StatusCode::BAD_GATEWAY);
    let users: Vec<String> = ["u1", "u2", "u3"].iter().map(|u| u.to_string()).collect();
    let handles = pollis_delivery::push::mint_push_handles(&conn, &users, "conv", "dm").await;
    assert_eq!(handles.len(), 3, "every recipient still has a handle");

    let served = double.work();
    assert_eq!(served.len(), 4, "the failed batch, then one row each: {served:?}");
    assert!(served[0].had_baton, "the batch went out on the open stream: {served:?}");
    assert!(!served[1].had_baton, "the retry is on a new stream: {served:?}");
    assert_ne!(served[1].peer, served[0].peer, "and a new socket: {served:?}");
}
