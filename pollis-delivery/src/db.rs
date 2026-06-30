//! Thin libsql wrapper. The Delivery Service is the *sole writer* to the MLS
//! control-plane tables (`mls_commit_log`, `mls_group_info`, `mls_welcome`,
//! `mls_key_package`), so all writes funnel through one place — here.

use anyhow::Result;
use libsql::{Builder, Connection, Database};

pub struct Db {
    db: Database,
}

impl Db {
    /// Connect to a remote Turso database (production).
    pub async fn connect_remote(url: &str, token: &str) -> Result<Self> {
        let db = Builder::new_remote(url.to_string(), token.to_string())
            .build()
            .await?;
        Ok(Self { db })
    }

    /// Connect to a local libsql file. Tests only — avoids the network and lets
    /// a test drive concurrent submitters against a single file.
    pub async fn connect_local(path: &str) -> Result<Self> {
        let db = Builder::new_local(path).build().await?;
        let me = Self { db };
        // WAL + a busy timeout so concurrent submitters serialize without
        // surfacing "database is locked" — the conditional INSERT still
        // guarantees exactly one winner per epoch.
        let conn = me.conn()?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=OFF;",
        )
        .await?;
        Ok(me)
    }

    pub fn conn(&self) -> Result<Connection> {
        Ok(self.db.connect()?)
    }
}
