//! Cold-launch (or post-reconnect) MLS catch-up sweep.
//!
//! Single backend command that, given a user_id, enumerates every MLS
//! group the user is in (regular groups + DMs) and runs the catch-up
//! sequence (`poll_mls_welcomes_inner` once + `process_pending_commits_inner`
//! per group) so the local MLS state matches the server's published epoch
//! before the user can take any MLS-powered action.
//!
//! Closes the cold-launch race documented in issue #371 scenario 5: between
//! sign-in / unlock and the first time any per-call catch-up fires, a user
//! action (send_message, edit_message, voice join, screen-share) could run
//! against a stale epoch. Calling this at AppShell mount and awaiting it
//! before unlocking interactive UI closes that window.
//!
//! Best-effort per group: a single group's failure (e.g. revoked device,
//! transient Turso error) logs and continues to the next so one bad row
//! never blocks the rest of the sweep.

use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

pub async fn catch_up_all_mls_groups(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    let device_id = state.device_id.lock().await.clone();
    if let Some(ref did) = device_id {
        if let Err(e) =
            crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await
        {
            eprintln!("[mls-sweep] poll_mls_welcomes: {e}");
        }
    }

    let conn = state.remote_db.conn().await?;

    let mut group_ids: Vec<String> = Vec::new();
    let mut rows = conn
        .query(
            "SELECT g.id FROM groups g \
             JOIN group_member gm ON gm.group_id = g.id \
             WHERE gm.user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        group_ids.push(row.get::<String>(0)?);
    }
    drop(rows);

    let mut dm_ids: Vec<String> = Vec::new();
    let mut rows = conn
        .query(
            "SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        dm_ids.push(row.get::<String>(0)?);
    }
    drop(rows);

    eprintln!(
        "[mls-sweep] {user_id}: {} group(s), {} dm(s)",
        group_ids.len(),
        dm_ids.len()
    );

    // Regular groups: mls_group_id IS the group id; process_pending_commits
    // takes mls_group_id directly.
    for gid in &group_ids {
        if let Err(e) =
            crate::commands::mls::process_pending_commits_inner(state, gid, user_id).await
        {
            eprintln!("[mls-sweep] process_pending_commits for group {gid}: {e}");
        }
    }

    // DMs: mls_group_id IS the dm_channel_id.
    for did in &dm_ids {
        if let Err(e) =
            crate::commands::mls::process_pending_commits_inner(state, did, user_id).await
        {
            eprintln!("[mls-sweep] process_pending_commits for dm {did}: {e}");
        }
    }

    Ok(())
}
