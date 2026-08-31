use serde::{Deserialize, Serialize};

use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

/// Generic error string returned whenever a send is suppressed because
/// of a block — same phrasing whether the recipient has actively
/// blocked the sender or simply hasn't accepted yet. Keeping this
/// deliberately uninformative prevents the sender from inferring their
/// block status.
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

/// Wire → domain for one DM channel. One conversion, so the create, list and
/// fetch paths cannot map the same row differently.
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
    // Require at least one other participant.
    let has_other = member_ids.iter().any(|id| id != &creator_id);
    if !has_other {
        return Err(Error::Other(anyhow::anyhow!("cannot create a DM with only yourself")));
    }

    // The block check and the 2-person dedupe both moved into the write (#987).
    //
    // Dedupe especially: run on the client it was two reads AND a race — two
    // devices creating the same DM at once each saw no existing pair and each
    // created one, splitting the pair's messages across two conversations.
    // Deciding it inside the write makes "one DM per pair" a property of the
    // transaction rather than of the client's timing. Multi-party DMs (3+
    // members) still skip dedupe and always create fresh.

    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    // DS seam: route the channel-creation writes (dm_channel + every membership
    // row + per-member watermark seeds) through the Delivery Service. The DS
    // performs all writes in one transaction, re-checks the block relationships,
    // dedupes a 2-person pair, and answers with the DM either way.
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

    // An existing DM already has its MLS group; re-initialising would delete and
    // rebuild one every member is already in.
    if !existing {
        // Initialise the MLS group for this DM (creator becomes the sole member).
        // Reconcile then adds all members' devices (including creator's other
        // devices).
        match crate::commands::mls::init_mls_group(state, &id, &creator_id).await {
            Ok(()) => {
                if let Err(e) =
                    crate::commands::mls::reconcile_group_mls_impl(state, &id, &creator_id).await
                {
                    eprintln!("[mls] create_dm_channel: reconcile failed: {e}");
                }
            }
            Err(e) => eprintln!("[mls] create_dm_channel: mls group init failed (non-fatal): {e}"),
        }
    }

    // TOFU-pin every peer's account_id_pub locally so a later Turso-side key
    // swap is detectable. Advisory — never blocks DM creation. Batched (#875):
    // one Turso query for the whole roster, not one per member.
    let peer_ids: Vec<String> = members
        .iter()
        .filter(|m| m.user_id != creator_id)
        .map(|m| m.user_id.clone())
        .collect();
    if let Err(e) = crate::commands::safety::batch_check_and_pin_account_keys(state, &peer_ids).await
    {
        eprintln!("[safety] create_dm_channel: batch pin failed: {e}");
    }

    // Notify non-creator members via their personal inbox rooms so they see
    // the new DM channel immediately without needing to refresh. Carry the
    // creator's username so the recipient's status-bar alert can name the
    // requester instead of a placeholder (issue #396).
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
            eprintln!("[inbox] create_dm_channel: notify {} failed: {e}", member.user_id);
        }
    }

    Ok(dm)
}

/// The caller's ACCEPTED DMs.
///
/// The block filter is deliberately ASYMMETRIC, and lives server-side now: it
/// hides channels whose other members the caller has blocked, but NOT channels
/// where the caller is the blocked party. A blocked user keeps seeing the
/// conversation, so the block produces no observable signal on their side —
/// their messages simply stay pending, indistinguishable from a recipient who
/// has not accepted.
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

    // Same reason the group tree caches its channels (#850): a local `message`
    // row cannot say whether its conversation is a channel or a DM, and a DM's
    // display name is the peer's username, which is only knowable here.
    cache_dms(state, &user_id, &dms).await;

    Ok(dms)
}

/// Mirror the DM list into `conversation_cache`, naming each one after the peer.
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

/// The caller's still-unaccepted DMs — the request list. Same asymmetric block
/// filter as [`list_dm_channels`], for the same reason.
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

    // DS seam: route the accept (flip the user's own accepted_at NULL → now)
    // through the Delivery Service. Server-side authz binds the accept to the
    // authenticated recipient's own membership row.
    let body = pollis_api::profile::AcceptDmBody {
        dm_channel_id,
        user_id,
        accepted_at: now,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// One DM and its members.
///
/// Membership-gated since #987: the DS answers `authorized: false` for a DM the
/// caller is not in, which surfaces here as the same "not found" the caller
/// already handled. Before, any user could read any DM's roster.
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
    // DS seam: route the membership INSERT + watermark seeds through the
    // Delivery Service. The DS performs both writes in one transaction and
    // re-derives the actor's DM membership server-side (a non-member cannot add
    // anyone).
    let now = chrono::Utc::now().to_rfc3339();
    let body = pollis_api::profile::AddDmMemberBody {
        dm_channel_id: dm_channel_id.clone(),
        user_id,
        added_by: added_by.clone(),
        added_at: now,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Reconcile adds the new member's devices to the MLS tree.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &dm_channel_id, &added_by,
    ).await {
        eprintln!("[mls] add_user_to_dm_channel: reconcile: {e}");
    }

    Ok(())
}

pub async fn remove_user_from_dm_channel(
    dm_channel_id: String,
    user_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // DS seam: route the membership DELETE through the Delivery Service. The
    // authz rule (only the channel creator or the user themselves may remove a
    // member) is enforced server-side.
    let body = pollis_api::profile::RemoveDmMemberBody {
        dm_channel_id: dm_channel_id.clone(),
        user_id,
        requester_id: requester_id.clone(),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Reconcile removes the member's leaves from the MLS tree (was a security gap).
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &dm_channel_id, &requester_id,
    ).await {
        eprintln!("[mls] remove_user_from_dm_channel: reconcile: {e}");
    }

    Ok(())
}

pub async fn leave_dm_channel(
    dm_channel_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // DS seam: route the membership DELETE and the empty-channel teardown
    // (envelopes + dm_channel) through the Delivery Service. The DS does the
    // member-delete + count + conditional teardown in ONE transaction so a DM
    // never lingers half-deleted; authz binds the leave to the actor's own
    // membership.
    let body = pollis_api::profile::LeaveDmBody {
        dm_channel_id: dm_channel_id.clone(),
        user_id,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Wipe local MLS state so the leaver can't decrypt future messages.
    match crate::commands::mls::forget_local_mls_group(state, &dm_channel_id).await {
        Ok(()) => {}
        Err(e) => eprintln!("[mls] leave_dm_channel: forget local group {dm_channel_id}: {e}"),
    }

    // Signal remaining members to reconcile (removes the leaver's stale leaf).
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        state,
        &dm_channel_id,
        serde_json::json!({"type": "membership_changed", "conversation_id": dm_channel_id}),
    ).await {
        eprintln!("[realtime] leave_dm_channel: notify room {dm_channel_id}: {e}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    /// The SHIPPED remote schema — baseline plus every numbered migration, in
    /// the order `scripts/db-apply.sh` applies them. #875: this used to be
    /// `BASELINE` plus a hand-written `EXTRA_TABLES` block standing in for the
    /// later migrations, which is how a fixture ends up laxer than production.
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
        // Registry claim first — 000017's guard triggers refuse a dm_channel
        // row the `conversation` registry did not grant (#948).
        conn.execute(
            "INSERT INTO conversation (id, kind) VALUES (?1, 'dm')",
            rusqlite::params![id],
        ).unwrap();
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

        conn.execute(
            "INSERT INTO user_device (device_id, user_id) VALUES
               ('alice-d1', 'alice'), ('alice-d2', 'alice'), ('bob-d1', 'bob')",
            [],
        ).unwrap();

        for uid in ["alice", "bob"] {
            conn.execute(
                "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at)
                 SELECT ?1, ?2, ud.device_id, datetime('now')
                 FROM user_device ud WHERE ud.user_id = ?2",
                rusqlite::params!["dm1", uid],
            ).unwrap();
        }

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conversation_watermark WHERE conversation_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 3, "2 alice devices + 1 bob device");
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

        // Insert twice — INSERT OR IGNORE should not error or duplicate
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

    // ── 2-person DM dedupe lookup ─────────────────────────────────────────

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

        // Lookup with creator/target in either order must find the same channel.
        assert_eq!(find_two_person_dm(&conn, "alice", "bob").as_deref(), Some("dm1"));
        assert_eq!(find_two_person_dm(&conn, "bob", "alice").as_deref(), Some("dm1"));

        // A 3-person channel involving the same pair must NOT match.
        create_dm(&conn, "dm2", "alice", &["alice", "bob", "carol"]);
        assert_eq!(find_two_person_dm(&conn, "alice", "bob").as_deref(), Some("dm1"),
            "2-person dm1 still matches; the 3-person dm2 must not");

        // A different 2-person pair must not collide.
        create_dm(&conn, "dm3", "alice", &["alice", "carol"]);
        assert_eq!(find_two_person_dm(&conn, "alice", "carol").as_deref(), Some("dm3"));
        assert_eq!(find_two_person_dm(&conn, "bob", "carol"), None);
    }

    #[test]
    fn dedupe_finds_channel_created_by_other_user() {
        // Duplicate-prevention case: bob created the pending request first;
        // when alice triggers create_dm_channel(alice, [bob]) the dedupe
        // lookup must find bob's existing channel regardless of who created it.
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "bob", &["bob", "alice"]);

        assert_eq!(find_two_person_dm(&conn, "alice", "bob").as_deref(), Some("dm1"));
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
