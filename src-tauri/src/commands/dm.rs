use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;
use ulid::Ulid;

use crate::commands::blocks::is_blocked_either_way;
use crate::error::{Error, Result};
use crate::state::AppState;

/// Generic error string returned whenever a send is suppressed because
/// of a block — same phrasing whether the recipient has actively
/// blocked the sender or simply hasn't accepted yet. Keeping this
/// deliberately uninformative prevents the sender from inferring their
/// block status.
pub const BLOCK_ERR: &str = "message request pending";

#[derive(Debug, Serialize, Deserialize)]
pub struct DmChannel {
    pub id: String,
    pub created_by: String,
    pub created_at: String,
    pub members: Vec<DmChannelMember>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DmChannelMember {
    pub user_id: String,
    pub username: Option<String>,
    pub avatar_url: Option<String>,
    pub added_by: String,
    pub added_at: String,
    pub accepted_at: Option<String>,
}

/// Fetch members of a DM channel from the remote DB.
async fn fetch_dm_members(
    conn: &libsql::Connection,
    dm_channel_id: &str,
) -> Result<Vec<DmChannelMember>> {
    let mut rows = conn.query(
        "SELECT dcm.user_id, u.username, u.avatar_url, dcm.added_by, dcm.added_at, dcm.accepted_at
         FROM dm_channel_member dcm
         LEFT JOIN users u ON u.id = dcm.user_id
         WHERE dcm.dm_channel_id = ?1",
        libsql::params![dm_channel_id],
    ).await?;

    let mut members = Vec::new();
    while let Some(row) = rows.next().await? {
        members.push(DmChannelMember {
            user_id: row.get(0)?,
            username: row.get(1)?,
            avatar_url: row.get(2)?,
            added_by: row.get(3)?,
            added_at: row.get(4)?,
            accepted_at: row.get(5)?,
        });
    }
    Ok(members)
}


#[tauri::command]
pub async fn create_dm_channel(
    creator_id: String,
    member_ids: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<DmChannel> {
    // Require at least one other participant.
    let has_other = member_ids.iter().any(|id| id != &creator_id);
    if !has_other {
        return Err(Error::Other(anyhow::anyhow!("cannot create a DM with only yourself")));
    }

    let conn = state.remote_db.conn().await?;

    // Refuse channel creation if ANY proposed pairing is blocked in
    // either direction. Return the generic BLOCK_ERR so neither side
    // can infer why their DM failed.
    for other_id in member_ids.iter().filter(|id| *id != &creator_id) {
        if is_blocked_either_way(&conn, &creator_id, other_id).await? {
            return Err(Error::Other(anyhow::anyhow!(BLOCK_ERR)));
        }
    }

    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO dm_channel (id, created_by, created_at) VALUES (?1, ?2, ?3)",
        libsql::params![id.clone(), creator_id.clone(), now.clone()],
    ).await?;

    // Creator is auto-accepted (they initiated the conversation).
    conn.execute(
        "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at, accepted_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        libsql::params![id.clone(), creator_id.clone(), creator_id.clone(), now.clone()],
    ).await?;

    // Every other member starts with accepted_at = NULL — the channel
    // is a pending request until they accept it.
    for member_id in &member_ids {
        if member_id == &creator_id {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at, accepted_at)
             VALUES (?1, ?2, ?3, ?4, NULL)",
            libsql::params![id.clone(), member_id.clone(), creator_id.clone(), now.clone()],
        ).await?;
    }

    let members = fetch_dm_members(&conn, &id).await?;

    // Initialize watermark rows for all members so envelope cleanup can proceed.
    for member in &members {
        if let Err(e) = conn.execute(
            "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, last_fetched_at) VALUES (?1, ?2, datetime('now'))",
            libsql::params![id.clone(), member.user_id.clone()],
        ).await {
            eprintln!("[watermark] create_dm_channel: watermark init failed for {}: {e}", member.user_id);
        }
    }

    // Initialise the MLS group for this DM (creator becomes the sole member).
    // Reconcile then adds all members' devices (including creator's other devices).
    match crate::commands::mls::init_mls_group(state.inner(), &id, &creator_id).await {
        Ok(()) => {
            if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
                state.inner(), &id, &creator_id,
            ).await {
                eprintln!("[mls] create_dm_channel: reconcile failed: {e}");
            }
        }
        Err(e) => eprintln!("[mls] create_dm_channel: mls group init failed (non-fatal): {e}"),
    }

    // Notify non-creator members via their personal inbox rooms so they see
    // the new DM channel immediately without needing to refresh.
    let inbox_payload = serde_json::json!({
        "type": "dm_created",
        "conversation_id": id,
    });
    for member in members.iter().filter(|m| m.user_id != creator_id) {
        if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
            &state.config, &member.user_id, inbox_payload.clone(),
        ).await {
            eprintln!("[inbox] create_dm_channel: notify {} failed: {e}", member.user_id);
        }
    }

    Ok(DmChannel {
        id,
        created_by: creator_id,
        created_at: now,
        members,
    })
}

#[tauri::command]
pub async fn list_dm_channels(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<DmChannel>> {
    let conn = state.remote_db.conn().await?;

    // Accepted DMs only. Filter hides channels with users I have
    // blocked — but NOT channels where I am the blocked party. The
    // blocked user must continue to see the conversation so the
    // block produces no observable signal on their side (messages
    // simply stay in the [pending] state, indistinguishable from a
    // recipient who hasn't accepted yet).
    let mut rows = conn.query(
        "SELECT dc.id, dc.created_by, dc.created_at
         FROM dm_channel dc
         JOIN dm_channel_member dcm ON dcm.dm_channel_id = dc.id
         WHERE dcm.user_id = ?1
           AND dcm.accepted_at IS NOT NULL
           AND NOT EXISTS (
             SELECT 1
             FROM dm_channel_member other
             JOIN user_block ub ON
                  ub.blocker_id = ?1 AND ub.blocked_id = other.user_id
             WHERE other.dm_channel_id = dc.id
               AND other.user_id <> ?1
           )",
        libsql::params![user_id],
    ).await?;

    let mut channels = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let created_by: String = row.get(1)?;
        let created_at: String = row.get(2)?;

        let members = fetch_dm_members(&conn, &id).await?;

        channels.push(DmChannel {
            id,
            created_by,
            created_at,
            members,
        });
    }

    Ok(channels)
}

#[tauri::command]
pub async fn list_dm_requests(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<DmChannel>> {
    let conn = state.remote_db.conn().await?;

    // Pending requests: my own row is un-accepted and I have not
    // blocked the other participant. Symmetric to list_dm_channels —
    // we filter on blocks I have made, not blocks made against me.
    // A user I blocked never reappears in my requests list; when I
    // unblock them, their unaccepted channel surfaces here again.
    let mut rows = conn.query(
        "SELECT dc.id, dc.created_by, dc.created_at
         FROM dm_channel dc
         JOIN dm_channel_member dcm ON dcm.dm_channel_id = dc.id
         WHERE dcm.user_id = ?1
           AND dcm.accepted_at IS NULL
           AND NOT EXISTS (
             SELECT 1
             FROM dm_channel_member other
             JOIN user_block ub ON
                  ub.blocker_id = ?1 AND ub.blocked_id = other.user_id
             WHERE other.dm_channel_id = dc.id
               AND other.user_id <> ?1
           )
         ORDER BY dc.created_at DESC",
        libsql::params![user_id],
    ).await?;

    let mut channels = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let created_by: String = row.get(1)?;
        let created_at: String = row.get(2)?;

        let members = fetch_dm_members(&conn, &id).await?;

        channels.push(DmChannel {
            id,
            created_by,
            created_at,
            members,
        });
    }

    Ok(channels)
}

#[tauri::command]
pub async fn accept_dm_request(
    dm_channel_id: String,
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;
    let now = chrono::Utc::now().to_rfc3339();

    // Only flip accepted_at when it's currently NULL — idempotent
    // and preserves the original acceptance time if the user
    // clicks accept twice.
    conn.execute(
        "UPDATE dm_channel_member
         SET accepted_at = ?3
         WHERE dm_channel_id = ?1
           AND user_id = ?2
           AND accepted_at IS NULL",
        libsql::params![dm_channel_id, user_id, now],
    ).await?;

    Ok(())
}

#[tauri::command]
pub async fn get_dm_channel(
    dm_channel_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<DmChannel> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT id, created_by, created_at FROM dm_channel WHERE id = ?1",
        libsql::params![dm_channel_id.clone()],
    ).await?;

    let (id, created_by, created_at) = if let Some(row) = rows.next().await? {
        (row.get::<String>(0)?, row.get::<String>(1)?, row.get::<String>(2)?)
    } else {
        return Err(Error::Other(anyhow::anyhow!("DM channel not found: {dm_channel_id}")));
    };

    let members = fetch_dm_members(&conn, &id).await?;

    Ok(DmChannel { id, created_by, created_at, members })
}

#[tauri::command]
pub async fn add_user_to_dm_channel(
    dm_channel_id: String,
    user_id: String,
    added_by: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at)
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![dm_channel_id.clone(), user_id.clone(), added_by.clone(), now],
    ).await?;

    // Initialize watermark for the new member so pre-join messages don't
    // block envelope cleanup indefinitely.
    if let Err(e) = conn.execute(
        "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, last_fetched_at) VALUES (?1, ?2, datetime('now'))",
        libsql::params![dm_channel_id.clone(), user_id.clone()],
    ).await {
        eprintln!("[watermark] add_user_to_dm_channel: watermark init failed: {e}");
    }

    // Reconcile adds the new member's devices to the MLS tree.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state.inner(), &dm_channel_id, &added_by,
    ).await {
        eprintln!("[mls] add_user_to_dm_channel: reconcile: {e}");
    }

    Ok(())
}

#[tauri::command]
pub async fn remove_user_from_dm_channel(
    dm_channel_id: String,
    user_id: String,
    requester_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Only the channel creator or the user themselves can remove
    let mut rows = conn.query(
        "SELECT created_by FROM dm_channel WHERE id = ?1",
        libsql::params![dm_channel_id.clone()],
    ).await?;

    let creator_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("DM channel not found")));
    };

    if requester_id != creator_id && requester_id != user_id {
        return Err(Error::Other(anyhow::anyhow!(
            "only the channel creator or the user themselves can remove a member"
        )));
    }

    conn.execute(
        "DELETE FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![dm_channel_id.clone(), user_id],
    ).await?;

    // Reconcile removes the member's leaves from the MLS tree (was a security gap).
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state.inner(), &dm_channel_id, &requester_id,
    ).await {
        eprintln!("[mls] remove_user_from_dm_channel: reconcile: {e}");
    }

    Ok(())
}

#[tauri::command]
pub async fn leave_dm_channel(
    dm_channel_id: String,
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    conn.execute(
        "DELETE FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![dm_channel_id.clone(), user_id.clone()],
    ).await?;

    // Wipe local MLS state so the leaver can't decrypt future messages.
    match crate::commands::mls::forget_local_mls_group(state.inner(), &dm_channel_id).await {
        Ok(()) => {}
        Err(e) => eprintln!("[mls] leave_dm_channel: forget local group {dm_channel_id}: {e}"),
    }

    // Signal remaining members to reconcile (removes the leaver's stale leaf).
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        &state.config,
        &dm_channel_id,
        serde_json::json!({"type": "membership_changed", "conversation_id": dm_channel_id}),
    ).await {
        eprintln!("[realtime] leave_dm_channel: notify room {dm_channel_id}: {e}");
    }

    // If no members remain, clean up the channel and all associated data
    let mut rows = conn.query(
        "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = ?1",
        libsql::params![dm_channel_id.clone()],
    ).await?;

    let remaining: i64 = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        0
    };

    if remaining == 0 {
        conn.execute("DELETE FROM message_envelope WHERE conversation_id = ?1", libsql::params![dm_channel_id.clone()]).await?;
        conn.execute("DELETE FROM dm_channel WHERE id = ?1", libsql::params![dm_channel_id]).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    const REMOTE_V001: &str = include_str!("../db/migrations/remote_schema.sql");

    const EXTRA_TABLES: &str = "
        CREATE TABLE IF NOT EXISTS conversation_watermark (
            conversation_id TEXT NOT NULL,
            user_id         TEXT NOT NULL,
            last_fetched_at TEXT NOT NULL,
            PRIMARY KEY (conversation_id, user_id)
        );
    ";

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(REMOTE_V001).unwrap();
        conn.execute_batch(EXTRA_TABLES).unwrap();
        conn
    }

    fn setup(conn: &Connection) {
        conn.execute("INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('bob',   'bob@x.com',   'bob')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('carol', 'carol@x.com', 'carol')", []).unwrap();
    }

    fn create_dm(conn: &Connection, id: &str, creator: &str, members: &[&str]) {
        let now = "2024-01-01T00:00:00Z";
        conn.execute(
            "INSERT INTO dm_channel (id, created_by, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, creator, now],
        ).unwrap();

        // Add creator
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, creator, creator, now],
        ).unwrap();

        // Add other members
        for member in members {
            if *member == creator {
                continue;
            }
            conn.execute(
                "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, member, creator, now],
            ).unwrap();
        }
    }

    // ── DM channel creation ────────────────────────────────────────────────

    #[test]
    fn create_dm_channel_with_two_members() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn create_dm_channel_creator_is_member() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        let exists: bool = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1' AND user_id = 'alice'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        ).unwrap();
        assert!(exists, "creator should be a member");
    }

    #[test]
    fn create_dm_channel_duplicate_member_ignored() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        // Try adding bob again
        conn.execute(
            "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at) VALUES ('dm1', 'bob', 'alice', '2024-01-02T00:00:00Z')",
            [],
        ).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1' AND user_id = 'bob'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1, "should still be one membership row");
    }

    // ── list_dm_channels ───────────────────────────────────────────────────

    #[test]
    fn list_dm_channels_only_returns_user_dms() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);
        create_dm(&conn, "dm2", "alice", &["alice", "carol"]);
        create_dm(&conn, "dm3", "bob", &["bob", "carol"]); // carol-bob only

        let alice_dms: Vec<String> = conn.prepare(
            "SELECT dc.id FROM dm_channel dc JOIN dm_channel_member dcm ON dcm.dm_channel_id = dc.id WHERE dcm.user_id = ?1",
        ).unwrap().query_map(
            rusqlite::params!["alice"],
            |row| row.get(0),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(alice_dms.len(), 2);
        assert!(alice_dms.contains(&"dm1".to_string()));
        assert!(alice_dms.contains(&"dm2".to_string()));
        assert!(!alice_dms.contains(&"dm3".to_string()));
    }

    // ── fetch_dm_members ───────────────────────────────────────────────────

    #[test]
    fn fetch_dm_members_includes_username() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        let members: Vec<(String, Option<String>)> = conn.prepare(
            "SELECT dcm.user_id, u.username
             FROM dm_channel_member dcm
             LEFT JOIN users u ON u.id = dcm.user_id
             WHERE dcm.dm_channel_id = ?1",
        ).unwrap().query_map(
            rusqlite::params!["dm1"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(members.len(), 2);
        assert!(members.contains(&("alice".into(), Some("alice".into()))));
        assert!(members.contains(&("bob".into(), Some("bob".into()))));
    }

    // ── get_dm_channel ─────────────────────────────────────────────────────

    #[test]
    fn get_dm_channel_returns_channel() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        let (id, created_by): (String, String) = conn.query_row(
            "SELECT id, created_by FROM dm_channel WHERE id = ?1",
            rusqlite::params!["dm1"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();

        assert_eq!(id, "dm1");
        assert_eq!(created_by, "alice");
    }

    #[test]
    fn get_dm_channel_not_found() {
        let conn = db();
        setup(&conn);

        let result = conn.query_row(
            "SELECT id FROM dm_channel WHERE id = 'nonexistent'",
            [],
            |row| row.get::<_, String>(0),
        );
        assert!(result.is_err());
    }

    // ── remove_user_from_dm_channel (authorization) ────────────────────────

    #[test]
    fn remove_member_by_creator_succeeds() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        // alice (creator) removes bob
        let creator: String = conn.query_row(
            "SELECT created_by FROM dm_channel WHERE id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(creator, "alice");

        conn.execute(
            "DELETE FROM dm_channel_member WHERE dm_channel_id = 'dm1' AND user_id = 'bob'",
            [],
        ).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1' AND user_id = 'bob'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0);
    }

    // ── leave_dm_channel: auto-cleanup ─────────────────────────────────────

    #[test]
    fn leave_dm_last_member_cleans_up_channel() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        // Add a message to verify cleanup
        conn.execute(
            "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m1', 'dm1', 'alice', 'hello', '2024-01-01T10:00:00Z')",
            [],
        ).unwrap();

        // Both leave
        conn.execute("DELETE FROM dm_channel_member WHERE dm_channel_id = 'dm1' AND user_id = 'alice'", []).unwrap();
        conn.execute("DELETE FROM dm_channel_member WHERE dm_channel_id = 'dm1' AND user_id = 'bob'", []).unwrap();

        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(remaining, 0);

        // Simulate cleanup logic from leave_dm_channel
        conn.execute("DELETE FROM message_envelope WHERE conversation_id = 'dm1'", []).unwrap();
        conn.execute("DELETE FROM dm_channel WHERE id = 'dm1'", []).unwrap();

        let channel_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel WHERE id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(channel_exists, 0, "channel should be deleted");

        let msg_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(msg_count, 0, "messages should be cleaned up");
    }

    #[test]
    fn leave_dm_not_last_member_keeps_channel() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        conn.execute("DELETE FROM dm_channel_member WHERE dm_channel_id = 'dm1' AND user_id = 'alice'", []).unwrap();

        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(remaining, 1, "bob is still a member");

        let channel_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel WHERE id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(channel_exists, 1, "channel should still exist");
    }

    // ── DM cascade deletes ─────────────────────────────────────────────────

    #[test]
    fn dm_channel_delete_cascades_members() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        conn.execute("DELETE FROM dm_channel WHERE id = 'dm1'", []).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "members should be cascade-deleted");
    }

    #[test]
    fn user_delete_cascades_dm_membership() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        conn.execute("DELETE FROM users WHERE id = 'bob'", []).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE user_id = 'bob'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "bob's membership should be cascade-deleted");
    }

    // ── watermark initialization ───────────────────────────────────────────

    #[test]
    fn watermark_initialized_for_dm_members() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        // Simulate watermark init
        for uid in ["alice", "bob"] {
            conn.execute(
                "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, last_fetched_at) VALUES (?1, ?2, datetime('now'))",
                rusqlite::params!["dm1", uid],
            ).unwrap();
        }

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conversation_watermark WHERE conversation_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn watermark_idempotent() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        // Insert twice — INSERT OR IGNORE should not error or duplicate
        for _ in 0..2 {
            conn.execute(
                "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, last_fetched_at) VALUES ('dm1', 'alice', datetime('now'))",
                [],
            ).unwrap();
        }

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conversation_watermark WHERE conversation_id = 'dm1' AND user_id = 'alice'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1);
    }

    // ── group DM (3+ members) ──────────────────────────────────────────────

    #[test]
    fn group_dm_three_members() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob", "carol"]);

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn add_user_to_existing_dm() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        // Add carol to existing DM
        conn.execute(
            "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at) VALUES ('dm1', 'carol', 'alice', '2024-01-02T00:00:00Z')",
            [],
        ).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 3);
    }
}
