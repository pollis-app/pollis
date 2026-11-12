use serde::{Deserialize, Serialize};

use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

/// Generic error for a block-suppressed send; deliberately uninformative so a block can't be inferred.
pub const BLOCK_ERR: &str = "message request pending";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmChannel {
    pub id: String,
    pub created_by: String,
    pub created_at: String,
    /// The newest local message's `sent_at`, else `created_at` (#1838): the list's sort key.
    pub last_activity_at: String,
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
        last_activity_at: d.created_at.clone(),
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

    // Block check and 2-person dedupe run inside the DS write (#987), so concurrent creates can't split a pair.

    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();

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
                last_activity_at: created_at.clone(),
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
        // Creator becomes the sole member; reconcile adds everyone's devices.
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

    // TOFU-pin every peer's account_id_pub so a later key swap is detectable. Advisory.
    let peer_ids: Vec<String> = members
        .iter()
        .filter(|m| m.user_id != creator_id)
        .map(|m| m.user_id.clone())
        .collect();
    if let Err(e) = crate::commands::safety::batch_check_and_pin_account_keys(state, &peer_ids).await
    {
        crate::diag!("[safety] create_dm_channel: batch pin failed: {e}");
    }

    // Notify other members via their inbox; the username lets their alert name the requester (#396).
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

    // Wake the creator's other devices (#1410): the inbox is the only room that reaches them.
    notify_own_devices_of_dm(state, &creator_id, &id, "create_dm_channel").await;

    Ok(dm)
}

/// Best-effort wake of every device of `user_id` about a change to DM `dm_channel_id`.
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

/// The caller's ACCEPTED DMs.
///
/// The DS block filter is deliberately asymmetric: a blocked user still sees the DM, so a block is unobservable.
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
    let dms = order_by_local_activity(state, dms).await;

    // Cache DM names for search (#850); the peer's username is only knowable here.
    cache_dms(state, &user_id, &dms).await;

    Ok(dms)
}

/// Newest local `sent_at` per conversation in `ids` (#1838), sent or received, any row kind.
/// Conversations with no local messages are absent.
fn latest_sent_at_on(
    conn: &rusqlite::Connection,
    ids: &[String],
) -> Result<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::with_capacity(ids.len());
    for chunk in crate::db::chunk::bind_chunks(ids, 0) {
        let sql = format!(
            "SELECT conversation_id, MAX(sent_at) FROM message
             WHERE conversation_id IN ({}) GROUP BY conversation_id",
            crate::db::chunk::placeholders(chunk.len(), 1)
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, at) = row?;
            out.insert(id, at);
        }
    }
    Ok(out)
}

/// A stored timestamp as an instant: RFC 3339, or SQLite's `datetime()` form read as UTC.
fn parse_instant(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(d.to_utc());
    }
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
        .ok()
        .map(|d| d.and_utc())
}

/// Stamp each DM's `last_activity_at` from `latest` and sort newest first (#1838).
///
/// A DM with no messages sorts by its creation time; ties (and unparseable stamps, last)
/// break by id descending, so the order is total and rows never swap between reads.
fn sort_by_activity(
    mut dms: Vec<DmChannel>,
    latest: &std::collections::HashMap<String, String>,
) -> Vec<DmChannel> {
    for dm in &mut dms {
        if let Some(at) = latest.get(&dm.id) {
            dm.last_activity_at = at.clone();
        }
    }
    dms.sort_by(|a, b| {
        parse_instant(&b.last_activity_at)
            .cmp(&parse_instant(&a.last_activity_at))
            .then_with(|| b.id.cmp(&a.id))
    });
    dms
}

/// [`sort_by_activity`] over this device's local messages. With no local DB open the
/// DMs still list, ordered by creation time.
async fn order_by_local_activity(state: &Arc<AppState>, dms: Vec<DmChannel>) -> Vec<DmChannel> {
    let ids: Vec<String> = dms.iter().map(|d| d.id.clone()).collect();
    let latest = {
        let guard = state.local_db.lock().await;
        match guard.as_ref().map(|db| latest_sent_at_on(db.conn(), &ids)) {
            Some(Ok(latest)) => latest,
            Some(Err(e)) => {
                crate::diag!("[dm] activity order: local read failed: {e}");
                Default::default()
            }
            None => Default::default(),
        }
    };
    sort_by_activity(dms, &latest)
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

    let body = pollis_api::profile::AcceptDmBody {
        dm_channel_id: dm_channel_id.clone(),
        user_id: user_id.clone(),
        accepted_at: now,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // #1410: the accepter's other devices still list this as a request until woken.
    notify_own_devices_of_dm(state, &user_id, &dm_channel_id, "accept_dm_request").await;

    // #1571: catch up and run the re-add backstop so the accepter's other devices join now. Best-effort.
    // Both boxed: large futures, kept off the caller's stack.
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

/// One DM and its members.
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

pub async fn leave_dm_channel(
    dm_channel_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // The DS deletes the member and tears down an empty DM in one transaction.
    let body = pollis_api::profile::LeaveDmBody {
        dm_channel_id: dm_channel_id.clone(),
        user_id,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Wipe local MLS state so the leaver can't decrypt future messages.
    match crate::commands::mls::leave_local_mls_group(state, &dm_channel_id).await {
        Ok(()) => {}
        Err(e) => crate::diag!("[mls] leave_dm_channel: forget local group {dm_channel_id}: {e}"),
    }

    // `kind: "leave"` makes remaining members remove the leaver's leaf (#1081).
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        state,
        &dm_channel_id,
        crate::commands::livekit_signalling::member_left_conversation_payload(&dm_channel_id),
    ).await {
        crate::diag!("[realtime] leave_dm_channel: notify room {dm_channel_id}: {e}");
    }

    Ok(())
}

#[cfg(test)]
mod activity_order_tests {
    // #1838: the DM list's order, against the local schema.
    use super::{latest_sent_at_on, sort_by_activity, DmChannel};
    use rusqlite::Connection;

    fn local_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::local::apply_local_schema(&conn).unwrap();
        conn
    }

    fn message(conn: &Connection, id: &str, conversation_id: &str, sender: &str, sent_at: &str) {
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, received_at)
             VALUES (?1, ?2, ?3, X'00', 'hi', ?4, ?4)",
            rusqlite::params![id, conversation_id, sender, sent_at],
        )
        .unwrap();
    }

    fn dm(id: &str, created_at: &str) -> DmChannel {
        DmChannel {
            id: id.to_string(),
            created_by: "me".to_string(),
            created_at: created_at.to_string(),
            last_activity_at: created_at.to_string(),
            members: Vec::new(),
        }
    }

    fn order(conn: &Connection, dms: Vec<DmChannel>) -> Vec<String> {
        let ids: Vec<String> = dms.iter().map(|d| d.id.clone()).collect();
        let latest = latest_sent_at_on(conn, &ids).unwrap();
        sort_by_activity(dms, &latest).into_iter().map(|d| d.id).collect()
    }

    #[test]
    fn newest_message_sent_or_received_sorts_first() {
        let conn = local_db();
        message(&conn, "m1", "dm-old", "peer", "2026-01-01T10:00:00+00:00");
        // Our own reply is the newest activity anywhere.
        message(&conn, "m2", "dm-old", "me", "2026-03-01T10:00:00+00:00");
        message(&conn, "m3", "dm-mid", "peer", "2026-02-01T10:00:00+00:00");
        message(&conn, "m4", "dm-new", "peer", "2026-01-15T10:00:00+00:00");

        let got = order(
            &conn,
            vec![
                dm("dm-new", "2025-12-03T00:00:00+00:00"),
                dm("dm-mid", "2025-12-02T00:00:00+00:00"),
                dm("dm-old", "2025-12-01T00:00:00+00:00"),
            ],
        );
        assert_eq!(got, ["dm-old", "dm-mid", "dm-new"]);
    }

    #[test]
    fn a_dm_with_no_messages_sorts_by_its_creation_time() {
        let conn = local_db();
        message(&conn, "m1", "dm-chatty", "peer", "2026-01-10T00:00:00+00:00");

        let got = order(
            &conn,
            vec![
                dm("dm-chatty", "2025-01-01T00:00:00+00:00"),
                dm("dm-empty-new", "2026-02-01T00:00:00Z"),
                dm("dm-empty-old", "2025-06-01T00:00:00Z"),
            ],
        );
        assert_eq!(got, ["dm-empty-new", "dm-chatty", "dm-empty-old"]);
    }

    #[test]
    fn the_stamp_carries_the_newest_message_time() {
        let conn = local_db();
        message(&conn, "m1", "dm-a", "peer", "2026-01-10T00:00:00+00:00");
        message(&conn, "m2", "dm-a", "peer", "2026-01-11T00:00:00+00:00");
        let ids = vec!["dm-a".to_string(), "dm-b".to_string()];
        let latest = latest_sent_at_on(&conn, &ids).unwrap();
        let out = sort_by_activity(
            vec![dm("dm-a", "2025-01-01T00:00:00Z"), dm("dm-b", "2025-01-02T00:00:00Z")],
            &latest,
        );
        assert_eq!(out[0].last_activity_at, "2026-01-11T00:00:00+00:00");
        // No messages: the creation time stands.
        assert_eq!(out[1].last_activity_at, "2025-01-02T00:00:00Z");
    }

    #[test]
    fn ties_break_by_id_and_offsets_compare_as_instants() {
        let conn = local_db();
        // The same instant written two ways, and a later one in a non-UTC offset.
        message(&conn, "m1", "dm-a", "peer", "2026-01-01T10:00:00Z");
        message(&conn, "m2", "dm-b", "peer", "2026-01-01T10:00:00+00:00");
        message(&conn, "m3", "dm-c", "peer", "2026-01-01T06:00:00-05:00");

        let input = || {
            vec![
                dm("dm-a", "2025-01-01T00:00:00Z"),
                dm("dm-b", "2025-01-01T00:00:00Z"),
                dm("dm-c", "2025-01-01T00:00:00Z"),
            ]
        };
        let got = order(&conn, input());
        assert_eq!(got, ["dm-c", "dm-b", "dm-a"]);
        // Stable across reads whatever order the DS returned.
        let mut reversed = input();
        reversed.reverse();
        assert_eq!(order(&conn, reversed), got);
    }

    #[test]
    fn sqlite_datetime_and_unparseable_stamps_still_order() {
        let conn = local_db();
        let got = order(
            &conn,
            vec![
                dm("dm-garbage", "not a time"),
                dm("dm-sqlite", "2026-01-02 00:00:00"),
                dm("dm-rfc", "2026-01-01T00:00:00Z"),
            ],
        );
        assert_eq!(got, ["dm-sqlite", "dm-rfc", "dm-garbage"]);
    }
}

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
        // Bob created the pending request first; the lookup must still find it.
        let conn = db();
        setup(&conn);
        create_dm(&conn, "dm1", "bob", &["bob", "alice"]);

        assert_eq!(find_two_person_dm(&conn, "alice", "bob").as_deref(), Some("dm1"));
    }
}
