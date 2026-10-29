use serde::{Deserialize, Serialize};
use std::sync::Arc;


use crate::error::Result;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockedUser {
    pub user_id: String,
    pub username: Option<String>,
    pub blocked_at: String,
}

/// True if either user has blocked the other; any traffic between two users is
/// halted by either side's block.
pub async fn is_blocked_either_way(
    state: &Arc<AppState>,
    user_a: &str,
    user_b: &str,
) -> Result<bool> {
    any_blocked_either_way(state, user_a, std::slice::from_ref(&user_b.to_string())).await
}

/// Whether any of `others` is in a block relationship with `me`, in either
/// direction.
///
/// Direction-blind by design: the DS answers which ids are blocked, never which
/// way, so callers can only report a generic refusal and neither side can infer
/// who blocked whom.
pub async fn any_blocked_either_way(
    state: &Arc<AppState>,
    _me: &str,
    others: &[String],
) -> Result<bool> {
    Ok(!crate::commands::ds_reads::blocked_among(state, others)
        .await?
        .is_empty())
}

pub async fn block_user(
    blocker_id: String,
    blocked_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    if blocker_id == blocked_id {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "cannot block yourself"
        )));
    }

    // The DS binds the block to the caller's own list and also resets the blocker's
    // `accepted_at` in shared DMs, in one transaction.
    let body = pollis_api::profile::AddBlock(pollis_api::profile::BlockBody {
        blocker_id,
        blocked_id,
    });
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Report an account (#1213), optionally citing one of their messages, and
/// optionally block them too. Carries ids and a reason, never message text.
/// `reason` is `spam`, `harassment`, `illegal` or `other`.
pub async fn report_user(
    reported_id: String,
    reason: String,
    conversation_id: Option<String>,
    message_id: Option<String>,
    also_block: bool,
    state: &Arc<AppState>,
) -> Result<()> {
    let reporter_id = crate::commands::mls::current_user_id(state).await?;
    if reporter_id == reported_id {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "cannot report yourself"
        )));
    }
    let reason: pollis_api::reports::ReportReason =
        serde_json::from_value(serde_json::Value::String(reason))
            .map_err(|_| crate::error::Error::Other(anyhow::anyhow!("unknown report reason")))?;
    let body = pollis_api::reports::ReportUserBody {
        reporter_id: reporter_id.clone(),
        reported_id: reported_id.clone(),
        reason,
        conversation_id,
        message_id,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;
    if also_block {
        block_user(reporter_id, reported_id, state).await?;
    }
    Ok(())
}

pub async fn unblock_user(
    blocker_id: String,
    blocked_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::profile::RemoveBlock(pollis_api::profile::BlockBody {
        blocker_id,
        blocked_id,
    });
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

pub async fn list_blocked_users(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<BlockedUser>> {
    Ok(crate::commands::ds_reads::bootstrap(state, &user_id)
        .await?
        .blocks
        .into_iter()
        .map(|b| BlockedUser {
            user_id: b.user_id,
            username: b.username,
            blocked_at: b.blocked_at,
        })
        .collect())
}

// Schema-constraint tests (PRIMARY KEY dedupe, ON DELETE CASCADE) against the
// shipped remote schema.
#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        // The shipped schema: baseline plus every numbered migration, in apply order.
        for sql in pollis_schema::main_scripts() {
            conn.execute_batch(sql).unwrap();
        }
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
