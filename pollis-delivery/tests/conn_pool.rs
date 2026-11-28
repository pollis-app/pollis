//! #777 — the DS connection pool over the remote (hrana/HTTP) path, via a test double.
//!
//! Two things are pooled and counted apart: libsql `Connection`s (each with a
//! hrana stream: a request "continues a stream" when it carries a baton) and,
//! under them, TCP sockets (`Double::connections`), which every connection of
//! one database shares (#2179).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use pollis_delivery::db::Db;

#[path = "support/hrana_double.rs"]
mod hrana_double;
use hrana_double::{remote_db, start_double};

/// Twenty sequential requests must cost one TCP + TLS connection, not twenty.
#[tokio::test]
async fn sequential_requests_reuse_one_connection() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    for i in 0..20 {
        let conn = db.conn().await.expect("checkout");
        conn.execute(&format!("INSERT INTO t VALUES ({i})"), ())
            .await
            .expect("execute");
    }

    assert_eq!(
        double.connections(),
        1,
        "20 sequential requests must share one connection, not open one each"
    );
}

/// A checked-out connection is removed from the pool: distinct streams per holder.
#[tokio::test]
async fn concurrent_holders_never_share_a_connection() {
    let (double, url) = start_double().await;
    let db = Arc::new(remote_db(&url).await);

    let holders = 8;
    let barrier = Arc::new(tokio::sync::Barrier::new(holders));
    let mut tasks = Vec::new();
    for i in 0..holders {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            let conn = db.conn().await.expect("checkout");
            barrier.wait().await;
            conn.execute(&format!("INSERT INTO holders VALUES ({i})"), ())
                .await
                .expect("execute");
        }));
    }
    for task in tasks {
        task.await.expect("task");
    }

    let mut streams = HashSet::new();
    for i in 0..holders {
        let found = double.streams_containing(&format!("INSERT INTO holders VALUES ({i})"));
        assert_eq!(found.len(), 1, "holder {i} wrote on exactly one stream");
        streams.insert(found[0].0);
    }
    assert_eq!(
        streams.len(),
        holders,
        "concurrent holders must not share a hrana stream"
    );
    let sockets = double.connections();
    assert!(sockets <= holders, "{sockets} sockets for {holders} holders");

    let conn = db.conn().await.expect("checkout");
    conn.execute("INSERT INTO after VALUES (1)", ())
        .await
        .expect("execute");
    assert_eq!(
        double.connections(),
        sockets,
        "a later request reuses a pooled socket, it does not dial a new one"
    );
}

/// A connection parked past the idle window is discarded: its hrana stream has
/// expired. The socket under it is not (#2179).
#[tokio::test]
async fn a_stale_parked_connection_is_never_reused() {
    let (double, url) = start_double().await;
    let db = remote_db(&url)
        .await
        .with_max_idle(Duration::from_millis(1000));

    let conn = db.conn().await.expect("checkout");
    // A read leaves its stream open, as on Turso.
    conn.query("SELECT 1", ()).await.expect("query");
    drop(conn);

    let conn = db.conn().await.expect("checkout");
    double.clear_served();
    conn.query("SELECT 2", ()).await.expect("query");
    drop(conn);
    assert!(
        double.work()[0].had_baton,
        "a fresh parked connection is reused: {:?}",
        double.work()
    );

    tokio::time::sleep(Duration::from_millis(1300)).await;

    double.clear_served();
    let conn = db.conn().await.expect("checkout");
    // Discarding the stale connection sends its stream's `close` in the
    // background; let it land, so it does not race the next request for a socket.
    tokio::time::sleep(Duration::from_millis(50)).await;
    conn.query("SELECT 3", ()).await.expect("query");
    assert!(
        !double.work()[0].had_baton,
        "a connection parked past the idle window must be discarded, not reused: {:?}",
        double.work()
    );
    assert_eq!(
        double.connections(),
        1,
        "discarding a stale stream keeps the warm socket under it (#2179)"
    );
}

/// #1500: age counts from last use; an untouched checkout must not reset it.
#[tokio::test]
async fn an_untouched_checkout_does_not_make_a_connection_look_fresh() {
    let (double, url) = start_double().await;
    let db = remote_db(&url)
        .await
        .with_max_idle(Duration::from_millis(200));

    let conn = db.conn().await.expect("checkout");
    conn.query("SELECT 1", ()).await.expect("query");
    drop(conn);

    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(80)).await;
        drop(db.conn().await.expect("checkout"));
    }

    double.clear_served();
    let conn = db.conn().await.expect("checkout");
    conn.query("SELECT 2", ()).await.expect("query");
    assert!(
        !double.work()[0].had_baton,
        "a connection last used longer ago than the window is not reused, however often it \
         was checked out: {:?}",
        double.work()
    );
}

/// #2179: age counts from the last STATEMENT, not from release. A guard that
/// ran one, then sat idle while still checked out, parks as old as that
/// statement: the stream under it has been idle all along.
#[tokio::test]
async fn a_touched_guard_held_idle_does_not_park_as_fresh() {
    let (double, url) = start_double().await;
    let db = remote_db(&url)
        .await
        .with_max_idle(Duration::from_millis(150));

    let conn = db.conn().await.expect("checkout");
    conn.query("SELECT 1", ()).await.expect("query");
    // Held past the window after its last statement, then released.
    tokio::time::sleep(Duration::from_millis(250)).await;
    drop(conn);

    double.clear_served();
    let conn = db.conn().await.expect("checkout");
    conn.query("SELECT 2", ()).await.expect("query");
    assert!(
        !double.work()[0].had_baton,
        "a guard idle past the window while checked out must not be handed out as fresh: {:?}",
        double.work()
    );
}

/// #2179: a connection whose stream went stale is replaced, but the
/// replacement sends on the same warm socket: no handshake after a lull.
#[tokio::test]
async fn connections_of_one_database_share_its_sockets() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await.with_max_idle(Duration::ZERO);

    for i in 0..5 {
        let conn = db.conn().await.expect("checkout");
        // As above: let a discarded connection's background `close` land first.
        tokio::time::sleep(Duration::from_millis(50)).await;
        conn.query(&format!("SELECT {i}"), ()).await.expect("query");
        drop(conn);
        // And the read's unread rest (see `a_read_dropped_early_gives_its_socket_back`).
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let work = double.work();
    assert!(work.iter().all(|s| !s.had_baton), "every checkout was a new connection: {work:?}");
    assert_eq!(double.connections(), 1, "five connections, one socket");
}

/// #2179: a read gives its socket back even when its caller stops reading
/// early. A `/v3/cursor` answer is streamed, and hyper pools a socket again
/// only once the answer on it was read to the end; before, every read that
/// took one row and dropped the rest dialed a new TCP + TLS connection.
#[tokio::test]
async fn a_read_dropped_early_gives_its_socket_back() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    for i in 0..5 {
        let rows = conn.query(&format!("SELECT {i}"), ()).await.expect("query");
        drop(rows);
        // The rest is read off in the background.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(double.connections(), 1, "five reads, one socket");
}

/// One pool per database, never one per process: two databases (main and log
/// in production) never send on each other's sockets.
#[tokio::test]
async fn two_databases_never_share_a_socket() {
    let (double, url) = start_double().await;
    let main = remote_db(&url).await;
    let log = remote_db(&url).await;

    main.conn().await.unwrap().query("SELECT 1", ()).await.unwrap();
    log.conn().await.unwrap().query("SELECT 1", ()).await.unwrap();
    main.conn().await.unwrap().query("SELECT 2", ()).await.unwrap();

    assert_eq!(double.connections(), 2);
}

/// #2179: a socket a 5xx came back on is never used again (a wedged Turso node
/// stays wedged), even when nobody reported the error as a fault.
#[tokio::test]
async fn a_5xx_answer_retires_the_socket_it_came_on() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let conn = db.conn().await.expect("checkout");
    conn.query("SELECT 1", ()).await.expect("query");
    assert_eq!(double.connections(), 1);

    double.fail_next(StatusCode::BAD_GATEWAY);
    conn.query("SELECT 2", ())
        .await
        .expect_err("the 502 reaches the caller");
    let failed_on = double.work().last().expect("the failed request").peer;
    drop(conn);

    double.clear_served();
    let conn = db.conn().await.expect("checkout");
    conn.execute("INSERT INTO t VALUES (1)", ()).await.expect("execute");
    assert_ne!(
        double.work()[0].peer, failed_on,
        "the request after a 5xx must open a new socket, not reuse the one it came on"
    );
}

/// The pool lets an idle socket go before Turso's edge closes it (30.0 s,
/// measured), so a request never races the server's close.
#[tokio::test]
async fn a_socket_idle_past_the_pool_timeout_is_not_reused() {
    assert!(
        libsql::REMOTE_POOL_IDLE_TIMEOUT < Duration::from_secs(30),
        "must stay under Turso's measured 30 s idle close"
    );
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    db.conn().await.unwrap().execute("INSERT INTO t VALUES (1)", ()).await.unwrap();
    tokio::time::sleep(libsql::REMOTE_POOL_IDLE_TIMEOUT + Duration::from_secs(1)).await;
    db.conn().await.unwrap().execute("INSERT INTO t VALUES (2)", ()).await.unwrap();

    assert_eq!(double.connections(), 2, "an expired idle socket is dropped, not reused");
}

/// Concurrent transactions over pooled connections never interleave BEGIN/COMMIT.
#[tokio::test]
async fn concurrent_transactions_get_their_own_streams() {
    let (double, url) = start_double().await;
    let db = Arc::new(remote_db(&url).await);

    let writers = 6;
    let barrier = Arc::new(tokio::sync::Barrier::new(writers));
    let mut tasks = Vec::new();
    for i in 0..writers {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            let conn = db.conn().await.expect("checkout");
            let tx = pollis_delivery::db::write_tx(&conn).await.expect("begin");
            barrier.wait().await;
            tx.execute(&format!("INSERT INTO tx VALUES ({i})"), ())
                .await
                .expect("execute");
            tx.commit().await.expect("commit");
        }));
    }
    for task in tasks {
        task.await.expect("task");
    }

    let mut streams = HashSet::new();
    for i in 0..writers {
        let found = double.streams_containing(&format!("INSERT INTO tx VALUES ({i})"));
        assert_eq!(found.len(), 1, "writer {i} used exactly one stream");
        let (id, sqls) = &found[0];
        streams.insert(*id);
        assert_eq!(
            sqls.len(),
            3,
            "writer {i} stream should be BEGIN/INSERT/COMMIT only, got {sqls:?}"
        );
        assert!(sqls[0].to_uppercase().starts_with("BEGIN"), "{sqls:?}");
        assert_eq!(
            sqls[1].trim_end_matches(';'),
            format!("INSERT INTO tx VALUES ({i})"),
            "{sqls:?}"
        );
        assert!(sqls[2].to_uppercase().starts_with("COMMIT"), "{sqls:?}");
    }
    assert_eq!(
        streams.len(),
        writers,
        "each concurrent transaction needs its own hrana stream"
    );
}

async fn pragma_i64(conn: &libsql::Connection, pragma: &str) -> i64 {
    let mut rows = conn.query(pragma, ()).await.expect("pragma query");
    rows.next()
        .await
        .expect("pragma row")
        .expect("pragma returns a value")
        .get::<i64>(0)
        .expect("integer value")
}

async fn local_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("delivery.db");
    let db = Db::connect_local(path.to_str().expect("utf-8 path"))
        .await
        .expect("local db");
    (dir, db)
}

/// Holding the first guard forces the nested checkout to build a new connection.
#[tokio::test]
async fn a_nested_checkout_gets_the_same_pragmas_as_the_first() {
    let (_dir, db) = local_db().await;

    let a = db.conn().await.expect("checkout a");
    let b = db.conn().await.expect("checkout b");

    for (name, conn) in [("a", &a), ("b", &b)] {
        assert_eq!(
            pragma_i64(conn, "PRAGMA foreign_keys").await,
            0,
            "conn {name}: foreign_keys must be OFF on EVERY connection of a local Db, \
             not just the one connect_local happened to prime"
        );
        assert_eq!(
            pragma_i64(conn, "PRAGMA busy_timeout").await,
            5000,
            "conn {name}: busy_timeout must be set on EVERY connection of a local Db"
        );
    }
}

/// `foreign_keys=OFF` is production parity; a connection that missed it rejects rows Turso accepts.
#[tokio::test]
async fn a_nested_checkout_enforces_the_same_constraints_as_the_first() {
    let (_dir, db) = local_db().await;
    pollis_schema::apply::single_db(&db.conn().await.expect("checkout"))
        .await
        .expect("schema");

    let a = db.conn().await.expect("checkout a");
    let b = db.conn().await.expect("checkout b");

    // The referenced user does not exist; Turso accepts this row anyway.
    for (name, conn) in [("a", &a), ("b", &b)] {
        conn.execute(
            "INSERT INTO device_enrollment_request \
                 (id, user_id, new_device_id, new_device_ephemeral_pub, verification_code, \
                  status, expires_at) \
             VALUES (?1, 'nobody', 'dev', x'00', '000000', 'pending', '2099-01-01T00:00:00+00:00')",
            libsql::params![format!("req-{name}")],
        )
        .await
        .unwrap_or_else(|e| {
            panic!(
                "conn {name}: an out-of-order fixture insert must be accepted on every \
                 connection of a local Db (foreign_keys=OFF); got {e}"
            )
        });
    }
}

/// The remote `Db` sets no PRAGMAs: each would be a round trip per new connection.
#[tokio::test]
async fn a_remote_connection_is_not_stamped_with_pragmas() {
    let (double, url) = start_double().await;
    let db = remote_db(&url).await;

    let a = db.conn().await.expect("checkout a");
    let b = db.conn().await.expect("checkout b");
    a.execute("INSERT INTO t VALUES (1)", ()).await.expect("execute a");
    b.execute("INSERT INTO t VALUES (2)", ()).await.expect("execute b");

    let pragmas: Vec<String> = double
        .streams
        .lock()
        .unwrap()
        .values()
        .flatten()
        .filter(|sql| sql.to_uppercase().contains("PRAGMA"))
        .cloned()
        .collect();
    assert!(
        pragmas.is_empty(),
        "a remote Db must not issue PRAGMAs on checkout, got {pragmas:?}"
    );
}
