use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;

use crate::error::Result;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockedUser {
    pub user_id: String,
    pub username: Option<String>,
    pub blocked_at: String,
}

/// Returns true if `blocker_id` has blocked `blocked_id` OR vice versa.
///
/// Used by any command that sends traffic between two users
/// (create_dm_channel, send_message, send_group_invite) so either
/// side's block silently halts delivery.
pub async fn is_blocked_either_way(
    conn: &libsql::Connection,
    user_a: &str,
    user_b: &str,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM user_block
             WHERE (blocker_id = ?1 AND blocked_id = ?2)
                OR (blocker_id = ?2 AND blocked_id = ?1)
             LIMIT 1",
            libsql::params![user_a, user_b],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

#[tauri::command]
pub async fn block_user(
    blocker_id: String,
    blocked_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    if blocker_id == blocked_id {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "cannot block yourself"
        )));
    }

    let conn = state.remote_db.conn().await?;

    // Insert the block row. PK conflict means the block already
    // exists, which is fine — idempotent.
    conn.execute(
        "INSERT OR IGNORE INTO user_block (blocker_id, blocked_id)
         VALUES (?1, ?2)",
        libsql::params![blocker_id.clone(), blocked_id.clone()],
    )
    .await?;

    // Reset accepted_at to NULL for the blocker's membership in every
    // DM channel shared with the blocked user. If the block is later
    // released, the channel resurfaces as a request rather than in
    // the regular DM list.
    conn.execute(
        "UPDATE dm_channel_member
         SET accepted_at = NULL
         WHERE user_id = ?1
           AND dm_channel_id IN (
             SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?2
           )",
        libsql::params![blocker_id, blocked_id],
    )
    .await?;

    Ok(())
}

#[tauri::command]
pub async fn unblock_user(
    blocker_id: String,
    blocked_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    conn.execute(
        "DELETE FROM user_block WHERE blocker_id = ?1 AND blocked_id = ?2",
        libsql::params![blocker_id, blocked_id],
    )
    .await?;

    Ok(())
}

#[tauri::command]
pub async fn list_blocked_users(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<BlockedUser>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn
        .query(
            "SELECT ub.blocked_id, u.username, ub.created_at
             FROM user_block ub
             LEFT JOIN users u ON u.id = ub.blocked_id
             WHERE ub.blocker_id = ?1
             ORDER BY ub.created_at DESC",
            libsql::params![user_id],
        )
        .await?;

    let mut blocked = Vec::new();
    while let Some(row) = rows.next().await? {
        blocked.push(BlockedUser {
            user_id: row.get(0)?,
            username: row.get(1)?,
            blocked_at: row.get(2)?,
        });
    }

    Ok(blocked)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    const REMOTE_V001: &str = include_str!("../db/migrations/remote_schema.sql");
    const MIGRATION_15: &str =
        include_str!("../db/migrations/000015_dm_requests_and_blocks.sql");

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(REMOTE_V001).unwrap();
        // remote_schema.sql is frozen and does not ship the
        // schema_migrations tracking table (that's created by
        // migration 000001 in prod). Create it here so the tail
        // INSERT inside MIGRATION_15 can run.
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                 version     INTEGER PRIMARY KEY,
                 description TEXT NOT NULL,
                 applied_at  TEXT NOT NULL DEFAULT (datetime('now'))
             );",
        )
        .unwrap();
        // Applying MIGRATION_15 adds accepted_at + user_block and
        // doubles as a smoke test that the migration parses.
        conn.execute_batch(MIGRATION_15).unwrap();
        conn
    }

    fn seed_users(conn: &Connection) {
        conn.execute(
            "INSERT INTO users (id, email, username) VALUES ('alice', 'a@x.com', 'alice')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (id, email, username) VALUES ('bob', 'b@x.com', 'bob')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (id, email, username) VALUES ('carol', 'c@x.com', 'carol')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn block_inserts_row() {
        let conn = db();
        seed_users(&conn);
        conn.execute(
            "INSERT OR IGNORE INTO user_block (blocker_id, blocked_id) VALUES ('alice', 'bob')",
            [],
        )
        .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_block WHERE blocker_id = 'alice' AND blocked_id = 'bob'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn block_is_idempotent() {
        let conn = db();
        seed_users(&conn);
        for _ in 0..3 {
            conn.execute(
                "INSERT OR IGNORE INTO user_block (blocker_id, blocked_id) VALUES ('alice', 'bob')",
                [],
            )
            .unwrap();
        }
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_block", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn block_is_directional() {
        let conn = db();
        seed_users(&conn);
        conn.execute(
            "INSERT INTO user_block (blocker_id, blocked_id) VALUES ('alice', 'bob')",
            [],
        )
        .unwrap();

        // alice blocks bob — does NOT imply bob blocks alice
        let a_blocks_b: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_block WHERE blocker_id = 'alice' AND blocked_id = 'bob'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let b_blocks_a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_block WHERE blocker_id = 'bob' AND blocked_id = 'alice'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(a_blocks_b, 1);
        assert_eq!(b_blocks_a, 0);
    }

    #[test]
    fn is_blocked_either_way_matches_both_directions() {
        let conn = db();
        seed_users(&conn);
        conn.execute(
            "INSERT INTO user_block (blocker_id, blocked_id) VALUES ('alice', 'bob')",
            [],
        )
        .unwrap();

        // Either direction of the query should find the block.
        let ab: i64 = conn.query_row(
            "SELECT COUNT(*) FROM user_block
             WHERE (blocker_id = 'alice' AND blocked_id = 'bob')
                OR (blocker_id = 'bob'   AND blocked_id = 'alice')",
            [],
            |row| row.get(0),
        ).unwrap();
        let ba: i64 = conn.query_row(
            "SELECT COUNT(*) FROM user_block
             WHERE (blocker_id = 'bob'   AND blocked_id = 'alice')
                OR (blocker_id = 'alice' AND blocked_id = 'bob')",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(ab, 1);
        assert_eq!(ba, 1);
    }

    #[test]
    fn unblock_removes_row() {
        let conn = db();
        seed_users(&conn);
        conn.execute(
            "INSERT INTO user_block (blocker_id, blocked_id) VALUES ('alice', 'bob')",
            [],
        )
        .unwrap();

        conn.execute(
            "DELETE FROM user_block WHERE blocker_id = 'alice' AND blocked_id = 'bob'",
            [],
        )
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_block", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn block_resets_accepted_at_in_shared_dms() {
        let conn = db();
        seed_users(&conn);

        // Set up a DM between alice and bob — both accepted.
        conn.execute(
            "INSERT INTO dm_channel (id, created_by, created_at) VALUES ('dm1', 'alice', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at, accepted_at)
             VALUES ('dm1', 'alice', 'alice', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at, accepted_at)
             VALUES ('dm1', 'bob',   'alice', '2024-01-01T00:00:00Z', '2024-01-02T00:00:00Z')",
            [],
        )
        .unwrap();

        // alice blocks bob → simulate block_user's UPDATE on accepted_at.
        conn.execute(
            "INSERT INTO user_block (blocker_id, blocked_id) VALUES ('alice', 'bob')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE dm_channel_member
             SET accepted_at = NULL
             WHERE user_id = 'alice'
               AND dm_channel_id IN (
                 SELECT dm_channel_id FROM dm_channel_member WHERE user_id = 'bob'
               )",
            [],
        )
        .unwrap();

        // Alice's row reset; Bob's row unchanged.
        let alice_accepted: Option<String> = conn.query_row(
            "SELECT accepted_at FROM dm_channel_member WHERE dm_channel_id = 'dm1' AND user_id = 'alice'",
            [],
            |row| row.get(0),
        ).unwrap();
        let bob_accepted: Option<String> = conn.query_row(
            "SELECT accepted_at FROM dm_channel_member WHERE dm_channel_id = 'dm1' AND user_id = 'bob'",
            [],
            |row| row.get(0),
        ).unwrap();

        assert!(alice_accepted.is_none(), "blocker's accepted_at reset to NULL");
        assert_eq!(bob_accepted.as_deref(), Some("2024-01-02T00:00:00Z"), "other party untouched");
    }

    #[test]
    fn user_delete_cascades_blocks() {
        let conn = db();
        seed_users(&conn);
        conn.execute(
            "INSERT INTO user_block (blocker_id, blocked_id) VALUES ('alice', 'bob')",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM users WHERE id = 'bob'", [])
            .unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_block", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "block row should cascade when user is deleted");
    }
}
