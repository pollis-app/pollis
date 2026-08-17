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

/// Per-connection PRAGMAs a [`Db`] stamps on EVERY connection it builds (#919).
///
/// A PRAGMA is one of two things: a property of the *file* (`journal_mode`) or a
/// property of the *connection* (`busy_timeout`, `foreign_keys`, `query_only`).
/// Setting a per-connection one on a single primed connection is not "setting it
/// on the database" — every other connection the pool ever builds gets libsql's
/// default instead. That is precisely the bug this type exists to make
/// unrepresentable: the set is chosen once, at construction, and [`Db::conn`]
/// applies it to every connection it creates, so no checkout can differ from any
/// other.
///
/// The defect it replaces: `connect_local` primed the FIRST connection and parked
/// it. Any *nested* checkout (a handler holding a `log_db` connection while taking
/// a `db` one — the pool is deliberately never a capacity limit) found the pool
/// empty and built a fresh connection with libsql's defaults, so `foreign_keys`
/// read `0` on connection A and `1` on connection B of the same `Db`. Fixtures
/// then passed or failed on how deeply the call under test nested its checkouts.
///
/// Each entry is issued with `query`, not `execute`: several PRAGMAs return the
/// resulting value as a row, which `execute` rejects. This mirrors
/// `pollis_core::db::remote::RemoteDb::conn`, which has always re-stamped its
/// per-connection PRAGMAs on every checkout.
type PerConnPragmas = &'static [&'static str];

/// Production (remote Turso). PRAGMAs are the server's business, and each entry
/// here would cost a network round trip per new connection.
const REMOTE_PRAGMAS: PerConnPragmas = &[];

/// A local file opened by this crate — `pollis-delivery`'s own test DBs.
///
/// `busy_timeout` so concurrent submitters serialize without surfacing "database
/// is locked" (the conditional INSERT still guarantees exactly one winner per
/// epoch).
///
/// **`foreign_keys=OFF` is production parity, not a fixture convenience.** Remote
/// Turso does not enable foreign-key enforcement, so the baseline schema's
/// `REFERENCES` and `ON DELETE CASCADE` clauses are inert on every deployment —
/// which is why, for example, `group_join_request.group_id REFERENCES groups(id)`
/// cannot be relied on and the existence check is explicit
/// (`tests/join_requests.rs`), and why migration `000016` keeps the conversation
/// registry append-only rather than trusting a cascade. libsql's LOCAL backend
/// turns FKs ON by default, so a local DB that says nothing is *stricter* than
/// any real deploy and would let a test pass for a reason production does not
/// have.
///
/// `journal_mode=WAL` is NOT here — it is a property of the file, set once in
/// [`Db::connect_local`].
const LOCAL_PRAGMAS: PerConnPragmas = &["PRAGMA busy_timeout=5000", "PRAGMA foreign_keys=OFF"];

/// A local file whose handle we were *given* — the `flows` harness (see
/// [`Db::from_shared`]).
///
/// `busy_timeout` matches what `RemoteDb::conn` puts on the *client* connections
/// to this same file, so neither side of that suite fails with "database is
/// locked" while the other holds the write lock. Previously `from_shared` set
/// nothing at all, so the DS half of the harness ran at libsql's default of 0.
///
/// **`foreign_keys=OFF`, matching production and [`LOCAL_PRAGMAS`].**
///
/// This used to be the one place that diverged. `flows` ran with foreign keys ON
/// — libsql's local default — so the harness executed *stricter* deletion
/// semantics than any real deploy, and the DS's cascade-dependent call sites
/// looked correct there while leaving rows behind on Turso
/// (`account::apply_delete_account` leaned on `DELETE FROM users` to clear
/// `user_device`, `dm_channel_member`, `group_invite`, `account_recovery` and
/// `security_event`; `groups::delete_group` leaned on it for members, channels
/// and invites). The divergence was recorded rather than fixed because flipping
/// it alone would have failed the suite on that very bug.
///
/// Those deletes are now explicit ([`crate::teardown`]), so the flip is safe and
/// is made here: `flows` is the suite that exists to mirror the real deployment,
/// and it now does. With FKs off, a future teardown path that forgets a table
/// leaves its rows behind in `flows` exactly as it would in production, which is
/// the only way that class of bug is visible to a test.
const SHARED_LOCAL_PRAGMAS: PerConnPragmas =
    &["PRAGMA busy_timeout=10000", "PRAGMA foreign_keys=OFF"];

pub struct Db {
    db: Arc<Database>,
    pool: Arc<Pool>,
    max_idle: Duration,
    /// Stamped on every connection this `Db` builds — see [`PerConnPragmas`].
    pragmas: PerConnPragmas,
}

impl Db {
    /// Connect to a remote Turso database (production).
    pub async fn connect_remote(url: &str, token: &str) -> Result<Self> {
        let db = Builder::new_remote(url.to_string(), token.to_string())
            .build()
            .await?;
        Ok(Self::wrap(Arc::new(db), REMOTE_PRAGMAS))
    }

    /// Connect to a local libsql file. Tests only — avoids the network and lets
    /// a test drive concurrent submitters against a single file.
    pub async fn connect_local(path: &str) -> Result<Self> {
        let db = Builder::new_local(path).build().await?;
        let me = Self::wrap(Arc::new(db), LOCAL_PRAGMAS);
        // `journal_mode` is a property of the FILE, so unlike [`LOCAL_PRAGMAS`]
        // it is set once here rather than per connection.
        me.conn().await?.execute_batch("PRAGMA journal_mode=WAL;").await?;
        Ok(me)
    }

    /// Wrap an ALREADY-OPEN libsql `Database` instead of opening one.
    ///
    /// For the `flows` integration harness (#918), which runs this crate's real
    /// router in-process against the same file its clients read: two independent
    /// `Database` handles on one local file do not share WAL writes promptly, so
    /// the DS must be given the client's handle rather than a second one. The
    /// pool below is per-`Db` and stays exclusive either way.
    ///
    /// Being handed the handle does NOT mean inheriting the giver's connection
    /// state: this `Db` still builds its own connections out of it, so it needs
    /// its own per-connection PRAGMAs — see [`SHARED_LOCAL_PRAGMAS`].
    pub fn from_shared(db: Arc<Database>) -> Self {
        Self::wrap(db, SHARED_LOCAL_PRAGMAS)
    }

    fn wrap(db: Arc<Database>, pragmas: PerConnPragmas) -> Self {
        Self {
            db,
            pool: Arc::new(Pool::default()),
            max_idle: DEFAULT_MAX_IDLE,
            pragmas,
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
    ///
    /// A *new* connection is stamped with this `Db`'s [`PerConnPragmas`] before
    /// it is handed out; a parked one already carries them, because it can only
    /// have come from here. So every checkout of a given `Db` sees the same
    /// per-connection state no matter how deeply checkouts nest (#919).
    ///
    /// `async` for exactly that reason — issuing a PRAGMA is a query. It still
    /// never blocks on the pool: an empty pool builds a connection rather than
    /// waiting, which is what lets handlers nest checkouts.
    pub async fn conn(&self) -> Result<ConnGuard> {
        let conn = match self.pool.take(self.max_idle) {
            Some(conn) => conn,
            None => {
                let conn = self.db.connect()?;
                for pragma in self.pragmas {
                    conn.query(pragma, ()).await?;
                }
                conn
            }
        };
        Ok(ConnGuard {
            conn: Some(conn),
            pool: Arc::clone(&self.pool),
        })
    }
}
