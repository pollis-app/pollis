//! Trigger claims (#1182, job loop step 5): several hosts of one bot see every
//! trigger, and the DS's `UNIQUE (installation_id, trigger_key)` decides which
//! of them acts. Event-driven: one call per trigger, no leases or keepalives.

use std::sync::Arc;

use pollis_api::bots::ClaimTriggerBody;

use crate::error::{Error, Result};
use crate::state::AppState;

/// Wake the members watching `group_id`'s realtime room after a job applied an
/// action: a job's store is not joined to that room, so the wake-up `send` makes
/// after a post reaches no one. Content-free (`payload` is a routing-only
/// `new_message` / `edited_message` / `deleted_message`), through the DS's
/// `SendData`, as any member's client sends it (#1182).
pub async fn wake_group_room(state: &Arc<AppState>, group_id: &str, payload: serde_json::Value) -> Result<()> {
    crate::commands::mls::ds_livekit_send_data(state, group_id, payload).await
}

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

/// The channel (conversation) and author of a message in the installation's
/// store, for checking an App's action on it (#2004). `None` when this host
/// has never seen it, or it was deleted.
pub async fn message_meta(state: &Arc<AppState>, message_id: &str) -> Result<Option<(String, String)>> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(not_active)?;
    match db.conn().query_row(
        "SELECT conversation_id, sender_id FROM message WHERE id = ?1 AND deleted_at IS NULL",
        rusqlite::params![message_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    ) {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The last slot (Unix seconds) the job loop finished for an App schedule
/// (#2004), kept in the installation's encrypted store beside the channel
/// cursors (`bot_trigger_cursor`, keyed `schedule:<name>`). `None` for a
/// schedule it has never seen.
pub async fn schedule_cursor(state: &Arc<AppState>, name: &str) -> Result<Option<u64>> {
    Ok(trigger_cursor(state, &format!("schedule:{name}"))
        .await?
        .and_then(|(slot, _)| slot.parse().ok()))
}

/// Record that the job loop has finished schedule `name` up to `slot`.
pub async fn set_schedule_cursor(state: &Arc<AppState>, name: &str, slot: u64) -> Result<()> {
    set_trigger_cursor(state, &format!("schedule:{name}"), &slot.to_string(), "").await
}

/// Whether `content` mentions `username` by `@username` (#843, #2004). The
/// rule lives in pollis-app-manifest, shared with `direct_mentions` and with
/// `pollis-app-dev`'s offline routing (#2006).
pub use pollis_app_manifest::mentions_username;

fn not_active() -> Error {
    Error::Other(anyhow::anyhow!("bot installation is not active"))
}

#[cfg(test)]
mod tests {
    use super::mentions_username;

    #[test]
    fn a_bot_is_mentioned_by_its_username_only() {
        assert!(mentions_username("hey @Triage_bot, look", "triage_bot"));
        assert!(mentions_username("@triage_bot", "triage_bot"));
        assert!(!mentions_username("@all please", "triage_bot"));
        assert!(!mentions_username("mail@triage_bot", "triage_bot"));
        assert!(!mentions_username("@triage_bots", "triage_bot"));
    }
}
