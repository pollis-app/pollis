//! Thin libsql wrapper. The Delivery Service is the *sole writer* to the MLS
//! control-plane tables (`mls_commit_log`, `mls_group_info`, `mls_welcome`,
//! `mls_key_package`), so all writes funnel through one place — here.
//!
//! ## Why connections are pooled (#777)
//!
//! For a *remote* database `libsql::Database::connect()` is not a cheap handle
//! clone: it builds a whole new `hyper::Client` (`libsql::hrana::hyper::
//! HttpSender::new`). hyper's connection pool lives in the `Client`, so a fresh
//! `Client` starts with an empty pool and every request paid a full TCP + TLS
//! handshake before its first query. Handing the *same* `Connection` back out
//! keeps that hyper pool warm, so the handshake is paid once instead of once
//! per request.
//!
//! Two properties keep the reuse safe, and both are enforced here rather than
//! left to caller discipline:
//!
//! 1. **Checkout is exclusive.** [`Db::conn`] *removes* the connection from the
//!    pool and only [`ConnGuard`]'s `Drop` puts it back, so no two callers ever
//!    hold the same `Connection`. That matters because a `Connection` carries
//!    per-connection state (`changes()`, `last_insert_rowid()`) and a single
//!    hrana stream whose statements serialize on one mutex — sharing one
//!    process-wide would both corrupt those counters and serialize every DS
//!    query behind a single in-flight request.
//! 2. **Parked connections are bounded in age.** A hrana stream left open by a
//!    `query()` (describe + cursor do not close it) expires server-side after a
//!    short idle window; reusing a connection whose stream has expired would
//!    fail the *next* request. Anything parked longer than [`DEFAULT_MAX_IDLE`]
//!    is therefore dropped instead of reused, which is exactly the sporadic
//!    traffic case where the handshake cost does not matter anyway.
//!
//! Transactions are unaffected: hrana's `transaction()` calls `open_stream()`
//! and binds the `Transaction` to its *own* stream, so concurrent transactions
//! never share statement context. Pooling also does not widen concurrency —
//! before this, every request already got its own independent connection; the
//! pool only reuses them.

use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use libsql::{Builder, Connection, Database};

/// How long a connection may sit parked before it is thrown away instead of
/// reused. Comfortably under the server-side hrana stream idle timeout, so a
/// reused connection can never be one whose stream expired underneath it.
pub const DEFAULT_MAX_IDLE: Duration = Duration::from_secs(3);

/// Upper bound on parked connections. A burst wide enough to exceed this simply
/// drops the extras — reuse is an optimisation, never a capacity limit, so
/// checkout must never block (handlers nest checkouts: `lib.rs` holds a
/// `log_db` connection while taking a `db` one, and in single-DB deploys those
/// are the same pool).
const MAX_PARKED: usize = 16;

struct Parked {
    conn: Connection,
    since: Instant,
}

/// Parked connections, most recently parked last.
#[derive(Default)]
struct Pool {
    parked: Mutex<Vec<Parked>>,
}

impl Pool {
    /// Take the freshest parked connection, or `None` if the pool is empty or
    /// everything in it has gone stale.
    fn take(&self, max_idle: Duration) -> Option<Connection> {
        let mut parked = self.parked.lock().unwrap();
        let entry = parked.pop()?;
        if entry.since.elapsed() < max_idle {
            return Some(entry.conn);
        }
        // Entries are pushed in park order, so everything still in the vec is
        // older than the one we just rejected. Drop the lot.
        parked.clear();
        None
    }

    fn park(&self, conn: Connection) {
        // A connection left mid-transaction carries statement state the next
        // caller must not inherit. Handlers use `transaction()` (which gets its
        // own stream), so this should never trip — but the pool must not be the
        // thing that makes it representable.
        if !conn.is_autocommit() {
            return;
        }
        let mut parked = self.parked.lock().unwrap();
        if parked.len() >= MAX_PARKED {
            return;
        }
        parked.push(Parked {
            conn,
            since: Instant::now(),
        });
    }
}

/// An exclusively checked-out connection. Derefs to [`libsql::Connection`], and
/// returns it to the pool on drop.
pub struct ConnGuard {
    /// `Some` for the guard's whole life; `Drop` takes it to park it.
    conn: Option<Connection>,
    pool: Arc<Pool>,
}

impl Deref for ConnGuard {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("connection is only taken in Drop")
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.park(conn);
        }
    }
}

pub struct Db {
    db: Arc<Database>,
    pool: Arc<Pool>,
    max_idle: Duration,
}

impl Db {
    /// Connect to a remote Turso database (production).
    pub async fn connect_remote(url: &str, token: &str) -> Result<Self> {
        let db = Builder::new_remote(url.to_string(), token.to_string())
            .build()
            .await?;
        Ok(Self::new(db))
    }

    /// Connect to a local libsql file. Tests only — avoids the network and lets
    /// a test drive concurrent submitters against a single file.
    pub async fn connect_local(path: &str) -> Result<Self> {
        let db = Builder::new_local(path).build().await?;
        let me = Self::new(db);
        // WAL + a busy timeout so concurrent submitters serialize without
        // surfacing "database is locked" — the conditional INSERT still
        // guarantees exactly one winner per epoch. `journal_mode` is a property
        // of the file; the other two are per-connection.
        let conn = me.conn()?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=OFF;",
        )
        .await?;
        Ok(me)
    }

    fn new(db: Database) -> Self {
        Self::from_shared(Arc::new(db))
    }

    /// Wrap an ALREADY-OPEN libsql `Database` instead of opening one.
    ///
    /// For the `flows` integration harness (#918), which runs this crate's real
    /// router in-process against the same file its clients read: two independent
    /// `Database` handles on one local file do not share WAL writes promptly, so
    /// the DS must be given the client's handle rather than a second one. The
    /// pool below is per-`Db` and stays exclusive either way.
    pub fn from_shared(db: Arc<Database>) -> Self {
        Self {
            db,
            pool: Arc::new(Pool::default()),
            max_idle: DEFAULT_MAX_IDLE,
        }
    }

    /// Shrink the window in which a parked connection may be reused. Tests only
    /// — production wants [`DEFAULT_MAX_IDLE`].
    #[doc(hidden)]
    pub fn with_max_idle(mut self, max_idle: Duration) -> Self {
        self.max_idle = max_idle;
        self
    }

    /// Check out a connection: a warm one from the pool, or a new one. The
    /// caller holds it exclusively until the guard drops.
    pub fn conn(&self) -> Result<ConnGuard> {
        let conn = match self.pool.take(self.max_idle) {
            Some(conn) => conn,
            None => self.db.connect()?,
        };
        Ok(ConnGuard {
            conn: Some(conn),
            pool: Arc::clone(&self.pool),
        })
    }
}
