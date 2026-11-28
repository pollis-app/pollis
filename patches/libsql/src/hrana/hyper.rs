use crate::connection::{BatchRows, Conn};
use crate::hrana::connection::HttpConnection;
use crate::hrana::proto::{Batch, Stmt};
use crate::hrana::stream::HranaStream;
use crate::hrana::transaction::{HttpTransaction, TxScopeCounter};
use crate::hrana::{bind_params, unwrap_err, HranaError, HttpSend, Result};
use crate::params::Params;
use crate::transaction::Tx;
use crate::util::ConnectorService;
use crate::{Error, Rows, Statement};
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{Stream, TryStreamExt};
use http::header::AUTHORIZATION;
use http::{HeaderValue, StatusCode};
use hyper::body::HttpBody;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;

use super::StmtResultRows;

pub type ByteStream = Box<dyn Stream<Item = std::io::Result<Bytes>> + Send + Sync + Unpin>;

/// POLLIS (#2179): how long an idle keep-alive socket to Turso stays pooled.
///
/// Turso's edge closes an idle HTTP/1.1 connection at exactly 30.0 s (measured,
/// 3 of 3 connections; it sends no `Keep-Alive` header and offers no h2), so
/// the pool lets go well before that, or a request could race the server's
/// close. hyper retries a request that never reached the wire on a socket the
/// server had already closed (`retry_canceled_requests`, left at its default).
pub const REMOTE_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(20);

type HyperClient = hyper::Client<ConnectorService, hyper::Body>;

/// POLLIS (#2179): ONE hyper client, and so one keep-alive socket pool, per
/// remote `Database`.
///
/// Upstream builds a client per `HttpSender`, i.e. per `Connection`, so a
/// socket lived exactly as long as its connection, and a caller that drops
/// connections whose hrana stream may have expired (the DS pool does after 3 s)
/// also threw away a warm TCP + TLS socket. Shared, the socket outlives the
/// stream; stream (baton) lifetime is untouched.
///
/// A socket that carried a transport failure or a 5xx answer must not carry
/// another request: a wedged Turso node stays wedged for everything sent to it.
/// So a failure swaps in a fresh client with an empty pool ([`Self::reset`]),
/// and the next request dials a new connection, which Turso's load balancer can
/// route to a healthy node. `HttpSender::send` does that itself on every such
/// failure; `Database::reset_remote_http_pool` does it for anything else a
/// caller counts as a fault.
#[derive(Clone)]
pub struct SharedHttpClient {
    current: Arc<std::sync::RwLock<(u64, HyperClient)>>,
    connector: ConnectorService,
}

impl std::fmt::Debug for SharedHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedHttpClient").finish_non_exhaustive()
    }
}

impl SharedHttpClient {
    pub(crate) fn new(connector: ConnectorService) -> Self {
        let client = Self::build(connector.clone());
        Self {
            current: Arc::new(std::sync::RwLock::new((0, client))),
            connector,
        }
    }

    fn build(connector: ConnectorService) -> HyperClient {
        hyper::Client::builder()
            .pool_idle_timeout(REMOTE_POOL_IDLE_TIMEOUT)
            .build(connector)
    }

    /// The client to send on now, and its generation.
    fn current(&self) -> (u64, HyperClient) {
        let current = self.current.read().unwrap_or_else(|e| e.into_inner());
        (current.0, current.1.clone())
    }

    /// Drop every pooled socket: the next request opens a new connection.
    pub fn reset(&self) {
        let mut current = self.current.write().unwrap_or_else(|e| e.into_inner());
        let next = current.0 + 1;
        *current = (next, Self::build(self.connector.clone()));
    }

    /// [`Self::reset`], unless the pool was already replaced since `generation`.
    fn reset_after(&self, generation: u64) {
        let mut current = self.current.write().unwrap_or_else(|e| e.into_inner());
        if current.0 == generation {
            *current = (generation + 1, Self::build(self.connector.clone()));
        }
    }
}

#[derive(Clone, Debug)]
pub struct HttpSender {
    inner: SharedHttpClient,
    version: HeaderValue,
    namespace: Option<HeaderValue>,
    #[cfg(any(feature = "remote", feature = "sync"))]
    remote_encryption: Option<crate::database::EncryptionContext>,
}

impl HttpSender {
    pub fn new(
        connector: ConnectorService,
        version: Option<&str>,
        namespace: Option<&str>,
        #[cfg(any(feature = "remote", feature = "sync"))] remote_encryption: Option<
            crate::database::EncryptionContext,
        >,
    ) -> Self {
        Self::with_client(
            SharedHttpClient::new(connector),
            version,
            namespace,
            #[cfg(any(feature = "remote", feature = "sync"))]
            remote_encryption,
        )
    }

    /// POLLIS (#2179): a sender on a `Database`'s shared client.
    pub(crate) fn with_client(
        inner: SharedHttpClient,
        version: Option<&str>,
        namespace: Option<&str>,
        #[cfg(any(feature = "remote", feature = "sync"))] remote_encryption: Option<
            crate::database::EncryptionContext,
        >,
    ) -> Self {
        let ver = version.unwrap_or(env!("CARGO_PKG_VERSION"));

        let version = HeaderValue::try_from(format!("libsql-remote-{ver}")).unwrap();
        let namespace = namespace.map(|v| HeaderValue::try_from(v).unwrap());

        Self {
            inner,
            version,
            namespace,
            #[cfg(any(feature = "remote", feature = "sync"))]
            remote_encryption,
        }
    }

    async fn send(
        self,
        url: Arc<str>,
        auth: Arc<str>,
        body: String,
    ) -> Result<super::HttpBody<ByteStream>> {
        let mut req_builder = hyper::Request::post(url.as_ref())
            .header(AUTHORIZATION, auth.as_ref())
            .header("x-libsql-client-version", self.version.clone());

        if let Some(namespace) = self.namespace {
            req_builder = req_builder.header("x-namespace", namespace);
        }

        #[cfg(any(feature = "remote", feature = "sync"))]
        if let Some(remote_encryption) = &self.remote_encryption {
            req_builder =
                req_builder.header("x-turso-encryption-key", remote_encryption.key.as_string());
        }

        let req = req_builder
            .body(hyper::Body::from(body))
            .map_err(|err| HranaError::Http(format!("{:?}", err)))?;

        // POLLIS (#2179): a socket that failed, or that a 5xx came back on, is
        // never used again (see `SharedHttpClient`).
        let (generation, client) = self.inner.current();
        let resp = match client.request(req).await {
            Ok(resp) => resp,
            Err(err) => {
                self.inner.reset_after(generation);
                return Err(HranaError::from(err));
            }
        };

        let status = resp.status();
        if status.is_server_error() {
            self.inner.reset_after(generation);
        }
        if status != StatusCode::OK {
            let body = hyper::body::to_bytes(resp.into_body())
                .await
                .map_err(HranaError::from)?;
            let body = String::from_utf8(body.into()).unwrap();
            return Err(HranaError::Api(format!("status={}, body={}", status, body)));
        }

        let body: super::HttpBody<ByteStream> = if resp.is_end_stream() {
            let body = hyper::body::to_bytes(resp.into_body())
                .await
                .map_err(HranaError::from)?;
            super::HttpBody::from(body)
        } else {
            let stream = resp
                .into_body()
                .into_stream()
                .map_err(|e| std::io::Error::new(ErrorKind::Other, e));
            super::HttpBody::Stream(Box::new(DrainOnDrop::new(Box::new(stream))))
        };

        Ok(body)
    }
}

/// POLLIS (#2179): the most a dropped response body is read on to free its socket.
const DRAIN_LIMIT_BYTES: usize = 1 << 20;
/// POLLIS (#2179): the longest a dropped response body is read on to free its socket.
const DRAIN_LIMIT: Duration = Duration::from_secs(5);

/// POLLIS (#2179): a streamed response body (a `/v3/cursor`) that is read to
/// its end even when its reader stops early.
///
/// hyper only puts a socket back in the keep-alive pool once the response on
/// it has been read to the end. A cursor's reader stops at its step's end, or
/// earlier (`rows.next()` once, then drop), so the body was never finished and
/// every remote read threw its TCP + TLS socket away, shared pool or not. On
/// drop, the unread rest is read in the background and discarded, bounded by
/// [`DRAIN_LIMIT_BYTES`] and [`DRAIN_LIMIT`]; past either the socket is simply
/// closed, as before. Nothing the caller sees changes.
struct DrainOnDrop {
    inner: Option<ByteStream>,
}

impl DrainOnDrop {
    fn new(inner: ByteStream) -> Self {
        Self { inner: Some(inner) }
    }
}

impl Stream for DrainOnDrop {
    type Item = std::io::Result<Bytes>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let Some(inner) = self.inner.as_mut() else {
            return std::task::Poll::Ready(None);
        };
        let polled = std::pin::Pin::new(inner).poll_next(cx);
        if matches!(polled, std::task::Poll::Ready(None)) {
            // Finished: nothing left to drain.
            self.inner = None;
        }
        polled
    }
}

impl Drop for DrainOnDrop {
    fn drop(&mut self) {
        let Some(mut rest) = self.inner.take() else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        rt.spawn(async move {
            use futures::StreamExt;
            let _ = tokio::time::timeout(DRAIN_LIMIT, async move {
                let mut read = 0usize;
                while let Some(Ok(chunk)) = rest.next().await {
                    read += chunk.len();
                    if read > DRAIN_LIMIT_BYTES {
                        break;
                    }
                }
            })
            .await;
        });
    }
}

impl HttpSend for HttpSender {
    type Stream = super::HttpBody<ByteStream>;
    type Result = BoxFuture<'static, Result<Self::Stream>>;

    fn http_send(&self, url: Arc<str>, auth: Arc<str>, body: String) -> Self::Result {
        let fut = self.clone().send(url, auth, body);
        Box::pin(fut)
    }

    fn oneshot(self, url: Arc<str>, auth: Arc<str>, body: String) {
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(self.send(url, auth, body));
        } else {
            tracing::warn!("tried to send request to `{url}` while no runtime was available");
        }
    }
}

impl From<hyper::Error> for HranaError {
    fn from(value: hyper::Error) -> Self {
        HranaError::Http(value.to_string())
    }
}

impl HttpConnection<HttpSender> {
    /// POLLIS (#2179): a connection on its `Database`'s shared client.
    pub(crate) fn new_with_shared_client(
        url: impl Into<String>,
        token: impl Into<String>,
        client: SharedHttpClient,
        version: Option<&str>,
        namespace: Option<&str>,
        #[cfg(any(feature = "remote", feature = "sync"))] remote_encryption: Option<
            crate::database::EncryptionContext,
        >,
    ) -> Self {
        let inner = HttpSender::with_client(
            client,
            version,
            namespace,
            #[cfg(any(feature = "remote", feature = "sync"))]
            remote_encryption,
        );
        Self::new(url.into(), token.into(), inner)
    }

    pub(crate) fn new_with_connector(
        url: impl Into<String>,
        token: impl Into<String>,
        connector: ConnectorService,
        version: Option<&str>,
        namespace: Option<&str>,
        #[cfg(any(feature = "remote", feature = "sync"))] remote_encryption: Option<
            crate::database::EncryptionContext,
        >,
    ) -> Self {
        let inner = HttpSender::new(
            connector,
            version,
            namespace,
            #[cfg(any(feature = "remote", feature = "sync"))]
            remote_encryption,
        );
        Self::new(url.into(), token.into(), inner)
    }
}

#[async_trait::async_trait]
impl Conn for HttpConnection<HttpSender> {
    async fn execute(&self, sql: &str, params: Params) -> crate::Result<u64> {
        self.current_stream().execute(sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> crate::Result<BatchRows> {
        self.current_stream().execute_batch(sql).await
    }

    async fn execute_transactional_batch(&self, sql: &str) -> crate::Result<BatchRows> {
        self.current_stream().execute_transactional_batch(sql).await
    }

    async fn prepare(&self, sql: &str) -> crate::Result<Statement> {
        let stream = self.current_stream().clone();
        let stmt = crate::hrana::Statement::new(stream, sql.to_string(), true).await?;
        Ok(Statement {
            inner: Box::new(stmt),
        })
    }

    async fn transaction(
        &self,
        tx_behavior: crate::TransactionBehavior,
    ) -> crate::Result<crate::transaction::Transaction> {
        let stream = self.open_stream();
        let mut tx = HttpTransaction::open(stream, tx_behavior)
            .await
            .map_err(|e| crate::Error::Hrana(Box::new(e)))?;
        Ok(crate::Transaction {
            inner: Box::new(tx.clone()),
            conn: crate::Connection {
                conn: Arc::new(tx.stream().clone()),
            },
            close: Some(Box::new(|| {
                // make sure that Hrana connection is closed and all uncommitted changes
                // are rolled back when we're about to drop the transaction
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    // transaction will rollback automatically after timeout on the server side
                    // this is gracefull rollback on best-effort basis
                    rt.spawn(async move {
                        let _ = tx.rollback().await;
                    });
                }
            })),
        })
    }

    fn interrupt(&self) -> crate::Result<()> {
        // Interrupt is a no-op for remote connections.
        Ok(())
    }

    fn busy_timeout(&self, _timeout: Duration) -> crate::Result<()> {
        // Busy timeout is a no-op for remote connections.
        Ok(())
    }

    fn is_autocommit(&self) -> bool {
        self.is_autocommit()
    }

    fn changes(&self) -> u64 {
        self.affected_row_count()
    }

    fn total_changes(&self) -> u64 {
        self.total_changes()
    }

    fn last_insert_rowid(&self) -> i64 {
        self.last_insert_rowid()
    }

    async fn reset(&self) {
        self.current_stream().reset().await;
    }
}

#[async_trait::async_trait]
impl crate::statement::Stmt for crate::hrana::Statement<HttpSender> {
    fn finalize(&mut self) {}

    async fn execute(&self, params: &Params) -> crate::Result<usize> {
        self.execute(params).await
    }

    async fn query(&self, params: &Params) -> crate::Result<Rows> {
        self.query(params).await
    }

    async fn run(&self, params: &Params) -> crate::Result<()> {
        self.run(params).await
    }

    fn interrupt(&self) -> crate::Result<()> {
        Err(crate::Error::Misuse(
            "interrupt is not supported for remote connections".to_string(),
        ))
    }

    fn reset(&self) {}

    fn parameter_count(&self) -> usize {
        let stmt = &self.inner;
        stmt.args.len() + stmt.named_args.len()
    }

    fn parameter_name(&self, idx: i32) -> Option<&str> {
        //FIXME: actual rules of named args are pretty convoluted and may require full AST parsing. Here we basically
        //       assume, that if one needs a param name, they don't use named and un-named params mixed in.
        if !self.inner.args.is_empty() {
            return None;
        }
        let named_param = self.inner.named_args.get(idx as usize)?;
        Some(&named_param.name)
    }

    fn column_count(&self) -> usize {
        self.cols.len()
    }

    fn columns(&self) -> Vec<crate::Column> {
        //FIXME: there are several blockers here:
        // 1. We cannot know the column types before sending a query, so this method will never return results right
        //    away.
        // 2. Even if we do execute query, Hrana doesn't return all info that Column exposes.
        // 3. Even if we would like to return some of the column info ie. column [ValueType], this information is not
        //    present in Hrana [Col] but rather inferred from the row cell type.
        self.cols
            .iter()
            .map(|name| crate::Column {
                name,
                origin_name: None,
                table_name: None,
                database_name: None,
                decl_type: None,
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl Tx for HttpTransaction<HttpSender> {
    async fn commit(&mut self) -> crate::Result<()> {
        self.commit()
            .await
            .map_err(|e| crate::Error::Hrana(Box::new(e)))?;
        Ok(())
    }

    async fn rollback(&mut self) -> crate::Result<()> {
        self.rollback()
            .await
            .map_err(|e| crate::Error::Hrana(Box::new(e)))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Conn for HranaStream<HttpSender> {
    async fn execute(&self, sql: &str, params: Params) -> crate::Result<u64> {
        // SQLite: execute() will only execute a single SQL statement
        let mut parsed = crate::parser::Statement::parse(sql);
        let mut c = TxScopeCounter::default();
        if let Some(s) = parsed.next() {
            let s = s?;
            c.count(s.kind);
            let in_tx_scope = !self.is_autocommit() || c.begin_tx();
            let close = !in_tx_scope || c.end_tx();
            let mut stmt = Stmt::new(s.stmt, false);
            bind_params(params, &mut stmt);
            let result = self
                .execute_inner(stmt, close)
                .await
                .map_err(|e| crate::Error::Hrana(e.into()))?;
            Ok(result.affected_row_count)
        } else {
            Err(crate::Error::Misuse(
                "no SQL statement provided".to_string(),
            ))
        }
    }

    async fn execute_batch(&self, sql: &str) -> crate::Result<BatchRows> {
        let mut stmts = Vec::new();
        let parse = crate::parser::Statement::parse(sql);
        let mut c = TxScopeCounter::default();
        for s in parse {
            let s = s?;
            c.count(s.kind);
            stmts.push(Stmt::new(s.stmt, false));
        }
        let in_tx_scope = !self.is_autocommit() || c.begin_tx();
        let close = !in_tx_scope || c.end_tx();
        let res = self
            .batch_inner(Batch::from_iter(stmts), close)
            .await
            .map_err(|e| crate::Error::Hrana(e.into()))?;
        unwrap_err(&res)?;
        let rows = res
            .step_results
            .into_iter()
            .map(|r| r.map(StmtResultRows::new).map(Rows::new))
            .collect::<Vec<_>>();

        Ok(BatchRows::new(rows))
    }

    async fn execute_transactional_batch(&self, sql: &str) -> crate::Result<BatchRows> {
        let mut stmts = Vec::new();
        let parse = crate::parser::Statement::parse(sql);
        for s in parse {
            let s = s?;

            use crate::parser::StmtKind;
            if matches!(
                s.kind,
                StmtKind::TxnBegin | StmtKind::TxnBeginReadOnly | StmtKind::TxnEnd
            ) {
                return Err(Error::TransactionalBatchError(
                    "Transactions forbidden inside transactional batch".to_string(),
                ));
            }

            stmts.push(Stmt::new(s.stmt, false));
        }
        let res = self
            .batch_inner(Batch::transactional(stmts), true)
            .await
            .map_err(|e| crate::Error::Hrana(e.into()))?;
        unwrap_err(&res)?;
        let rows = res
            .step_results
            .into_iter()
            // skip the first row since this is related to the already injected
            // BEGIN statement.
            .skip(1)
            .map(|r| r.map(StmtResultRows::new).map(Rows::new))
            .collect::<Vec<_>>();

        // Skip the last row as well since this corresponds to the injected commit statement
        // that the user never sees.
        Ok(BatchRows::new_skip_last(rows, 2))
    }

    async fn prepare(&self, sql: &str) -> crate::Result<Statement> {
        let stmt = crate::hrana::Statement::new(self.clone(), sql.to_string(), true).await?;
        Ok(Statement {
            inner: Box::new(stmt),
        })
    }

    async fn transaction(
        &self,
        _tx_behavior: crate::TransactionBehavior,
    ) -> crate::Result<crate::transaction::Transaction> {
        todo!("sounds like nested transactions innit?")
    }

    fn interrupt(&self) -> crate::Result<()> {
        // Interrupt is a no-op for remote connections.
        Ok(())
    }

    fn busy_timeout(&self, _timeout: Duration) -> crate::Result<()> {
        // Busy timeout is a no-op for remote connections.
        Ok(())
    }

    fn is_autocommit(&self) -> bool {
        false // for streams this method is callable only when we're within explicit transaction
    }

    fn changes(&self) -> u64 {
        self.affected_row_count()
    }

    fn total_changes(&self) -> u64 {
        self.total_changes()
    }

    fn last_insert_rowid(&self) -> i64 {
        self.last_insert_rowid()
    }

    async fn reset(&self) {
        self.reset().await;
    }
}
