use serde::{Deserialize, Serialize};

use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

/// Returned whenever a send is suppressed by a block, worded the same as a
/// not-yet-accepted request so the sender can't infer they're blocked.
pub const BLOCK_ERR: &str = "message request pending";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmChannel {
    pub id: String,
    pub created_by: String,
    pub created_at: String,
    pub members: Vec<DmChannelMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmChannelMember {
    pub user_id: String,
    pub username: Option<String>,
    pub avatar_url: Option<String>,
    pub added_by: String,
    pub added_at: String,
    pub accepted_at: Option<String>,
}

fn dm_from_wire(d: pollis_api::directory::DmChannelWire) -> DmChannel {
    DmChannel {
        id: d.id,
        created_by: d.created_by,
        created_at: d.created_at,
        members: d
            .members
            .into_iter()
            .map(|m| DmChannelMember {
                user_id: m.user_id,
                username: m.username,
                avatar_url: m.avatar_url,
                added_by: m.added_by.unwrap_or_default(),
                added_at: m.added_at.unwrap_or_default(),
                accepted_at: m.accepted_at,
            })
            .collect(),
    }
}


pub async fn create_dm_channel(
    creator_id: String,
    member_ids: Vec<String>,
    state: &Arc<AppState>,
) -> Result<DmChannel> {
    let has_other = member_ids.iter().any(|id| id != &creator_id);
    if !has_other {
        return Err(Error::Other(anyhow::anyhow!("cannot create a DM with only yourself")));
    }

    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    // The DS checks blocks and dedupes a 2-person DM inside its write (#987), so
    // "one DM per pair" holds under concurrent creates; 3+ member DMs never dedupe.
    let body = pollis_api::profile::CreateDmBody {
        id: id.clone(),
        creator_id: creator_id.clone(),
        member_ids,
        created_at: now.clone(),
    };
    let (dm, existing) = match crate::commands::mls::ds_post_json(state, &body).await? {
        pollis_api::profile::CreatedDm::Blocked => {
            return Err(Error::Other(anyhow::anyhow!(BLOCK_ERR)));
        }
        pollis_api::profile::CreatedDm::Ok {
            id,
            created_by,
            created_at,
            members,
            existing,
        } => (
            DmChannel {
                id,
                created_by,
                created_at,
                members: members
                    .into_iter()
                    .map(|m| DmChannelMember {
                        user_id: m.user_id,
                        username: m.username,
                        avatar_url: m.avatar_url,
                        added_by: creator_id.clone(),
                        added_at: now.clone(),
                        accepted_at: m.accepted_at,
                    })
                    .collect(),
            },
            existing,
        ),
    };
    let id = dm.id.clone();
    let members = dm.members.clone();

    // An existing DM already has its MLS group; re-initialising would rebuild one
    // every member is already in.
    if !existing {
        match crate::commands::mls::init_mls_group(state, &id, &creator_id).await {
            Ok(()) => {
                if let Err(e) =
                    crate::commands::mls::reconcile_group_mls_impl(state, &id, &creator_id).await
                {
                    crate::diag!("[mls] create_dm_channel: reconcile failed: {e}");
                }
            }
            Err(e) => crate::diag!("[mls] create_dm_channel: mls group init failed (non-fatal): {e}"),
        }
    }

    // TOFU-pin every peer's account key so a later server-side swap is detectable.
    // Advisory; never blocks creation.
    let peer_ids: Vec<String> = members
        .iter()
        .filter(|m| m.user_id != creator_id)
        .map(|m| m.user_id.clone())
        .collect();
    if let Err(e) = crate::commands::safety::batch_check_and_pin_account_keys(state, &peer_ids).await
    {
        crate::diag!("[safety] create_dm_channel: batch pin failed: {e}");
    }

    // Wake the other members' inboxes, naming the creator for their alert (#396).
    let creator_username = members
        .iter()
        .find(|m| m.user_id == creator_id)
        .and_then(|m| m.username.clone());
    let inbox_payload = crate::commands::livekit_signalling::dm_created_inbox_payload(
        &id,
        creator_username.as_deref(),
    );
    for member in members.iter().filter(|m| m.user_id != creator_id) {
        if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
            state, &member.user_id, inbox_payload.clone(),
        ).await {
            crate::diag!("[inbox] create_dm_channel: notify {} failed: {e}", member.user_id);
        }
    }

    // Wake the creator's other devices too (#1410): the inbox is the only room that
    // reaches them, and they refetch DM lists and pull Welcomes on `dm_created`.
    // Sent after the reconcile so those Welcomes exist.
    notify_own_devices_of_dm(state, &creator_id, &id, "create_dm_channel").await;

    Ok(dm)
}

/// Wake every device of `user_id` about a DM change they made on one of them.
/// Best-effort: a device that misses it catches up on next launch.
async fn notify_own_devices_of_dm(
    state: &Arc<AppState>,
    user_id: &str,
    dm_channel_id: &str,
    what: &str,
) {
    if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
        state,
        user_id,
        crate::commands::livekit_signalling::dm_created_inbox_payload(dm_channel_id, None),
    )
    .await
    {
        crate::diag!("[inbox] {what}: notify own devices of {dm_channel_id} failed: {e}");
    }
}

/// The caller's accepted DMs.
///
/// The DS's block filter is deliberately asymmetric: it hides DMs with members
/// the caller blocked, not DMs where the caller is blocked, so a block gives the
/// blocked side no observable signal.
pub async fn list_dm_channels(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<DmChannel>> {
    let dms: Vec<DmChannel> = crate::commands::ds_reads::bootstrap(state, &user_id)
        .await?
        .dms
        .into_iter()
        .map(dm_from_wire)
        .collect();

    // Cached for the same reason as group channels (#850): a local `message` row
    // can't tell channel from DM, and a DM's name is the peer's username.
    cache_dms(state, &user_id, &dms).await;

    Ok(dms)
}

async fn cache_dms(state: &Arc<AppState>, user_id: &str, dms: &[DmChannel]) {
    let entries: Vec<crate::commands::messages::CachedConversation> = dms
        .iter()
        .map(|dm| crate::commands::messages::CachedConversation {
            id: dm.id.clone(),
            kind: "dm",
            name: dm
                .members
                .iter()
                .find(|m| m.user_id != user_id)
                .and_then(|m| m.username.clone()),
            group_id: None,
            group_name: None,
        })
        .collect();
    crate::commands::messages::cache_conversations(state, &entries).await;
}

/// The caller's unaccepted DMs (requests), with the same asymmetric block filter.
pub async fn list_dm_requests(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<DmChannel>> {
    Ok(crate::commands::ds_reads::bootstrap(state, &user_id)
        .await?
        .dm_requests
        .into_iter()
        .map(dm_from_wire)
        .collect())
}

pub async fn accept_dm_request(
    dm_channel_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    let body = pollis_api::profile::AcceptDmBody {
        dm_channel_id: dm_channel_id.clone(),
        user_id: user_id.clone(),
        accepted_at: now,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // #1410: the accepter's other devices still list this as a request until told.
    notify_own_devices_of_dm(state, &user_id, &dm_channel_id, "accept_dm_request").await;

    // Catch up and run the re-add backstop now, so the accepter's other devices a
    // creator's add missed join the tree before they next open the DM (#1571).
    // Best-effort: the accept has landed. Boxed: large futures.
    if let Err(e) = Box::pin(crate::commands::messages::catch_up_mls_group_interleaved(
        state,
        &dm_channel_id,
        &user_id,
    ))
    .await
    {
        crate::diag!("[mls] accept_dm_request: catch-up for {dm_channel_id}: {e}");
    }
    if let Err(e) =
        Box::pin(crate::commands::mls::reconcile_backstop_one(state, &dm_channel_id, &user_id))
            .await
    {
        crate::diag!("[mls] accept_dm_request: re-add backstop for {dm_channel_id}: {e}");
    }

    Ok(())
}

/// One DM and its members. Membership-gated: a DM the caller is not in reads as
/// not found.
pub async fn get_dm_channel(
    dm_channel_id: String,
    state: &Arc<AppState>,
) -> Result<DmChannel> {
    let resolved = crate::commands::ds_reads::catch_up(state, &dm_channel_id, false, true).await?;
    match resolved.dm {
        Some(dm) => Ok(dm_from_wire(dm)),
        None => Err(Error::Other(anyhow::anyhow!(
            "DM channel not found: {dm_channel_id}"
        ))),
    }
}

pub async fn add_user_to_dm_channel(
    dm_channel_id: String,
    user_id: String,
    added_by: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // The DS writes membership and watermark seeds in one transaction and checks
    // that the actor is a member.
    let now = chrono::Utc::now().to_rfc3339();
    let body = pollis_api::profile::AddDmMemberBody {
        dm_channel_id: dm_channel_id.clone(),
        user_id,
        added_by: added_by.clone(),
        added_at: now,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &dm_channel_id, &added_by,
    ).await {
        crate::diag!("[mls] add_user_to_dm_channel: reconcile: {e}");
    }

    Ok(())
}

pub async fn remove_user_from_dm_channel(
    dm_channel_id: String,
    user_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // Only the creator or the user themselves may remove a member (DS-enforced).
    let body = pollis_api::profile::RemoveDmMemberBody {
        dm_channel_id: dm_channel_id.clone(),
        user_id,
        requester_id: requester_id.clone(),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &dm_channel_id, &requester_id,
    ).await {
        crate::diag!("[mls] remove_user_from_dm_channel: reconcile: {e}");
    }

    Ok(())
}

pub async fn leave_dm_channel(
    dm_channel_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // The DS also tears down an emptied DM, in the same transaction.
    let body = pollis_api::profile::LeaveDmBody {
        dm_channel_id: dm_channel_id.clone(),
        user_id,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    match crate::commands::mls::forget_local_mls_group(state, &dm_channel_id).await {
        Ok(()) => {}
        Err(e) => crate::diag!("[mls] leave_dm_channel: forget local group {dm_channel_id}: {e}"),
    }

    // Remaining members must stage the leaver's remove; `kind: "leave"` (#1081)
    // makes them.
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        state,
        &dm_channel_id,
        crate::commands::livekit_signalling::member_left_conversation_payload(&dm_channel_id),
    ).await {
        crate::diag!("[realtime] leave_dm_channel: notify room {dm_channel_id}: {e}");
    }

    Ok(())
}

// Schema-constraint tests against the shipped remote schema, plus the 2-person
// DM dedupe lookup whose SQL lives in `pollis-delivery/src/profile.rs`.
#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    /// The shipped remote schema: baseline plus every numbered migration.
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        for sql in pollis_schema::main_scripts() {
            conn.execute_batch(sql).unwrap();
        }
        conn
    }

    fn setup(conn: &Connection) {
        conn.execute("INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('bob',   'bob@x.com',   'bob')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('carol', 'carol@x.com', 'carol')", []).unwrap();
    }

    fn create_dm(conn: &Connection, id: &str, creator: &str, members: &[&str]) {
        let now = "2024-01-01T00:00:00Z";
        // The `conversation` registry row must exist first (000017 triggers, #948).
        conn.execute(
            "INSERT INTO conversation (id, kind) VALUES (?1, 'dm')",
            rusqlite::params![id],
        ).unwrap();
        conn.execute(
            "INSERT INTO dm_channel (id, created_by, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, creator, now],
        ).unwrap();

        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, creator, creator, now],
        ).unwrap();

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

    #[test]
    fn create_dm_channel_duplicate_member_ignored() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

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

    #[test]
    fn watermark_idempotent() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        conn.execute(
            "INSERT INTO user_device (device_id, user_id) VALUES ('alice-d1', 'alice')",
            [],
        ).unwrap();

        for _ in 0..2 {
            conn.execute(
                "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at)
                 SELECT 'dm1', 'alice', ud.device_id, datetime('now')
                 FROM user_device ud WHERE ud.user_id = 'alice'",
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

    fn find_two_person_dm(conn: &Connection, a: &str, b: &str) -> Option<String> {
        conn.query_row(
            "SELECT dm_channel_id
             FROM dm_channel_member
             GROUP BY dm_channel_id
             HAVING COUNT(*) = 2
                AND SUM(CASE WHEN user_id = ?1 THEN 1 ELSE 0 END) = 1
                AND SUM(CASE WHEN user_id = ?2 THEN 1 ELSE 0 END) = 1
             LIMIT 1",
            rusqlite::params![a, b],
            |row| row.get::<_, String>(0),
        ).ok()
    }

    #[test]
    fn create_dm_channel_returns_existing_for_same_pair() {
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "alice", &["alice", "bob"]);

        assert_eq!(find_two_person_dm(&conn, "alice", "bob").as_deref(), Some("dm1"));
        assert_eq!(find_two_person_dm(&conn, "bob", "alice").as_deref(), Some("dm1"));

        create_dm(&conn, "dm2", "alice", &["alice", "bob", "carol"]);
        assert_eq!(find_two_person_dm(&conn, "alice", "bob").as_deref(), Some("dm1"),
            "2-person dm1 still matches; the 3-person dm2 must not");

        create_dm(&conn, "dm3", "alice", &["alice", "carol"]);
        assert_eq!(find_two_person_dm(&conn, "alice", "carol").as_deref(), Some("dm3"));
        assert_eq!(find_two_person_dm(&conn, "bob", "carol"), None);
    }

    #[test]
    fn dedupe_finds_channel_created_by_other_user() {
        // Bob created the pending request first; the lookup must still find it.
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "bob", &["bob", "alice"]);

        assert_eq!(find_two_person_dm(&conn, "alice", "bob").as_deref(), Some("dm1"));
    }
}
