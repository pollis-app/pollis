//! Thin libsql wrapper. The Delivery Service is the *sole writer* to the MLS
//! control-plane tables (`mls_commit_log`, `mls_group_info`, `mls_welcome`,
//! `mls_key_package`), so all writes funnel through one place — here.
//!
//! ## Why connections are pooled (#777)
//!
//! A fresh remote `Connection` builds a new hyper client with an empty pool; reuse keeps it warm.
//!
//! Reuse is safe because:
//!
//! 1. **Checkout is exclusive.** No two callers hold one `Connection` (per-connection state, one stream).
//! 2. **Parked connections are bounded in age** since last *use* ([`DEFAULT_MAX_IDLE`], #1500).
//! 3. **A connection that saw a transport failure is never parked** ([`note_transport_fault`]).
//!
//! Transactions get their own hrana stream, so pooling never shares statement context.

use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use libsql::{Builder, Connection, Database};

/// How long a connection may sit parked; comfortably under the hrana stream idle timeout.
pub const DEFAULT_MAX_IDLE: Duration = Duration::from_secs(3);

/// Upper bound on parked connections; extras are dropped, as checkout must never block.
const MAX_PARKED: usize = 16;

struct Parked {
    conn: Connection,
    /// When the connection was last used — not when it was parked.
    since: Instant,
}

/// Transient database failures recorded since start ([`note_transport_fault`]).
/// A checkout remembers the value it saw; one that changed while it was out is
/// not parked again.
static TRANSPORT_FAULTS: AtomicU64 = AtomicU64::new(0);

/// Record a transient database failure (#1500): connections checked out now are dropped on release.
///
/// Coarse on purpose: dropping a healthy one only costs a reconnect.
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
    /// Take the freshest parked connection, or `None` if the pool is empty or
    /// everything in it has gone stale.
    fn take(&self, max_idle: Duration) -> Option<Parked> {
        let mut parked = self.parked.lock().unwrap();
        let entry = parked.pop()?;
        if entry.since.elapsed() < max_idle {
            return Some(entry);
        }
        // Entries are kept in last-used order, so everything still in the vec is
        // older than the one we just rejected. Drop the lot.
        parked.clear();
        None
    }

    fn park(&self, conn: Connection, since: Instant) {
        // A connection left mid-transaction must never be handed on.
        if !conn.is_autocommit() {
            return;
        }
        let mut parked = self.parked.lock().unwrap();
        if parked.len() >= MAX_PARKED {
            return;
        }
        // Kept in last-used order, which `take` relies on.
        let at = parked.partition_point(|p| p.since <= since);
        parked.insert(at, Parked { conn, since });
    }
}

/// An exclusively checked-out connection. Derefs to [`libsql::Connection`], and
/// returns it to the pool on drop.
pub struct ConnGuard {
    /// `Some` for the guard's whole life; `Drop` takes it to park it.
    conn: Option<Connection>,
    pool: Arc<Pool>,
    /// When a parked connection was last used before this checkout; `None` for
    /// a new one, which has no stream to go stale.
    last_used: Option<Instant>,
    /// Whether anything reached the connection; an untouched guard keeps its old age.
    touched: AtomicBool,
    /// [`TRANSPORT_FAULTS`] at checkout.
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

/// Per-connection PRAGMAs a [`Db`] stamps on EVERY connection it builds (#919).
///
/// Per-connection PRAGMAs go on every connection, or nested checkouts get libsql's defaults.
///
/// Issued with `query`, not `execute`: several PRAGMAs return a row.
type PerConnPragmas = &'static [&'static str];

/// Production (remote Turso). PRAGMAs are the server's business, and each entry
/// here would cost a network round trip per new connection.
const REMOTE_PRAGMAS: PerConnPragmas = &[];

/// A local file opened by this crate — `pollis-delivery`'s own test DBs.
///
/// `busy_timeout` so concurrent submitters serialize without "database is locked".
///
/// **`foreign_keys=OFF` is production parity**: Turso doesn't enforce FKs, so local tests mustn't.
///
/// `journal_mode=WAL` is per file, set in [`Db::connect_local`].
const LOCAL_PRAGMAS: PerConnPragmas = &["PRAGMA busy_timeout=5000", "PRAGMA foreign_keys=OFF"];

/// `Clone` is shallow: clones share the `Database` and the pool.
#[derive(Clone)]
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
        // libsql is built without its own `tls` feature (#1303), so without a
        // connector this would panic on the first query. See
        // `pollis-libsql-connector` for why it is not hyper-rustls.
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
        // `journal_mode` is a property of the FILE, so unlike [`LOCAL_PRAGMAS`]
        // it is set once here rather than per connection.
        me.conn().await?.execute_batch("PRAGMA journal_mode=WAL;").await?;
        Ok(me)
    }

    /// The common tail of [`Self::connect_remote`] and [`Self::connect_local`]:
    /// an empty pool plus the per-connection pragmas that backend needs.
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
    /// A *new* connection is stamped with this `Db`'s [`PerConnPragmas`] first (#919).
    ///
    /// Never blocks on the pool: an empty pool builds a connection.
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

/// Begin a transaction that writes: `BEGIN IMMEDIATE`, taking the write lock up front (#1890).
///
/// A deferred transaction that reads before it writes holds a read snapshot; if
/// any other writer commits in between (the spawned `last_seen` bump, another
/// request), the upgrade to a writer fails with `SQLITE_BUSY` at once, without
/// waiting out the busy timeout, and the request becomes a spurious 503.
/// `IMMEDIATE` waits for the lock instead and reads the state it then writes on.
///
/// Every DS write transaction opens through here; `clippy.toml` forbids the bare
/// `Connection::transaction`.
#[allow(clippy::disallowed_methods)]
pub async fn write_tx(conn: &Connection) -> Result<libsql::Transaction> {
    Ok(conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?)
}

/// Begin a READ-ONLY transaction: one consistent snapshot, no write lock.
///
/// Never write through it — that is [`write_tx`]'s job (see there for why).
#[allow(clippy::disallowed_methods)]
pub async fn read_tx(conn: &Connection) -> Result<libsql::Transaction> {
    Ok(conn
        .transaction_with_behavior(libsql::TransactionBehavior::Deferred)
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The write `revoke_device` lost in #1890.
    const BUMP: &str = "UPDATE conversation_membership_version SET version = version + 1 \
                        WHERE conversation_id = 'c'";

    /// Two connections on one WAL file and a row for both to update.
    async fn two_conns() -> (tempfile::TempDir, ConnGuard, ConnGuard) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::connect_local(dir.path().join("t.db").to_str().unwrap())
            .await
            .unwrap();
        let a = db.conn().await.unwrap();
        pollis_schema::apply::single_db(&a).await.unwrap();
        a.execute(
            "INSERT INTO conversation_membership_version (conversation_id, version) VALUES ('c', 0)",
            (),
        )
        .await
        .unwrap();
        let b = db.conn().await.unwrap();
        (dir, a, b)
    }

    async fn read_n(conn: &Connection) -> i64 {
        let mut rows = conn
            .query(
                "SELECT version FROM conversation_membership_version WHERE conversation_id = 'c'",
                (),
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    /// The #1890 race, pinned: another writer commits between a transaction's
    /// first read and its first write. Deferred, that transaction can no longer
    /// write at all, and the DS would answer with a spurious 503.
    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn a_deferred_transaction_that_read_first_loses_to_a_concurrent_writer() {
        let (_dir, a, b) = two_conns().await;
        let tx = a.transaction().await.unwrap();
        read_n(&tx).await;
        b.execute(BUMP, ()).await.unwrap();
        let err = anyhow::Error::new(tx.execute(BUMP, ()).await.expect_err("stale snapshot"));
        assert!(crate::error::is_transient_db_error(&err));
    }

    /// [`write_tx`] holds the write lock from BEGIN, so no writer can commit
    /// between its read and its write: the other writer is the one kept waiting
    /// (here, with no busy timeout, refused outright), and the transaction lands.
    #[tokio::test]
    async fn a_write_tx_that_read_first_cannot_be_overtaken() {
        let (_dir, a, b) = two_conns().await;
        b.query("PRAGMA busy_timeout=0", ()).await.unwrap();
        let tx = write_tx(&a).await.unwrap();
        read_n(&tx).await;
        b.execute(BUMP, ())
            .await
            .expect_err("the write lock is the transaction's from BEGIN");
        tx.execute(BUMP, ()).await.expect("the write it read for");
        tx.commit().await.unwrap();
        b.execute(BUMP, ()).await.expect("the lock is free again");
        assert_eq!(read_n(&b).await, 2);
    }
}
