//! #777 — the DS connection pool, exercised over the **remote** (hrana/HTTP)
//! path, which is the only path where the per-request `hyper::Client` bug
//! existed and the only one where reuse could break something.
//!
//! Every other `pollis-delivery` test builds the DB with `connect_local`, so
//! none of them touch hrana at all. This file stands up a hrana-speaking test
//! double, points `Db::connect_remote` at it, and asserts the three properties
//! the pool has to hold:
//!
//! 1. a warm connection is reused, so N requests cost ONE TCP connection (in
//!    production, one TLS handshake) instead of N;
//! 2. a checked-out connection is never handed to a second caller — concurrent
//!    holders always get distinct connections, hence distinct hrana streams;
//! 3. a parked connection that has gone stale is dropped, never reused, so a
//!    server-expired hrana stream can't surface as a failed request later.
//!
//! Plus the transaction-isolation guarantee the pool must not disturb:
//! concurrent transactions each run on their own hrana stream, so their
//! BEGIN/…/COMMIT sequences never interleave.
//!
//! The double counts distinct *client* socket addresses. Each `libsql`
//! `Connection` for a remote DB owns its own `hyper::Client` (and therefore its
//! own TCP pool), so "distinct peer addresses" is exactly "how many connections
//! were built" — the number the defect made equal to the request count.
//!
//! The final section covers the pool's OTHER job (#919): every connection it
//! builds carries the same per-connection PRAGMAs. That one runs over the local
//! path, because per-connection PRAGMAs are exactly what the local test DBs
//! depend on.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{ConnectInfo, State};
use axum::routing::post;
use axum::{Json, Router};
use libsql_hrana::proto::{
    BatchResult, BatchStreamResp, CloseStreamResp, DescribeResult, DescribeStreamResp,
    ExecuteStreamResp, GetAutocommitStreamResp, PipelineReqBody, PipelineRespBody, StmtResult,
    StreamRequest, StreamResponse, StreamResult,
};
use pollis_delivery::db::Db;

/// What the double saw, keyed the way the assertions need it.
#[derive(Default)]
struct Double {
    /// Distinct client socket addresses == distinct TCP connections == distinct
    /// `hyper::Client`s built by libsql.
    peers: Mutex<HashSet<SocketAddr>>,
    /// SQL executed on each hrana stream, in order.
    streams: Mutex<HashMap<u64, Vec<String>>>,
    /// Whether each stream is in autocommit (i.e. NOT inside a transaction).
    autocommit: Mutex<HashMap<u64, bool>>,
    /// Live baton -> stream id. A stream keeps its id across baton rotations.
    batons: Mutex<HashMap<String, u64>>,
    next_stream: AtomicU64,
    next_baton: AtomicU64,
}

impl Double {
    fn record(&self, stream: u64, sql: &str) {
        self.streams
            .lock()
            .unwrap()
            .entry(stream)
            .or_default()
            .push(sql.to_string());
        let upper = sql.trim().to_uppercase();
        if upper.starts_with("BEGIN") {
            self.autocommit.lock().unwrap().insert(stream, false);
        }
        if upper.starts_with("COMMIT") || upper.starts_with("ROLLBACK") {
            self.autocommit.lock().unwrap().insert(stream, true);
        }
    }

    fn is_autocommit(&self, stream: u64) -> bool {
        *self
            .autocommit
            .lock()
            .unwrap()
            .get(&stream)
            .unwrap_or(&true)
    }

    fn mint_baton(&self, stream: u64) -> String {
        let baton = format!("b{}", self.next_baton.fetch_add(1, Ordering::SeqCst));
        self.batons.lock().unwrap().insert(baton.clone(), stream);
        baton
    }

    fn connections(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    /// Streams whose SQL log contains `needle`, as (stream id, sql log).
    fn streams_containing(&self, needle: &str) -> Vec<(u64, Vec<String>)> {
        self.streams
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, sqls)| sqls.iter().any(|s| s.contains(needle)))
            .map(|(id, sqls)| (*id, sqls.clone()))
            .collect()
    }
}

fn ok(response: StreamResponse) -> StreamResult {
    StreamResult::Ok { response }
}

/// A result shaped like a one-row write. The tests assert on protocol traffic,
/// not on data, so nothing here needs a real SQLite behind it.
fn stmt_result() -> StmtResult {
    StmtResult {
        cols: vec![],
        rows: vec![],
        affected_row_count: 1,
        last_insert_rowid: Some(1),
        replication_index: None,
        rows_read: 0,
        rows_written: 1,
        query_duration_ms: 0.0,
    }
}

async fn pipeline(
    State(state): State<Arc<Double>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    // Raw bytes, not `Json<_>`: libsql posts the pipeline without a
    // `Content-Type` header, which axum's JSON extractor rejects.
    body: axum::body::Bytes,
) -> Json<PipelineRespBody> {
    let req: PipelineReqBody = serde_json::from_slice(&body).expect("pipeline request");
    state.peers.lock().unwrap().insert(peer);

    // A request carrying a baton continues an existing stream; one without
    // opens a new one — the hrana rule the pool's staleness bound protects.
    let stream = match req.baton.as_deref() {
        Some(baton) => *state
            .batons
            .lock()
            .unwrap()
            .get(baton)
            .expect("baton was issued by this double"),
        None => state.next_stream.fetch_add(1, Ordering::SeqCst),
    };

    let mut results = Vec::new();
    let mut closed = false;
    for request in req.requests {
        match request {
            StreamRequest::Execute(exec) => {
                state.record(stream, exec.stmt.sql.as_deref().unwrap_or(""));
                results.push(ok(StreamResponse::Execute(ExecuteStreamResp {
                    result: stmt_result(),
                })));
            }
            StreamRequest::Batch(batch) => {
                let steps = batch.batch.steps.len();
                for step in &batch.batch.steps {
                    state.record(stream, step.stmt.sql.as_deref().unwrap_or(""));
                }
                results.push(ok(StreamResponse::Batch(BatchStreamResp {
                    result: BatchResult {
                        step_results: (0..steps).map(|_| Some(stmt_result())).collect(),
                        step_errors: (0..steps).map(|_| None).collect(),
                        replication_index: None,
                    },
                })));
            }
            StreamRequest::GetAutocommit(_) => {
                results.push(ok(StreamResponse::GetAutocommit(GetAutocommitStreamResp {
                    is_autocommit: state.is_autocommit(stream),
                })));
            }
            StreamRequest::Describe(_) => {
                results.push(ok(StreamResponse::Describe(DescribeStreamResp {
                    result: DescribeResult {
                        params: vec![],
                        cols: vec![],
                        is_explain: false,
                        is_readonly: false,
                    },
                })));
            }
            StreamRequest::Close(_) => {
                closed = true;
                results.push(ok(StreamResponse::Close(CloseStreamResp {})));
            }
            other => panic!("test double got an unhandled hrana request: {other:?}"),
        }
    }

    // Dropping the baton is how a server tells the client the stream is gone.
    let baton = if closed {
        None
    } else {
        Some(state.mint_baton(stream))
    };
    Json(PipelineRespBody {
        baton,
        base_url: None,
        results,
    })
}

/// Start the double and return it with the `http://` URL to point `Db` at.
async fn start_double() -> (Arc<Double>, String) {
    let state = Arc::new(Double::default());
    let app = Router::new()
        .route("/v3/pipeline", post(pipeline))
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind double");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve double");
    });
    (state, format!("http://{addr}"))
}

async fn remote_db(url: &str) -> Db {
    Db::connect_remote(url, "test-token")
        .await
        .expect("remote db")
}

/// The defect itself: before pooling, twenty sequential requests built twenty
/// `hyper::Client`s and therefore paid twenty TCP + TLS handshakes. Reuse makes
/// it one.
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

/// Exclusivity: a checked-out connection is *removed* from the pool, so a
/// second caller can never be handed the same one. Observable at the protocol
/// layer as distinct hrana streams (and distinct TCP peers).
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
            // Hold every guard at once, so the pool cannot satisfy any of them
            // from a parked connection.
            barrier.wait().await;
            conn.execute(&format!("INSERT INTO holders VALUES ({i})"), ())
                .await
                .expect("execute");
        }));
    }
    for task in tasks {
        task.await.expect("task");
    }

    assert_eq!(
        double.connections(),
        holders,
        "each concurrent holder needs its own connection"
    );
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

    // Once released they are reusable: a follow-up request opens nothing new.
    let conn = db.conn().await.expect("checkout");
    conn.execute("INSERT INTO after VALUES (1)", ())
        .await
        .expect("execute");
    assert_eq!(
        double.connections(),
        holders,
        "a released connection is reused, not rebuilt"
    );
}

/// Staleness bound: a hrana stream left open by an earlier statement expires
/// server-side, so a connection parked for too long must be thrown away rather
/// than reused. Proven by the pool building a *second* connection after the
/// idle window elapses, and reusing the first when it has not.
#[tokio::test]
async fn a_stale_parked_connection_is_never_reused() {
    let (double, url) = start_double().await;
    let db = remote_db(&url)
        .await
        .with_max_idle(Duration::from_millis(50));

    let conn = db.conn().await.expect("checkout");
    conn.execute("INSERT INTO t VALUES (1)", ())
        .await
        .expect("execute");
    drop(conn);

    // Well inside the window: the parked connection is still good.
    let conn = db.conn().await.expect("checkout");
    conn.execute("INSERT INTO t VALUES (2)", ())
        .await
        .expect("execute");
    drop(conn);
    assert_eq!(
        double.connections(),
        1,
        "a fresh parked connection is reused"
    );

    tokio::time::sleep(Duration::from_millis(120)).await;

    let conn = db.conn().await.expect("checkout");
    conn.execute("INSERT INTO t VALUES (3)", ())
        .await
        .expect("execute");
    assert_eq!(
        double.connections(),
        2,
        "a connection parked past the idle window must be discarded, not reused"
    );
}

/// Transaction isolation, which reuse must not disturb: hrana binds each
/// `transaction()` to its own stream, so concurrent transactions run over
/// pooled connections still never interleave BEGIN/COMMIT with each other.
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
            let tx = conn.transaction().await.expect("begin");
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
        // The whole point: this writer's stream carries its transaction and
        // nothing else — no other writer's statement landed in the middle of
        // it.
        assert_eq!(
            sqls.len(),
            3,
            "writer {i} stream should be BEGIN/INSERT/COMMIT only, got {sqls:?}"
        );
        assert!(sqls[0].to_uppercase().starts_with("BEGIN"), "{sqls:?}");
        // libsql's parser re-emits each statement terminated.
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

// ── #919: every checkout carries the same per-connection PRAGMAs ─────────────
//
// `Db::connect_local` used to prime the ONE connection it built and park it. The
// pool is deliberately not a capacity limit — an empty pool builds a fresh
// connection rather than blocking, because handlers nest checkouts (`lib.rs`
// holds a `log_db` connection while taking a `db` one) — so any *nested* checkout
// got a connection with libsql's defaults instead. Probed at the time:
// `conn a: foreign_keys=0`, `conn b: foreign_keys=1`, on one `Db`.
//
// That is test-only (production is remote Turso, and its `Db` sets no PRAGMAs at
// all), but it made a fixture's success depend on how deeply the code under test
// happened to nest its checkouts, which is not a property any test means to
// assert.

/// Read a PRAGMA's integer value off a specific connection.
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

/// The defect, directly. Holding the first guard is what forces the second
/// checkout to build a connection instead of reusing the parked one — i.e. the
/// nesting every handler does.
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

/// …and it is not cosmetic. `foreign_keys=OFF` is **production parity** — remote
/// Turso does not enforce foreign keys, so the schema's `REFERENCES` clauses are
/// inert on every deployment — and libsql's local backend turns them ON. With the
/// pragma reaching only the primed connection, the same insert that production
/// accepts succeeded on connection A and was rejected on connection B of one
/// `Db`: a test could pass or fail on a constraint no deploy has, depending only
/// on checkout nesting.
#[tokio::test]
async fn a_nested_checkout_enforces_the_same_constraints_as_the_first() {
    let (_dir, db) = local_db().await;
    pollis_schema::apply::single_db(&db.conn().await.expect("checkout"))
        .await
        .expect("schema");

    let a = db.conn().await.expect("checkout a");
    let b = db.conn().await.expect("checkout b");

    // `device_enrollment_request.user_id` REFERENCES users(id), and no such user
    // exists. Production accepts this row (Turso does not enforce the reference);
    // a connection that missed the pragma does not.
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

/// The remote `Db` gets NO per-connection PRAGMAs, and must not acquire any by
/// accident: each one would be a network round trip on every new connection,
/// against a server that owns those settings anyway. Guard.
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
