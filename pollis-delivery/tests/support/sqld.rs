//! A real `sqld` (the libSQL server Turso runs) for the tests that must cover
//! the remote hrana path the DS takes in production, not local libsql.
//!
//! `Builder::new_local` never reaches `patches/libsql`'s hrana code, so a test
//! that only ever opens a local file says nothing about what Turso sees. These
//! tests are `#[ignore]` because CI has no `sqld`; run them with
//!
//! ```text
//! POLLIS_SQLD=~/.turso/sqld cargo test -p pollis-delivery --test remote_sqld -- --ignored
//! ```
//!
//! (`POLLIS_SQLD` defaults to `~/.turso/sqld`). Between the DS and the server
//! sits [`Proxy`]: it counts every HTTP request (one request is one Turso round
//! trip) and can cut a connection after a request reached the server but before
//! its answer came back.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A running `sqld` on a fresh data directory; killed on drop.
pub struct Sqld {
    child: std::process::Child,
    _dir: tempfile::TempDir,
    pub port: u16,
}

impl Drop for Sqld {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn sqld_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("POLLIS_SQLD") {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME").expect("HOME or POLLIS_SQLD");
    PathBuf::from(home).join(".turso/sqld")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

impl Sqld {
    pub async fn spawn() -> Sqld {
        let bin = sqld_binary();
        assert!(
            bin.exists(),
            "no sqld at {} — set POLLIS_SQLD to a sqld 0.24 binary",
            bin.display()
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let port = free_port();
        let grpc = free_port();
        let child = std::process::Command::new(&bin)
            .arg("-d")
            .arg(dir.path().join("data.sqld"))
            .arg("--http-listen-addr")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--grpc-listen-addr")
            .arg(format!("127.0.0.1:{grpc}"))
            .arg("--no-welcome")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sqld");
        let me = Sqld {
            child,
            _dir: dir,
            port,
        };
        let health = format!("http://127.0.0.1:{port}/health");
        for _ in 0..400 {
            if let Ok(resp) = reqwest::get(&health).await {
                if resp.status().is_success() {
                    return me;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("sqld did not come up on {port}");
    }
}

/// One HTTP request the proxy forwarded.
#[derive(Clone, Debug)]
pub struct Forwarded {
    /// `/v3/pipeline`, `/v3/cursor`, …
    pub path: String,
    /// Whether the body carried a hrana `describe` request.
    pub describe: bool,
    /// The SQL in the body, whitespace-collapsed.
    pub sql: Vec<String>,
}

/// A TCP proxy in front of a [`Sqld`] that logs every request.
#[derive(Clone)]
pub struct Proxy {
    pub url: String,
    log: Arc<Mutex<Vec<Forwarded>>>,
    /// Cut the next connection whose request contains this text, after the
    /// server has answered it and before the answer reaches the client.
    cut_after: Arc<Mutex<Option<String>>>,
    cut: Arc<AtomicBool>,
}

impl Proxy {
    pub async fn start(upstream_port: u16) -> Proxy {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind proxy");
        let port = listener.local_addr().expect("addr").port();
        let me = Proxy {
            url: format!("http://127.0.0.1:{port}"),
            log: Arc::default(),
            cut_after: Arc::default(),
            cut: Arc::default(),
        };
        let proxy = me.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let proxy = proxy.clone();
                tokio::spawn(async move { proxy.relay(client, upstream_port).await });
            }
        });
        me
    }

    async fn relay(&self, client: tokio::net::TcpStream, upstream_port: u16) {
        let Ok(upstream) = tokio::net::TcpStream::connect(("127.0.0.1", upstream_port)).await
        else {
            return;
        };
        let (mut cr, mut cw) = client.into_split();
        let (mut ur, mut uw) = upstream.into_split();
        let mut cbuf = vec![0u8; 256 * 1024];
        let mut ubuf = vec![0u8; 256 * 1024];
        loop {
            tokio::select! {
                n = cr.read(&mut cbuf) => {
                    let Ok(n) = n else { return };
                    if n == 0 {
                        return;
                    }
                    let chunk = &cbuf[..n];
                    let cut = self.note(chunk);
                    if uw.write_all(chunk).await.is_err() {
                        return;
                    }
                    if cut {
                        // Let the server apply it and answer, then drop the
                        // answer and both sockets.
                        let _ = tokio::time::timeout(
                            Duration::from_secs(5),
                            ur.read(&mut ubuf),
                        )
                        .await;
                        self.cut.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                n = ur.read(&mut ubuf) => {
                    let Ok(n) = n else { return };
                    if n == 0 {
                        return;
                    }
                    if cw.write_all(&ubuf[..n]).await.is_err() {
                        return;
                    }
                }
            }
        }
    }

    /// Log `chunk` if it starts a request; whether to cut after it.
    fn note(&self, chunk: &[u8]) -> bool {
        let text = String::from_utf8_lossy(chunk);
        if text.starts_with("POST ") || text.starts_with("GET ") {
            let path = text
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .split('?')
                .next()
                .unwrap_or("")
                .to_string();
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
            let mut sql = Vec::new();
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
                collect_sql(&json, &mut sql);
            }
            self.log.lock().unwrap().push(Forwarded {
                path,
                describe: body.contains(r#""type":"describe""#),
                sql,
            });
        }
        let mut cut_after = self.cut_after.lock().unwrap();
        if let Some(needle) = cut_after.as_deref() {
            if text.contains(needle) {
                *cut_after = None;
                return true;
            }
        }
        false
    }

    /// Every request forwarded since the last [`Self::reset`].
    pub fn log(&self) -> Vec<Forwarded> {
        self.log.lock().unwrap().clone()
    }

    /// Requests forwarded since the last reset, excluding the `close` a
    /// dropped connection sends for its stream in the background.
    pub fn round_trips(&self) -> usize {
        self.log()
            .iter()
            .filter(|f| !(f.path == "/v3/pipeline" && f.sql.is_empty()))
            .count()
    }

    pub fn reset(&self) {
        self.log.lock().unwrap().clear();
    }

    /// Cut the connection carrying the next request that contains `needle`
    /// after the server answered it, so the client never sees the answer.
    pub fn cut_after_request_containing(&self, needle: &str) {
        *self.cut_after.lock().unwrap() = Some(needle.to_string());
        self.cut.store(false, Ordering::SeqCst);
    }

    /// Whether the armed cut has happened.
    pub fn did_cut(&self) -> bool {
        self.cut.load(Ordering::SeqCst)
    }
}

fn collect_sql(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            if let Some(serde_json::Value::String(s)) = m.get("sql") {
                out.push(s.split_whitespace().collect::<Vec<_>>().join(" "));
            }
            for (_, x) in m {
                collect_sql(x, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_sql(x, out)),
        _ => {}
    }
}

/// A sqld behind a proxy, connected to as the DS does in production.
pub struct RemoteDb {
    pub db: Arc<pollis_delivery::db::Db>,
    pub proxy: Proxy,
    _sqld: Sqld,
}

impl RemoteDb {
    /// A fresh server, with `schema` applied through a direct connection.
    pub async fn with_schema(
        schema: impl for<'a> FnOnce(
            &'a libsql::Connection,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), libsql::Error>> + 'a>,
        >,
    ) -> RemoteDb {
        let sqld = Sqld::spawn().await;
        let direct = pollis_delivery::db::Db::connect_remote(
            &format!("http://127.0.0.1:{}", sqld.port),
            "test",
        )
        .await
        .expect("direct db");
        schema(&direct.conn().await.expect("conn"))
            .await
            .expect("schema");
        let proxy = Proxy::start(sqld.port).await;
        let db = pollis_delivery::db::Db::connect_remote(&proxy.url, "test")
            .await
            .expect("remote db");
        RemoteDb {
            db: Arc::new(db),
            proxy,
            _sqld: sqld,
        }
    }

    /// The single-DB deploy shape: main and log schema on one server.
    pub async fn single_db() -> RemoteDb {
        Self::with_schema(|c| Box::pin(pollis_schema::apply::single_db(c))).await
    }

    pub async fn main_db() -> RemoteDb {
        Self::with_schema(|c| Box::pin(pollis_schema::apply::main_db(c))).await
    }

    pub async fn log_db() -> RemoteDb {
        Self::with_schema(|c| Box::pin(pollis_schema::apply::log_db(c))).await
    }
}
