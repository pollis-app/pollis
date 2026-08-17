//! Shared fixture plumbing for the `pollis-delivery` integration tests.
//!
//! # Why this exists (#942)
//!
//! Every fixture in this directory used to open a `tempfile::tempdir()`, build a
//! libsql database inside it, and then call `std::mem::forget(dir)`. The forget
//! was not gratuitous — a `TempDir` deletes its directory when it drops, and
//! dropping it at the end of the fixture function would pull the directory out
//! from under a database the test had not started using yet. Leaking the handle
//! was the shortest way to keep it alive.
//!
//! It is also permanent. Twenty-one test binaries each left a directory in
//! `/tmp` on every run, none of which anything ever removes.
//!
//! The fix is to give the guard a lifetime instead of destroying it: [`TempDb`]
//! owns both the database and the directory, so the directory lives exactly as
//! long as the test holds the fixture and is cleaned up when it does not.
//!
//! # Using it
//!
//! `TempDb` dereferences to `Arc<Db>`, so call sites are unchanged — `db.conn()`,
//! `Arc::clone(&db)` and `&db` where a `&Db` is wanted all still work through
//! deref coercion. The only rule is that the value must be **held for the
//! duration of the test** rather than dropped early, which is what binding it to
//! a `let` already does.
//!
//! # Drop order
//!
//! `db` is declared before `_dir` so it drops first: the database handles close,
//! and only then is the directory removed. Reversing the two would unlink files
//! sqlite still has open — harmless on Linux, needlessly untidy everywhere.
//!
//! # The same rule elsewhere
//!
//! Two fixtures leaked a `libsql::Database` handle rather than a directory, so
//! that a `Connection` borrowed from a `:memory:` database stayed valid after
//! the builder returned (`conversation_namespace.rs`, `schema_is_not_reinvented.rs`).
//! They now return the handle alongside the connection for the caller to hold.
//! `std::mem::forget` no longer appears anywhere in this crate, which is the
//! property worth keeping: "leak it so it stays alive" is never the only option,
//! and it is the one that cannot be undone.

#![allow(dead_code)]

use pollis_delivery::db::Db;
use std::ops::Deref;
use std::sync::Arc;
use tempfile::TempDir;

/// A libsql database in a temporary directory that is removed when this value
/// drops.
pub struct TempDb {
    db: Arc<Db>,
    _dir: TempDir,
}

impl TempDb {
    /// Open an empty database at `<tempdir>/<file>`. No schema is applied — the
    /// caller decides between `single_db`, `main_db`, `log_db` or raw
    /// migrations, exactly as it did before.
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

    /// A cloned handle, for the call sites that need an owned `Arc<Db>` and
    /// cannot get there by coercion.
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
