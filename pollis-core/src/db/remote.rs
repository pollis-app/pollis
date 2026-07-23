use libsql::{Builder, Database, Connection};
use tokio::sync::RwLock;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use crate::error::Result;

#[cfg(test)]
const BASELINE: &str = include_str!("migrations/000000_baseline.sql");

#[derive(Clone)]
enum Backend {
    Remote { url: String, token: String },
    Local { path: PathBuf },
}

pub struct RemoteDb {
    /// `Arc` so a [`query_only_view`](RemoteDb::query_only_view) can SHARE the
    /// exact same underlying libsql `Database` — two independent `Database`s on
    /// one local file don't share WAL writes promptly, so the view must wrap the
    /// same handle to see the writer's committed rows with no lag.
    db: Arc<RwLock<Database>>,
    backend: Backend,
    /// When set, every connection returned by [`conn`](RemoteDb::conn) issues
    /// `PRAGMA query_only=ON`, which rejects INSERT/UPDATE/DELETE — exactly like
    /// a read-only Turso token. `query_only` is per-CONNECTION, not per-database,
    /// so a writable `RemoteDb` and a `query_only_view` of it can share one
    /// `Database` yet enforce different write permissions. Defaults to `false`;
    /// production never sets it (the real read-only token enforces this server
    /// side). Used by the flows harness to prove the client is read-only-safe.
    query_only: bool,
    /// A DS-minted short-TTL read-only token that supersedes the baked
    /// `Backend::Remote` token once available (#393). `None` → use the baked
    /// token (pre-mint window / unconfigured DS / local backend). Set via
    /// [`set_remote_token`](RemoteDb::set_remote_token); read on every
    /// [`reconnect`](RemoteDb::reconnect).
    token_override: Arc<RwLock<Option<String>>>,
    /// Loopback address of the overlay SOCKS5 shim to route this DB's Hrana/TLS
    /// through (design §14.1). `None` → today's unchanged libsql `.build()` path
    /// (overlay off, or a local backend). `Some` → build with
    /// `.connector(overlay_connector(shim))`, so the TCP lands on the shim while
    /// the client TLS still terminates at the real Turso host. Carried on the
    /// struct so [`reconnect`](RemoteDb::reconnect) rebuilds through the overlay
    /// too. The shim's own policy decides overlay-vs-direct per host.
    ///
    /// Interior-mutable so the overlay can be turned on/off **at runtime**
    /// ([`set_overlay_shim`](RemoteDb::set_overlay_shim)) without swapping the
    /// `Arc<RemoteDb>` every reader holds: flipping the mode rebuilds the inner
    /// libsql `Database` through (or without) the connector while every
    /// `state.remote_db` handle stays valid. `std::sync::Mutex` — the critical
    /// section is a pointer read with no `.await` held, and `query_only_view`
    /// (sync) needs to snapshot it.
    overlay_shim: std::sync::Mutex<Option<SocketAddr>>,
}

/// Build a remote libsql `Database`, optionally routing through the overlay shim.
/// This is the single seam where the overlay connector is (or is not) attached —
/// `overlay_shim: None` is byte-for-byte today's `.build()` path.
async fn build_remote_database(
    url: &str,
    token: &str,
    overlay_shim: Option<SocketAddr>,
) -> Result<Database> {
    let builder = Builder::new_remote(url.to_string(), token.to_string());
    let db = match overlay_shim {
        Some(shim) => {
            builder
                .connector(crate::net::overlay::overlay_connector(shim)?)
                .build()
                .await?
        }
        None => builder.build().await?,
    };
    Ok(db)
}

impl RemoteDb {
    /// Connect to the remote database. The schema must already be up to date —
    /// run `pnpm db:apply <env>` before shipping a new schema version.
    ///
    /// Direct (no overlay) — equivalent to `connect_with_overlay(url, token, None)`.
    pub async fn connect(url: &str, token: &str) -> Result<Self> {
        Self::connect_with_overlay(url, token, None).await
    }

    /// Connect to the remote database, optionally routing through the overlay
    /// SOCKS5 shim (design §14.1). `overlay_shim: None` is the unchanged direct
    /// `.build()` path; `Some(addr)` attaches the libsql overlay connector.
    pub async fn connect_with_overlay(
        url: &str,
        token: &str,
        overlay_shim: Option<SocketAddr>,
    ) -> Result<Self> {
        let db = build_remote_database(url, token, overlay_shim).await?;
        Ok(Self {
            db: Arc::new(RwLock::new(db)),
            backend: Backend::Remote {
                url: url.to_string(),
                token: token.to_string(),
            },
            query_only: false,
            token_override: Arc::new(RwLock::new(None)),
            overlay_shim: std::sync::Mutex::new(overlay_shim),
        })
    }

    /// Connect to a local libsql file. Integration-test harness only — avoids
    /// the network round-trip against the shared test Turso, dropping the
    /// flows suite from minutes to seconds.
    ///
    /// WAL + busy_timeout are required: a single test exercises many
    /// concurrent clients writing to the same file, and SQLite's default
    /// rollback journal serializes writers with no wait, producing
    /// `database is locked` mid-flow.
    #[cfg(any(test, feature = "test-harness"))]
    pub async fn connect_local<P: Into<PathBuf>>(path: P) -> Result<Self> {
        let path = path.into();
        let db = Builder::new_local(&path).build().await?;
        let conn = db.connect()?;
        // WAL + synchronous=NORMAL must run via `query`: both PRAGMAs return
        // the resulting mode as a row, which `execute` rejects.
        conn.query("PRAGMA journal_mode=WAL", ()).await?;
        conn.query("PRAGMA synchronous=NORMAL", ()).await?;
        Ok(Self {
            db: Arc::new(RwLock::new(db)),
            backend: Backend::Local { path },
            query_only: false,
            token_override: Arc::new(RwLock::new(None)),
            // Local test backend never dials the network — no overlay.
            overlay_shim: std::sync::Mutex::new(None),
        })
    }

    /// A read-only VIEW that shares the exact same underlying `Database` handle
    /// as `self` but issues `PRAGMA query_only=ON` on every connection, so it
    /// rejects writes while still seeing every row the writable handle commits.
    ///
    /// This mirrors the production read-only Turso token: in prod the client
    /// holds a read-only token and every write goes through the Delivery
    /// Service. The flows harness gives each `TestClient` a `query_only_view`
    /// of the main DB while the in-process DS keeps the writable handle, so the
    /// suite FAILS on any stray direct client write — the definitive gate for
    /// flipping the client to a real read-only token.
    ///
    /// Sharing the `Arc<RwLock<Database>>` (rather than opening a second
    /// `Database` on the same file) is required: independent libsql `Database`s
    /// on one local file don't share WAL writes promptly, so a second handle
    /// wouldn't see the DS's writes. `query_only` is per-connection, so flipping
    /// it on the view alone is safe.
    #[cfg(any(test, feature = "test-harness"))]
    pub fn query_only_view(&self) -> Self {
        Self {
            db: Arc::clone(&self.db),
            backend: self.backend.clone(),
            query_only: true,
            token_override: Arc::clone(&self.token_override),
            // Snapshot the current overlay target (the view is a read-only
            // sibling; it never mutates or reconnects independently).
            overlay_shim: std::sync::Mutex::new(*self.overlay_shim.lock().unwrap()),
        }
    }

    pub async fn conn(&self) -> Result<Connection> {
        let db = self.db.read().await;
        let conn = db.connect()?;
        // `busy_timeout` is per-connection — set it on every new connection
        // for the local test backend so concurrent clients in a single test
        // wait for each other instead of failing with `database is locked`.
        if matches!(self.backend, Backend::Local { .. }) {
            // `query` (not `execute`) — PRAGMA busy_timeout returns the
            // resulting value as a row.
            conn.query("PRAGMA busy_timeout=10000", ()).await?;
        }
        // Read-only view: reject INSERT/UPDATE/DELETE on this connection,
        // exactly like a read-only Turso token. Per-connection, so it must be
        // set on every fresh connection. `query` (not `execute`) — some libsql
        // builds surface the PRAGMA's resulting value as a row.
        if self.query_only {
            conn.query("PRAGMA query_only=ON", ()).await?;
        }
        Ok(conn)
    }

    /// Rebuild the underlying libsql `Database`. Long-lived handles can be
    /// torn down by the server (TCP reset) or have their streams GC'd
    /// ("stream not found"); neither is recoverable from the existing handle.
    /// Callers that hit a transient Hrana error should `reconnect()` and retry.
    pub async fn reconnect(&self) -> Result<()> {
        let new_db = match &self.backend {
            Backend::Remote { url, token } => {
                // Prefer a DS-minted short-TTL token when one has been set;
                // otherwise the baked read-only token (pre-mint / unconfigured).
                let effective = self
                    .token_override
                    .read()
                    .await
                    .clone()
                    .unwrap_or_else(|| token.clone());
                // Rebuild through the overlay too, so a reconnect after a dropped
                // Hrana stream keeps the same routing as the initial connect (or
                // the routing most recently set via `set_overlay_shim`).
                let shim = *self.overlay_shim.lock().unwrap();
                build_remote_database(url, &effective, shim).await?
            }
            Backend::Local { path } => Builder::new_local(path).build().await?,
        };
        let mut db = self.db.write().await;
        *db = new_db;
        Ok(())
    }

    /// Point this DB's connections at the overlay shim (`Some`) or back to a
    /// direct dial (`None`), rebuilding the inner libsql `Database` in place so
    /// every `Arc<RemoteDb>` reader picks up the new routing without being
    /// swapped. This is the libsql half of runtime overlay apply (design §14):
    /// `set_overlay_mode` calls it on both `remote_db` and `log_db` when the mode
    /// crosses the off/non-off boundary, so Turso's Hrana/TLS starts (or stops)
    /// landing on the loopback shim. No-op — and Ok — for the local test backend
    /// (which never dials the network) and when the target is unchanged.
    pub async fn set_overlay_shim(&self, shim: Option<SocketAddr>) -> Result<()> {
        if matches!(self.backend, Backend::Local { .. }) {
            return Ok(());
        }
        {
            let mut cur = self.overlay_shim.lock().unwrap();
            if *cur == shim {
                return Ok(());
            }
            *cur = shim;
        }
        // Rebuild the handle so the connector (or its absence) takes effect now.
        self.reconnect().await
    }

    /// The overlay shim this DB currently routes through, if any. Test-only:
    /// lets the overlay-apply tests assert the DB was (re)pointed live.
    #[cfg(any(test, feature = "test-harness"))]
    pub fn overlay_shim(&self) -> Option<SocketAddr> {
        *self.overlay_shim.lock().unwrap()
    }

    /// Swap in a DS-minted short-TTL read-only token (#393) and rebuild the
    /// underlying handle so every subsequent connection uses it. No-op for the
    /// local test backend (which has no token). On mint failure the caller keeps
    /// the current (baked) token, so reads never break.
    pub async fn set_remote_token(&self, token: String) -> Result<()> {
        if matches!(self.backend, Backend::Local { .. }) {
            return Ok(());
        }
        *self.token_override.write().await = Some(token);
        self.reconnect().await
    }

    /// Cheap round-trip to verify the connection is alive. Used by the
    /// keepalive task and by `heal_if_stale`.
    pub async fn ping(&self) -> std::result::Result<(), libsql::Error> {
        let conn = {
            let db = self.db.read().await;
            db.connect()?
        };
        conn.query("SELECT 1", ()).await?;
        Ok(())
    }

    /// Probe the connection; if the probe fails with a transient error,
    /// reconnect. Non-transient failures are surfaced. Safe to call
    /// concurrently — callers may observe a reconnect in progress but will
    /// block only briefly on the write lock.
    pub async fn heal_if_stale(&self) -> Result<()> {
        match self.ping().await {
            Ok(()) => Ok(()),
            Err(e) if is_transient_libsql_error(&e) => {
                eprintln!("[remote_db] ping failed ({e}); reconnecting");
                self.reconnect().await
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Run a DB operation with transparent reconnect on transient libsql
    /// failures. The closure receives a fresh `Connection`; if it returns a
    /// transient error on the first try, `RemoteDb` rebuilds the underlying
    /// `Database` and invokes the closure again. Non-transient errors — and
    /// transient errors on the second attempt — are surfaced.
    ///
    /// Use this at call sites where a single operation failing mid-flight
    /// would force the user to restart the app (message send, list fetches
    /// after wake-from-sleep). For multi-statement flows, either wrap each
    /// statement individually or accept that a mid-flow reset aborts the
    /// whole operation.
    pub async fn with_retry<F, Fut, T>(&self, op: F) -> Result<T>
    where
        F: Fn(Connection) -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, libsql::Error>>,
    {
        let conn = self.conn().await?;
        match op(conn).await {
            Ok(v) => Ok(v),
            Err(e) if is_transient_libsql_error(&e) => {
                eprintln!("[remote_db] transient error ({e}); reconnecting and retrying once");
                self.reconnect().await?;
                let conn = self.conn().await?;
                Ok(op(conn).await?)
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Heuristic: does this libsql error look like a transient connection/stream
/// failure that a `reconnect()` + retry can recover from? libsql's error enum
/// doesn't distinguish these structurally, so match on the rendered message.
pub fn is_transient_libsql_error(e: &libsql::Error) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("connection reset")
        || s.contains("connection refused")
        || s.contains("connection closed")
        || s.contains("connection error")
        || s.contains("broken pipe")
        || s.contains("stream not found")
        || s.contains("stream expired")
        || s.contains("timed out")
}

// Remote schema tests use rusqlite in-memory to avoid a SQLite threading
// conflict: libsql-sys bundles SQLite with SQLITE_THREADSAFE=0, which clashes
// with rusqlite-bundled's multi-threaded configuration when both exist in the
// same test binary. The SQL dialect is identical so coverage is equivalent.
#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(super::BASELINE).unwrap();
        conn
    }

    #[test]
    fn migration_creates_tables() {
        let conn = db();
        // Each insert will fail if the table doesn't exist.
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@example.com', 'u1')", []).expect("users");
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Test', 'u1')", []).expect("groups");
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'u1')", []).expect("group_member");
        conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'general')", []).expect("channels");
        conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('e1', 'c1', 'u1', 'enc', '2024-01-01T00:00:00Z')", []).expect("message_envelope");
    }

    #[test]
    fn user_email_must_be_unique() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'same@example.com', 'u1')", []).unwrap();
        let result = conn.execute("INSERT INTO users (id, email, username) VALUES ('u2', 'same@example.com', 'u2')", []);
        assert!(result.is_err(), "duplicate email should violate UNIQUE constraint");
    }

    #[test]
    fn user_fields_roundtrip() {
        let conn = db();
        conn.execute(
            "INSERT INTO users (id, email, username, avatar_url)
             VALUES ('u1', 'alice@example.com', 'alice', 'https://example.com/avatar.png')",
            [],
        ).unwrap();

        let (id, email, username, avatar_url): (String, String, String, String) =
            conn.query_row(
                "SELECT id, email, username, avatar_url FROM users WHERE id = 'u1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).unwrap();

        assert_eq!(id, "u1");
        assert_eq!(email, "alice@example.com");
        assert_eq!(username, "alice");
        assert_eq!(avatar_url, "https://example.com/avatar.png");
    }

    #[test]
    fn group_with_admin_and_member() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('admin', 'admin@x.com', 'admin')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('member', 'member@x.com', 'member')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Crew', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'admin', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'member', 'member')", []).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(count, 2);
    }

    #[test]
    fn channel_belongs_to_group() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'u@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();

        for name in ["general", "random", "announcements"] {
            conn.execute(
                "INSERT INTO channels (id, group_id, name) VALUES (?1, 'g1', ?2)",
                rusqlite::params![format!("ch-{name}"), name],
            ).unwrap();
        }

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM channels WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(count, 3);
    }

    // ── Group roles ──────────────────────────────────────────────────────────

    #[test]
    fn group_member_defaults_to_member_role() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        // No role supplied — should default to 'member'
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'u1')", []).unwrap();

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'u1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "member");
    }

    #[test]
    fn creator_is_inserted_as_admin() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'u1', 'admin')", []).unwrap();

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'u1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin");
    }

    #[test]
    fn set_member_role_toggles_between_admin_and_member() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u2', 'b@x.com', 'u2')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'u1', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'u2', 'member')", []).unwrap();

        // Promote u2 to admin
        conn.execute(
            "UPDATE group_member SET role = 'admin' WHERE group_id = 'g1' AND user_id = 'u2'",
            [],
        ).unwrap();
        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'u2'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin");

        // Demote back to member
        conn.execute(
            "UPDATE group_member SET role = 'member' WHERE group_id = 'g1' AND user_id = 'u2'",
            [],
        ).unwrap();
        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'u2'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "member");
    }

    #[test]
    fn migration_008_owner_role_becomes_admin() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        // Simulate pre-migration data
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'u1', 'owner')", []).unwrap();

        conn.execute("UPDATE group_member SET role = 'admin' WHERE role = 'owner'", []).unwrap();

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'u1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin", "migration should have renamed 'owner' to 'admin'");
    }

    #[test]
    fn duplicate_membership_violates_primary_key() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'u1')", []).unwrap();

        let result = conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'u1')", []);
        assert!(result.is_err(), "duplicate (group_id, user_id) should violate PRIMARY KEY");
    }

    #[test]
    fn admin_role_check_matches_only_admin() {
        // Mirrors the SQL pattern used in every admin-gated command
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('a', 'a@x.com', 'a')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('m', 'm@x.com', 'm')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'a')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'a', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'm', 'member')", []).unwrap();

        let admin_check = |user_id: &str| -> Option<String> {
            conn.query_row(
                "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = ?1",
                rusqlite::params![user_id],
                |row| row.get(0),
            ).ok()
        };

        assert_eq!(admin_check("a").as_deref(), Some("admin"));
        assert_ne!(admin_check("m").as_deref(), Some("admin"));
        assert_eq!(admin_check("unknown"), None);
    }

    #[test]
    fn remove_member_leaves_admin_intact() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('a', 'a@x.com', 'a')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('m', 'm@x.com', 'm')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'a')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'a', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'm', 'member')", []).unwrap();

        conn.execute("DELETE FROM group_member WHERE group_id = 'g1' AND user_id = 'm'", []).unwrap();

        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(remaining, 1);

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'a'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin");
    }

    #[test]
    fn delete_group_cascades_to_members_and_channels() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'u1', 'admin')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'general')", []).unwrap();

        conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

        let members: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        let channels: i64 = conn.query_row(
            "SELECT COUNT(*) FROM channels WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(members, 0, "group_member rows should cascade delete");
        assert_eq!(channels, 0, "channel rows should cascade delete");
    }

    // ── Invites ──────────────────────────────────────────────────────────────

    #[test]
    fn invite_can_be_created_and_queried() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u2', 'b@x.com', 'u2')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'u1', 'u2')",
            [],
        ).unwrap();

        // All rows in group_invite are implicitly pending — accepted/declined rows are deleted.
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_invite WHERE invitee_id = 'u2'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn invite_deleted_on_accept_or_decline() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u2', 'b@x.com', 'u2')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u3', 'c@x.com', 'u3')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'u1', 'u2')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv2', 'g1', 'u1', 'u3')",
            [],
        ).unwrap();

        // Accept / decline both delete the row.
        conn.execute("DELETE FROM group_invite WHERE id = 'inv1'", []).unwrap();
        conn.execute("DELETE FROM group_invite WHERE id = 'inv2'", []).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_invite WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "both invite rows should be gone after accept/decline");
    }

    // ── Join requests ────────────────────────────────────────────────────────

    #[test]
    fn join_request_defaults_to_pending() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u2', 'b@x.com', 'u2')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id) VALUES ('jr1', 'g1', 'u2')",
            [],
        ).unwrap();

        let status: String = conn.query_row(
            "SELECT status FROM group_join_request WHERE id = 'jr1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(status, "pending");
    }

    #[test]
    fn join_request_approve_and_reject_flows() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('admin', 'a@x.com', 'admin')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u2', 'b@x.com', 'u2')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u3', 'c@x.com', 'u3')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'admin')", []).unwrap();
        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id) VALUES ('jr1', 'g1', 'u2')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id) VALUES ('jr2', 'g1', 'u3')",
            [],
        ).unwrap();

        conn.execute(
            "UPDATE group_join_request SET status = 'approved', reviewed_by = 'admin' WHERE id = 'jr1'",
            [],
        ).unwrap();
        conn.execute(
            "UPDATE group_join_request SET status = 'rejected', reviewed_by = 'admin' WHERE id = 'jr2'",
            [],
        ).unwrap();

        let s1: String = conn.query_row("SELECT status FROM group_join_request WHERE id = 'jr1'", [], |r| r.get(0)).unwrap();
        let s2: String = conn.query_row("SELECT status FROM group_join_request WHERE id = 'jr2'", [], |r| r.get(0)).unwrap();
        assert_eq!(s1, "approved");
        assert_eq!(s2, "rejected");
    }

    #[test]
    fn join_request_rejects_invalid_status() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u2', 'b@x.com', 'u2')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id) VALUES ('jr1', 'g1', 'u2')",
            [],
        ).unwrap();

        let result = conn.execute("UPDATE group_join_request SET status = 'bogus' WHERE id = 'jr1'", []);
        assert!(result.is_err(), "CHECK constraint should reject invalid status");
    }

    // ── DM channels ──────────────────────────────────────────────────────────

    #[test]
    fn dm_channel_with_two_members() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u2', 'b@x.com', 'u2')", []).unwrap();
        conn.execute("INSERT INTO dm_channel (id, created_by) VALUES ('dm1', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', 'u1', 'u1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', 'u2', 'u1')",
            [],
        ).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn dm_channel_delete_cascades_to_members() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u1', 'a@x.com', 'u1')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('u2', 'b@x.com', 'u2')", []).unwrap();
        conn.execute("INSERT INTO dm_channel (id, created_by) VALUES ('dm1', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', 'u1', 'u1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', 'u2', 'u1')",
            [],
        ).unwrap();

        conn.execute("DELETE FROM dm_channel WHERE id = 'dm1'", []).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "dm_channel_member rows should cascade delete");
    }

    // ── Attachment dedup ─────────────────────────────────────────────────────

    #[test]
    fn attachment_object_deduplicates_by_content_hash() {
        let conn = db();
        conn.execute(
            "INSERT INTO attachment_object (content_hash, r2_key)
             VALUES ('abc123', 'r2/abc123')",
            [],
        ).unwrap();

        // Same content_hash from a different upload must fail
        let result = conn.execute(
            "INSERT INTO attachment_object (content_hash, r2_key)
             VALUES ('abc123', 'r2/different')",
            [],
        );
        assert!(result.is_err(), "duplicate content_hash should violate PRIMARY KEY");

        // Different hash must succeed
        conn.execute(
            "INSERT INTO attachment_object (content_hash, r2_key)
             VALUES ('def456', 'r2/def456')",
            [],
        ).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM attachment_object",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn message_envelope_delivered_flag() {
        let conn = db();

        for i in 1..=3i64 {
            conn.execute(
                "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at)
                 VALUES (?1, 'conv1', 'u1', 'enc', '2024-01-01T00:00:00Z')",
                rusqlite::params![format!("e{i}")],
            ).unwrap();
        }

        conn.execute("UPDATE message_envelope SET delivered = 1 WHERE id = 'e1'", []).unwrap();

        let undelivered: i64 = conn.query_row(
            "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = 'conv1' AND delivered = 0",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(undelivered, 2, "2 undelivered envelopes should remain");
    }
}
