//! Thin libsql wrapper and connection pool. The DS is the sole writer to the
//! MLS control-plane tables, so all writes funnel through here.
//!
//! Connections are pooled because a remote `Database::connect()` builds a new
//! `hyper::Client` with an empty pool, paying a TCP + TLS handshake per request
//! (#777). Reuse stays safe because checkout is exclusive (a `Connection` holds
//! per-connection state and one hrana stream), parked connections are bounded
//! by age since last *use* so an expired stream is never reused, and a
//! connection that was out during a transport fault is never parked (#1500).
//! Transactions open their own stream. See `.codesight/wiki/database.md` →
//! "How the DS connects to it".

use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use libsql::{Builder, Connection, Database};

/// Comfortably under the server-side hrana stream idle timeout, so a reused
/// connection is never one whose stream expired underneath it.
pub const DEFAULT_MAX_IDLE: Duration = Duration::from_secs(3);

/// Upper bound on parked connections; extras are dropped. Reuse is never a
/// capacity limit, so checkout must never block: handlers nest checkouts
/// (`log_db` then `db`, the same pool in single-DB deploys).
const MAX_PARKED: usize = 16;

struct Parked {
    conn: Connection,
    /// When the connection was last used — not when it was parked.
    since: Instant,
}

/// A checkout remembers the value it saw; one that changed while it was out is
/// not parked again.
static TRANSPORT_FAULTS: AtomicU64 = AtomicU64::new(0);

/// Record a transient database failure (#1500): every connection checked out
/// right now is dropped on release instead of parked, because libsql keeps a
/// dead stream's baton. Coarse on purpose: the pool cannot tell which one
/// failed, and dropping a healthy one costs only a reconnect.
pub fn note_transport_fault() {
    TRANSPORT_FAULTS.fetch_add(1, Ordering::SeqCst);
}

fn transport_faults() -> u64 {
    TRANSPORT_FAULTS.load(Ordering::SeqCst)
}

/// Parked connections, most recently used last.
#[derive(Default)]
struct Pool {
    parked: Mutex<Vec<Parked>>,
}

impl Pool {
    fn take(&self, max_idle: Duration) -> Option<Parked> {
        let mut parked = self.parked.lock().unwrap();
        let entry = parked.pop()?;
        if entry.since.elapsed() < max_idle {
            return Some(entry);
        }
        // Last-used order: everything left is older than the rejected entry.
        parked.clear();
        None
    }

    fn park(&self, conn: Connection, since: Instant) {
        // Mid-transaction state must not be inherited by the next caller.
        if !conn.is_autocommit() {
            return;
        }
        let mut parked = self.parked.lock().unwrap();
        if parked.len() >= MAX_PARKED {
            return;
        }
        // `take` relies on last-used order.
        let at = parked.partition_point(|p| p.since <= since);
        parked.insert(at, Parked { conn, since });
    }
}

/// An exclusively checked-out connection. Derefs to [`libsql::Connection`], and
/// returns it to the pool on drop.
pub struct ConnGuard {
    conn: Option<Connection>,
    pool: Arc<Pool>,
    /// `None` for a new connection, which has no stream to go stale.
    last_used: Option<Instant>,
    /// Set by `Deref`, so an untouched guard re-parks the connection with its
    /// old age instead of a fresh one.
    touched: AtomicBool,
    faults_at_checkout: u64,
}

impl Deref for ConnGuard {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.touched.store(true, Ordering::Relaxed);
        self.conn
            .as_ref()
            .expect("connection is only taken in Drop")
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        if transport_faults() != self.faults_at_checkout {
            return;
        }
        let since = match self.last_used {
            Some(last_used) if !*self.touched.get_mut() => last_used,
            _ => Instant::now(),
        };
        self.pool.park(conn, since);
    }
}

/// Per-connection PRAGMAs a [`Db`] stamps on EVERY connection it builds (#919),
/// so no checkout, however nested, differs from another. Issued with `query`,
/// not `execute`: several PRAGMAs return a row, which `execute` rejects.
type PerConnPragmas = &'static [&'static str];

/// Remote Turso: PRAGMAs are the server's business, and each would cost a round
/// trip per new connection.
const REMOTE_PRAGMAS: PerConnPragmas = &[];

/// Local test DBs. `busy_timeout` so concurrent submitters serialize without
/// "database is locked".
///
/// `foreign_keys=OFF` is production parity: Turso does not enforce FKs, so the
/// baseline's `REFERENCES`/`ON DELETE CASCADE` are inert everywhere, while
/// libsql's local backend turns them ON by default and would let a test pass for
/// a reason production lacks.
const LOCAL_PRAGMAS: PerConnPragmas = &["PRAGMA busy_timeout=5000", "PRAGMA foreign_keys=OFF"];

/// `Clone` is shallow (same `Database`, same pool), so a spawned task can own a
/// handle without opening a second one.
#[derive(Clone)]
pub struct Db {
    db: Arc<Database>,
    pool: Arc<Pool>,
    max_idle: Duration,
    pragmas: PerConnPragmas,
}

impl Db {
    /// Connect to a remote Turso database (production).
    pub async fn connect_remote(url: &str, token: &str) -> Result<Self> {
        // libsql is built without its own `tls` (#1303): no connector panics on
        // the first query.
        let db = Builder::new_remote(url.to_string(), token.to_string())
            .connector(pollis_libsql_connector::Connector::new())
            .build()
            .await?;
        Ok(Self::wrap(Arc::new(db), REMOTE_PRAGMAS))
    }

    /// Connect to a local libsql file. Tests only.
    pub async fn connect_local(path: &str) -> Result<Self> {
        let db = Builder::new_local(path).build().await?;
        let me = Self::wrap(Arc::new(db), LOCAL_PRAGMAS);
        // A property of the file, so set once rather than per connection.
        me.conn().await?.execute_batch("PRAGMA journal_mode=WAL;").await?;
        Ok(me)
    }

    fn wrap(db: Arc<Database>, pragmas: PerConnPragmas) -> Self {
        Self {
            db,
            pool: Arc::new(Pool::default()),
            max_idle: DEFAULT_MAX_IDLE,
            pragmas,
        }
    }

    /// Tests only; production wants [`DEFAULT_MAX_IDLE`].
    #[doc(hidden)]
    pub fn with_max_idle(mut self, max_idle: Duration) -> Self {
        self.max_idle = max_idle;
        self
    }

    /// Check out a connection exclusively: a warm one from the pool, or a new
    /// one stamped with [`PerConnPragmas`]. Never waits on the pool, which is
    /// what lets handlers nest checkouts.
    pub async fn conn(&self) -> Result<ConnGuard> {
        let faults_at_checkout = transport_faults();
        let (conn, last_used) = match self.pool.take(self.max_idle) {
            Some(parked) => (parked.conn, Some(parked.since)),
            None => {
                let conn = self.db.connect()?;
                for pragma in self.pragmas {
                    conn.query(pragma, ()).await?;
                }
                (conn, None)
            }
        };
        Ok(ConnGuard {
            conn: Some(conn),
            pool: Arc::clone(&self.pool),
            last_used,
            touched: AtomicBool::new(false),
            faults_at_checkout,
        })
    }
}
