//! Device-local message retention (lookback window) commands.
//!
//! Retention is a device-only setting: it bounds how much *local* message
//! history this device keeps so the encrypted local SQLite file does not grow
//! forever. It is stored in the local `ui_state` table (never synced to remote,
//! unlike the `preferences` mirror) and never deletes anything on Turso. It is
//! orthogonal to MLS epoch visibility — see the "bounded history" product
//! principle in CLAUDE.md.
//!
//! The actual storage + eviction primitives live in `db::local`; these are the
//! thin async command wrappers that take the shared `AppState` local DB.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

/// Read the configured retention window in days. `0` means Forever (no
/// eviction).
pub async fn get_message_retention(state: &Arc<AppState>) -> Result<i64> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("local database not open")))?;
    crate::db::local::get_message_retention_days(db.conn())
}

/// Set the retention window (one of `0`, `30`, `90`, `365`) and immediately run
/// an eviction sweep so the new window's effect is visible right away.
pub async fn set_message_retention(days: i64, state: &Arc<AppState>) -> Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("local database not open")))?;
    crate::db::local::set_message_retention_days(db.conn(), days)
}

/// Run an eviction sweep now — the lifecycle entry point used by the startup
/// and app-focus hooks. Returns the number of rows deleted. A no-op (returns
/// `Ok(0)`) when retention is Forever or no local DB is open yet.
pub async fn run_message_eviction(state: &Arc<AppState>) -> Result<usize> {
    let guard = state.local_db.lock().await;
    let Some(db) = guard.as_ref() else {
        return Ok(0);
    };
    crate::db::local::evict_old_messages(db.conn())
}
