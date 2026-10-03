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

/// Returns true if `blocker_id` has blocked `blocked_id` OR vice versa.
///
/// Used by any command that sends traffic between two users
/// (create_dm_channel, send_message, send_group_invite) so either
/// side's block silently halts delivery.
pub async fn is_blocked_either_way(
    state: &Arc<AppState>,
    user_a: &str,
    user_b: &str,
) -> Result<bool> {
    any_blocked_either_way(state, user_a, std::slice::from_ref(&user_b.to_string())).await
}

/// Whether ANY of `others` is in a block relationship with `me`, in either
/// direction.
///
/// The batch form, because every caller has a set: a DM's other members, a
/// multi-party DM's proposed roster. Before #987 this was a query per candidate,
/// run inside a loop on the send path.
///
/// Direction-blind by construction — the DS answers which ids are blocked, never
/// which way — because every caller reports the same generic refusal so neither
/// side can infer who blocked whom. A helper that knew the direction would put
/// that inference one log line away from leaking.
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

    // DS seam: route the block write (insert block row + reset the blocker's
    // accepted_at in shared DMs) through the Delivery Service. Server-side authz
    // binds the block to the authenticated user's own list and runs both writes
    // in one transaction.
    let body = pollis_api::profile::AddBlock(pollis_api::profile::BlockBody {
        blocker_id,
        blocked_id,
    });
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Report an account (#1213), optionally pointing at one of their messages,
/// and optionally block them in the same step.
///
/// Signal-style: the report carries the reported account, a reason and the
/// conversation/message ids, never message text. The reporter is this device's
/// own user (the DS binds it to the signature anyway). `reason` is one of
/// `spam`, `harassment`, `illegal`, `other`; anything else is refused here
/// before it is sent, and by the DS and the table after.
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
    // DS seam: route the unblock (delete block row) through the Delivery
    // Service.
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

// The remaining tests pin schema constraints (PRIMARY KEY dedupe, ON DELETE
// CASCADE) against the shipped remote schema. Query-simulation tests for the
// client SQL #987 removed were deleted.
#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        // The SHIPPED schema: baseline plus every numbered migration, in the order
        // `scripts/db-apply.sh` applies them. #875 — applying the baseline alone left
        // the fixture on a pre-migration schema no deploy has run for months.
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
