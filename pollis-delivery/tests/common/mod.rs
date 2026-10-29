//! Shared fixture plumbing for the `pollis-delivery` integration tests.
//!
//! `db` is declared before `_dir` so the database closes before the directory goes.

#![allow(dead_code)]

use pollis_delivery::db::Db;
use std::ops::Deref;
use std::sync::Arc;
use tempfile::TempDir;

/// A libsql database in a temp directory removed on drop.
pub struct TempDb {
    db: Arc<Db>,
    _dir: TempDir,
}

impl TempDb {
    /// Open an empty database at `<tempdir>/<file>`; no schema is applied.
    pub async fn open(file: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(file);
        let db = Db::connect_local(path.to_str().expect("utf-8 path"))
            .await
            .expect("local db");
        Self {
            db: Arc::new(db),
            _dir: dir,
        }
    }

    pub fn arc(&self) -> Arc<Db> {
        Arc::clone(&self.db)
    }
}

impl Deref for TempDb {
    type Target = Arc<Db>;

    fn deref(&self) -> &Arc<Db> {
        &self.db
    }
}
