//! A hrana-over-HTTP double for the conn-pool tests: `/v3/pipeline` and
//! `/v3/cursor`, with a log of every request and the client socket it came on.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use libsql_hrana::proto::{
    BatchResult, BatchStreamResp, CloseStreamResp, CursorReqBody, DescribeResult,
    DescribeStreamResp, ExecuteStreamResp, GetAutocommitStreamResp, PipelineReqBody,
    PipelineRespBody, StmtResult, StreamRequest, StreamResponse, StreamResult,
};
use pollis_delivery::db::Db;

/// One HTTP request the double served.
#[derive(Clone, Debug)]
pub struct Served {
    /// `/v3/pipeline` or `/v3/cursor`.
    pub path: &'static str,
    /// The client socket it arrived on.
    pub peer: SocketAddr,
    /// Whether it continued an existing stream (carried a baton).
    pub had_baton: bool,
    /// The hrana request kinds in it (`execute`, `describe`, `close`, `cursor`…).
    pub kinds: Vec<&'static str>,
}

#[derive(Default)]
pub struct Double {
    /// Distinct client sockets (TCP connections).
    peers: Mutex<HashSet<SocketAddr>>,
    pub streams: Mutex<HashMap<u64, Vec<String>>>,
    autocommit: Mutex<HashMap<u64, bool>>,
    /// Live baton -> stream id. A stream keeps its id across baton rotations.
    batons: Mutex<HashMap<String, u64>>,
    next_stream: AtomicU64,
    next_baton: AtomicU64,
    served: Mutex<Vec<Served>>,
    /// Answer the next request with this status instead of serving it.
    fail_next: Mutex<Option<StatusCode>>,
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

    /// Distinct client sockets seen so far: TCP connections, not libsql `Connection`s.
    pub fn connections(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    /// Every request served so far, in arrival order.
    pub fn served(&self) -> Vec<Served> {
        self.served.lock().unwrap().clone()
    }

    /// [`Self::served`] without the `close` libsql sends, in the background,
    /// for a dropped connection's stream.
    pub fn work(&self) -> Vec<Served> {
        self.served()
            .into_iter()
            .filter(|s| s.kinds != ["close"])
            .collect()
    }

    /// Forget the request log (not the sockets or streams).
    pub fn clear_served(&self) {
        self.served.lock().unwrap().clear();
    }

    /// Answer the next request with `status`, as a wedged Turso edge does.
    pub fn fail_next(&self, status: StatusCode) {
        *self.fail_next.lock().unwrap() = Some(status);
    }

    /// Expire every open stream, as Turso does to an idle one.
    pub fn expire_streams(&self) {
        self.batons.lock().unwrap().clear();
    }

    pub fn streams_containing(&self, needle: &str) -> Vec<(u64, Vec<String>)> {
        self.streams
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, sqls)| sqls.iter().any(|s| s.contains(needle)))
            .map(|(id, sqls)| (*id, sqls.clone()))
            .collect()
    }

    /// The request's stream: the baton's, or a new one. `Err` is the 404 a
    /// server answers for an expired baton.
    fn stream_for(&self, baton: Option<&str>) -> Result<u64, Box<Response>> {
        match baton {
            Some(baton) => self.batons.lock().unwrap().get(baton).copied().ok_or_else(|| {
                Box::new(
                    (
                        StatusCode::NOT_FOUND,
                        format!(r#"{{"error":"stream not found: {baton}"}}"#),
                    )
                        .into_response(),
                )
            }),
            None => Ok(self.next_stream.fetch_add(1, Ordering::SeqCst)),
        }
    }

    /// Log the request and take any injected failure.
    fn arrive(&self, served: Served) -> Option<Response> {
        self.peers.lock().unwrap().insert(served.peer);
        self.served.lock().unwrap().push(served);
        self.fail_next
            .lock()
            .unwrap()
            .take()
            .map(|status| (status, r#"{"error":"injected"}"#).into_response())
    }
}

fn ok(response: StreamResponse) -> StreamResult {
    StreamResult::Ok { response }
}

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

fn kind_of(request: &StreamRequest) -> &'static str {
    match request {
        StreamRequest::Execute(_) => "execute",
        StreamRequest::Batch(_) => "batch",
        StreamRequest::GetAutocommit(_) => "get_autocommit",
        StreamRequest::Describe(_) => "describe",
        StreamRequest::Close(_) => "close",
        _ => "other",
    }
}

async fn pipeline(
    State(state): State<Arc<Double>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    // Raw bytes: libsql sends no `Content-Type`, which axum's JSON extractor rejects.
    body: axum::body::Bytes,
) -> Response {
    let req: PipelineReqBody = serde_json::from_slice(&body).expect("pipeline request");
    let served = Served {
        path: "/v3/pipeline",
        peer,
        had_baton: req.baton.is_some(),
        kinds: req.requests.iter().map(kind_of).collect(),
    };
    if let Some(failed) = state.arrive(served) {
        return failed;
    }
    let stream = match state.stream_for(req.baton.as_deref()) {
        Ok(stream) => stream,
        Err(not_found) => return *not_found,
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
    .into_response()
}

/// `/v3/cursor`: every statement succeeds and returns no rows.
async fn cursor(
    State(state): State<Arc<Double>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: axum::body::Bytes,
) -> Response {
    let req: CursorReqBody = serde_json::from_slice(&body).expect("cursor request");
    let served = Served {
        path: "/v3/cursor",
        peer,
        had_baton: req.baton.is_some(),
        kinds: vec!["cursor"],
    };
    if let Some(failed) = state.arrive(served) {
        return failed;
    }
    let stream = match state.stream_for(req.baton.as_deref()) {
        Ok(stream) => stream,
        Err(not_found) => return *not_found,
    };
    // A cursor never closes its stream: the client closes it on a later request.
    let mut lines = vec![serde_json::json!({ "baton": state.mint_baton(stream), "base_url": null })];
    for (step, s) in req.batch.steps.iter().enumerate() {
        state.record(stream, s.stmt.sql.as_deref().unwrap_or(""));
        lines.push(serde_json::json!({ "type": "step_begin", "step": step, "cols": [] }));
        lines.push(serde_json::json!({
            "type": "step_end",
            "affected_row_count": 0,
            "last_inserted_rowid": null,
        }));
    }
    // Streamed line by line (chunked), as sqld and Turso send a cursor.
    let chunks: Vec<Result<String, std::convert::Infallible>> =
        lines.into_iter().map(|line| Ok(format!("{line}\n"))).collect();
    axum::body::Body::from_stream(futures::stream::iter(chunks)).into_response()
}

pub async fn start_double() -> (Arc<Double>, String) {
    let state = Arc::new(Double::default());
    let app = Router::new()
        .route("/v3/pipeline", post(pipeline))
        .route("/v3/cursor", post(cursor))
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

pub async fn remote_db(url: &str) -> Db {
    Db::connect_remote(url, "test-token")
        .await
        .expect("remote db")
}
