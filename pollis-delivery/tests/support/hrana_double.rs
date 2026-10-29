//! A hrana-over-HTTP double for the conn-pool tests; one client socket per libsql `Connection`.

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
    BatchResult, BatchStreamResp, CloseStreamResp, DescribeResult, DescribeStreamResp,
    ExecuteStreamResp, GetAutocommitStreamResp, PipelineReqBody, PipelineRespBody, StmtResult,
    StreamRequest, StreamResponse, StreamResult,
};
use pollis_delivery::db::Db;

#[derive(Default)]
pub struct Double {
    /// Distinct client sockets == distinct libsql connections.
    peers: Mutex<HashSet<SocketAddr>>,
    pub streams: Mutex<HashMap<u64, Vec<String>>>,
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

    pub fn connections(&self) -> usize {
        self.peers.lock().unwrap().len()
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

async fn pipeline(
    State(state): State<Arc<Double>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    // Raw bytes: libsql sends no `Content-Type`, which axum's JSON extractor rejects.
    body: axum::body::Bytes,
) -> Response {
    let req: PipelineReqBody = serde_json::from_slice(&body).expect("pipeline request");
    state.peers.lock().unwrap().insert(peer);

    let stream = match req.baton.as_deref() {
        Some(baton) => {
            let Some(stream) = state.batons.lock().unwrap().get(baton).copied() else {
                return (
                    StatusCode::NOT_FOUND,
                    format!(r#"{{"error":"stream not found: {baton}"}}"#),
                )
                    .into_response();
            };
            stream
        }
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
    .into_response()
}

pub async fn start_double() -> (Arc<Double>, String) {
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

pub async fn remote_db(url: &str) -> Db {
    Db::connect_remote(url, "test-token")
        .await
        .expect("remote db")
}

