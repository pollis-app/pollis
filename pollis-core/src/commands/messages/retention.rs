//! Device-local message retention (lookback window) commands.
//!
//! Device-only: bounds local history and never deletes anything remote.

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

/// Run an eviction sweep now; returns rows deleted (0 when Forever or no local DB).
pub async fn run_message_eviction(state: &Arc<AppState>) -> Result<usize> {
    let guard = state.local_db.lock().await;
    let Some(db) = guard.as_ref() else {
        return Ok(0);
    };
    crate::db::local::evict_old_messages(db.conn())
}
