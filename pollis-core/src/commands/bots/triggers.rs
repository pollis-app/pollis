//! Trigger claims (#1182, job loop step 5): several hosts of one bot see every
//! trigger, and the DS's `UNIQUE (installation_id, trigger_key)` decides which
//! of them acts. Event-driven: one call per trigger, no leases or keepalives.

use std::sync::Arc;

use pollis_api::bots::ClaimTriggerBody;

use crate::error::{Error, Result};
use crate::state::AppState;

/// Claim `(installation_id, trigger_key)` for this host, device-signed.
/// `true` when this host won; a retry by the winner answers `true` again, so
/// a job that failed after winning can run again without losing its claim.
pub async fn claim_trigger(state: &Arc<AppState>, installation_id: &str, trigger_key: &str) -> Result<bool> {
    let body = ClaimTriggerBody {
        installation_id: installation_id.to_string(),
        trigger_key: trigger_key.to_string(),
    };
    Ok(crate::commands::mls::ds_post_json(state, &body).await?.won)
}

/// Where a bot's job loop has got to in one channel: the newest message it has
/// finished with, as `(sent_at, message_id)`. Kept in the installation's
/// encrypted store. `None` for a channel it has never seen.
pub async fn trigger_cursor(state: &Arc<AppState>, channel_id: &str) -> Result<Option<(String, String)>> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_active)?;
    match db.conn().query_row(
        "SELECT sent_at, message_id FROM bot_trigger_cursor WHERE channel_id = ?1",
        rusqlite::params![channel_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    ) {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Record that the job loop has finished with `channel_id` up to `(sent_at, message_id)`.
/// An empty `message_id` marks a channel seen while it had no messages.
pub async fn set_trigger_cursor(state: &Arc<AppState>, channel_id: &str, sent_at: &str, message_id: &str) -> Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_active)?;
    db.conn().execute(
        "INSERT INTO bot_trigger_cursor (channel_id, sent_at, message_id) VALUES (?1, ?2, ?3)
         ON CONFLICT(channel_id) DO UPDATE SET sent_at = excluded.sent_at, message_id = excluded.message_id",
        rusqlite::params![channel_id, sent_at, message_id],
    )?;
    Ok(())
}

fn not_active() -> Error {
    Error::Other(anyhow::anyhow!("bot installation is not active"))
}
