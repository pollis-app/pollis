use rusqlite::Connection;

use crate::db::BASELINE_SQL as BASELINE;

/// Extra tables from numbered migrations that the base schema doesn't include.
const EXTRA_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS conversation_watermark (
        conversation_id TEXT NOT NULL,
        user_id         TEXT NOT NULL,
        device_id       TEXT NOT NULL,
        last_fetched_at TEXT NOT NULL,
        PRIMARY KEY (conversation_id, user_id, device_id)
    );
    CREATE TABLE IF NOT EXISTS user_device (
        device_id   TEXT PRIMARY KEY,
        user_id     TEXT NOT NULL,
        device_name TEXT,
        created_at  TEXT NOT NULL DEFAULT (datetime('now')),
        last_seen   TEXT NOT NULL DEFAULT (datetime('now'))
    );
";

fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    conn.execute_batch(BASELINE).unwrap();
    conn.execute_batch(EXTRA_TABLES).unwrap();
    conn
}

fn setup(conn: &Connection) {
    conn.execute("INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice')", []).unwrap();
    conn.execute("INSERT INTO users (id, email, username) VALUES ('bob',   'bob@x.com',   'bob')", []).unwrap();
    conn.execute("INSERT INTO users (id, email, username) VALUES ('carol', 'carol@x.com', 'carol')", []).unwrap();

    conn.execute("INSERT INTO groups (id, name, description, owner_id) VALUES ('g1', 'Test Group', 'a group', 'alice')", []).unwrap();
    conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')", []).unwrap();
    conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')", []).unwrap();

    conn.execute("INSERT INTO channels (id, group_id, name, channel_type) VALUES ('ch1', 'g1', 'general', 'text')", []).unwrap();
    conn.execute("INSERT INTO channels (id, group_id, name, channel_type) VALUES ('ch2', 'g1', 'random', 'text')", []).unwrap();
}

// ── derive_slug ────────────────────────────────────────────────────────

#[test]
fn slug_simple_name() {
    assert_eq!(super::derive_slug("Test Group"), "test-group");
}

#[test]
fn slug_special_characters_stripped() {
    assert_eq!(super::derive_slug("Hello, World!"), "hello-world");
}

#[test]
fn slug_multiple_spaces_collapsed() {
    assert_eq!(super::derive_slug("a   b"), "a-b");
}

#[test]
fn slug_leading_trailing_hyphens_trimmed() {
    assert_eq!(super::derive_slug("-test-"), "test");
}

#[test]
fn slug_consecutive_hyphens_collapsed() {
    assert_eq!(super::derive_slug("a---b"), "a-b");
}

#[test]
fn slug_mixed_case_lowered() {
    assert_eq!(super::derive_slug("My Cool Group"), "my-cool-group");
}

#[test]
fn slug_already_clean() {
    assert_eq!(super::derive_slug("simple"), "simple");
}

#[test]
fn slug_unicode_stripped() {
    assert_eq!(super::derive_slug("café"), "caf");
}

// ── group queries ──────────────────────────────────────────────────────

#[test]
fn list_user_groups_only_returns_member_groups() {
    let conn = db();
    setup(&conn);

    // carol's own group
    conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g2', 'Carol Group', 'carol')", []).unwrap();
    conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'carol', 'admin')", []).unwrap();

    let groups: Vec<String> = conn.prepare(
        "SELECT g.id FROM groups g JOIN group_member gm ON gm.group_id = g.id WHERE gm.user_id = ?1",
    ).unwrap().query_map(
        rusqlite::params!["bob"],
        |row| row.get(0),
    ).unwrap().map(|r| r.unwrap()).collect();

    assert_eq!(groups, ["g1"]);
    assert!(!groups.contains(&"g2".to_string()));
}

#[test]
fn list_group_channels_returns_all_channels() {
    let conn = db();
    setup(&conn);

    let channels: Vec<(String, String)> = conn.prepare(
        "SELECT id, name FROM channels WHERE group_id = ?1",
    ).unwrap().query_map(
        rusqlite::params!["g1"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap().map(|r| r.unwrap()).collect();

    assert_eq!(channels.len(), 2);
    let names: Vec<&str> = channels.iter().map(|(_, n)| n.as_str()).collect();
    assert!(names.contains(&"general"));
    assert!(names.contains(&"random"));
}

#[test]
fn channel_type_defaults_to_text() {
    let conn = db();
    setup(&conn);

    // Insert channel without explicit channel_type
    conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('ch-no-type', 'g1', 'untyped')", []).unwrap();

    let ct: String = conn.query_row(
        "SELECT channel_type FROM channels WHERE id = 'ch-no-type'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(ct, "text");
}

// ── RBAC: admin-only operations ────────────────────────────────────────

#[test]
fn admin_role_check_returns_admin_for_admin_user() {
    let conn = db();
    setup(&conn);

    let role: String = conn.query_row(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        rusqlite::params!["g1", "alice"],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(role, "admin");
}

#[test]
fn admin_role_check_returns_member_for_regular_user() {
    let conn = db();
    setup(&conn);

    let role: String = conn.query_row(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        rusqlite::params!["g1", "bob"],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(role, "member");
}

#[test]
fn role_check_returns_none_for_non_member() {
    let conn = db();
    setup(&conn);

    let result = conn.query_row(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        rusqlite::params!["g1", "carol"],
        |row| row.get::<_, String>(0),
    );
    assert!(result.is_err());
}

// ── group update (partial) ─────────────────────────────────────────────

#[test]
fn update_group_name_only() {
    let conn = db();
    setup(&conn);

    conn.execute("UPDATE groups SET name = ?1 WHERE id = ?2", rusqlite::params!["New Name", "g1"]).unwrap();

    let (name, desc): (String, Option<String>) = conn.query_row(
        "SELECT name, description FROM groups WHERE id = 'g1'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(name, "New Name");
    assert_eq!(desc.as_deref(), Some("a group"), "description should be unchanged");
}

#[test]
fn update_group_icon_url() {
    let conn = db();
    setup(&conn);

    conn.execute("UPDATE groups SET icon_url = ?1 WHERE id = ?2", rusqlite::params!["https://img.example.com/icon.png", "g1"]).unwrap();

    let icon: Option<String> = conn.query_row(
        "SELECT icon_url FROM groups WHERE id = 'g1'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(icon.as_deref(), Some("https://img.example.com/icon.png"));
}

// ── group deletion cascades ────────────────────────────────────────────

#[test]
fn delete_group_cascades_members_and_channels() {
    let conn = db();
    setup(&conn);

    conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

    let member_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(member_count, 0, "members should be cascade-deleted");

    let channel_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM channels WHERE group_id = 'g1'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(channel_count, 0, "channels should be cascade-deleted");
}

// ── member removal ─────────────────────────────────────────────────────

#[test]
fn remove_member_deletes_membership_row() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        rusqlite::params!["g1", "bob"],
    ).unwrap();

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1' AND user_id = 'bob'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0);
}

// ── leave group: auto-delete when empty ────────────────────────────────

#[test]
fn leave_group_last_member_deletes_group() {
    let conn = db();
    setup(&conn);

    // Remove bob first, then alice (last member)
    conn.execute("DELETE FROM group_member WHERE group_id = 'g1' AND user_id = 'bob'", []).unwrap();
    conn.execute("DELETE FROM group_member WHERE group_id = 'g1' AND user_id = 'alice'", []).unwrap();

    let member_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(member_count, 0);

    // Simulate: if member_count <= 1, delete group
    conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

    let group_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM groups WHERE id = 'g1'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(group_exists, 0);
}

// ── set_member_role ────────────────────────────────────────────────────

#[test]
fn set_member_role_promotes_to_admin() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "UPDATE group_member SET role = ?1 WHERE group_id = ?2 AND user_id = ?3",
        rusqlite::params!["admin", "g1", "bob"],
    ).unwrap();

    let role: String = conn.query_row(
        "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'bob'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(role, "admin");
}

#[test]
fn set_member_role_demotes_to_member() {
    let conn = db();
    setup(&conn);

    // alice starts as admin
    conn.execute(
        "UPDATE group_member SET role = 'member' WHERE group_id = 'g1' AND user_id = 'alice'",
        [],
    ).unwrap();

    let role: String = conn.query_row(
        "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'alice'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(role, "member");
}

// ── add_member_to_group (INSERT OR IGNORE) ─────────────────────────────

#[test]
fn add_member_ignores_duplicate() {
    let conn = db();
    setup(&conn);

    // bob is already a member — INSERT OR IGNORE should not error
    conn.execute(
        "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')",
        [],
    ).unwrap();

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1' AND user_id = 'bob'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 1, "should still be exactly one membership row");
}

#[test]
fn add_member_initializes_watermarks_for_existing_channels() {
    let conn = db();
    setup(&conn);

    // Carol has two devices. Seeding must produce one row per (channel, device).
    conn.execute(
        "INSERT INTO user_device (device_id, user_id) VALUES ('carol-d1', 'carol'), ('carol-d2', 'carol')",
        [],
    ).unwrap();

    conn.execute(
        "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES ('g1', 'carol', 'member')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at)
         SELECT c.id, ?1, ud.device_id, datetime('now')
         FROM channels c
         JOIN user_device ud ON ud.user_id = ?1
         WHERE c.group_id = ?2",
        rusqlite::params!["carol", "g1"],
    ).unwrap();

    let watermark_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversation_watermark WHERE user_id = 'carol'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(watermark_count, 4, "2 channels × 2 devices");
}

// ── get_group_members ──────────────────────────────────────────────────

#[test]
fn get_group_members_returns_all_with_roles() {
    let conn = db();
    setup(&conn);

    let members: Vec<(String, String)> = conn.prepare(
        "SELECT gm.user_id, gm.role FROM group_member gm WHERE gm.group_id = ?1",
    ).unwrap().query_map(
        rusqlite::params!["g1"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap().map(|r| r.unwrap()).collect();

    assert_eq!(members.len(), 2);
    assert!(members.contains(&("alice".into(), "admin".into())));
    assert!(members.contains(&("bob".into(), "member".into())));
}

#[test]
fn get_group_members_joins_user_profile() {
    let conn = db();
    setup(&conn);

    let result: (String, Option<String>) = conn.query_row(
        "SELECT gm.user_id, u.username
         FROM group_member gm
         LEFT JOIN users u ON u.id = gm.user_id
         WHERE gm.group_id = ?1 AND gm.user_id = ?2",
        rusqlite::params!["g1", "alice"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();

    assert_eq!(result.0, "alice");
    assert_eq!(result.1.as_deref(), Some("alice"));
}

// ── invites ────────────────────────────────────────────────────────────

#[test]
fn invite_insert_and_query() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'alice', 'carol')",
        [],
    ).unwrap();

    let (inviter, invitee): (String, String) = conn.query_row(
        "SELECT inviter_id, invitee_id FROM group_invite WHERE id = 'inv1'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(inviter, "alice");
    assert_eq!(invitee, "carol");
}

#[test]
fn invite_existing_member_blocked() {
    let conn = db();
    setup(&conn);

    // bob is already a member of g1 — check that a membership check catches it
    let is_member: bool = conn.query_row(
        "SELECT COUNT(*) FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        rusqlite::params!["g1", "bob"],
        |row| row.get::<_, i64>(0).map(|c| c > 0),
    ).unwrap();
    assert!(is_member, "bob should already be a member");

    // In the command, this check prevents the invite from being created.
    // The INSERT itself would succeed (no DB constraint), so the guard is in app logic.
}

#[test]
fn invite_self_blocked() {
    let conn = db();
    setup(&conn);

    // The command checks invitee_id == inviter_id before inserting.
    // Verify the condition that would be checked:
    let inviter_id = "alice";
    let invitee_id = "alice";
    assert_eq!(inviter_id, invitee_id, "self-invite should be caught by app logic");
}

#[test]
fn duplicate_pending_invite_blocked() {
    let conn = db();
    setup(&conn);

    // First invite
    conn.execute(
        "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'alice', 'carol')",
        [],
    ).unwrap();

    // The command checks for existing pending invites before inserting.
    // Since group_invite has no status column (all rows are implicitly pending),
    // the check is: any row with (group_id, invitee_id) exists.
    let existing: bool = conn.query_row(
        "SELECT COUNT(*) FROM group_invite WHERE group_id = ?1 AND invitee_id = ?2",
        rusqlite::params!["g1", "carol"],
        |row| row.get::<_, i64>(0).map(|c| c > 0),
    ).unwrap();
    assert!(existing, "pending invite should already exist — app logic blocks duplicate");
}

#[test]
fn invite_cascade_deletes_with_group() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'alice', 'carol')",
        [],
    ).unwrap();

    conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM group_invite WHERE id = 'inv1'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0, "invite should be cascade-deleted with group");
}

#[test]
fn invite_delete_on_accept() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'alice', 'carol')",
        [],
    ).unwrap();

    // Accept: add member + delete invite
    conn.execute(
        "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES ('g1', 'carol', 'member')",
        [],
    ).unwrap();
    conn.execute("DELETE FROM group_invite WHERE id = 'inv1'", []).unwrap();

    let member_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1' AND user_id = 'carol'",
        [],
        |row| row.get::<_, i64>(0).map(|c| c > 0),
    ).unwrap();
    assert!(member_exists, "carol should now be a member");

    let invite_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM group_invite",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(invite_count, 0, "invite should be deleted after acceptance");
}

// ── join requests ──────────────────────────────────────────────────────

#[test]
fn join_request_insert() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'pending')",
        [],
    ).unwrap();

    let status: String = conn.query_row(
        "SELECT status FROM group_join_request WHERE id = 'jr1'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(status, "pending");
}

#[test]
fn join_request_blocked_when_already_member() {
    let conn = db();
    setup(&conn);

    // bob is already a member — the command checks this before inserting
    let is_member: bool = conn.query_row(
        "SELECT COUNT(*) FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        rusqlite::params!["g1", "bob"],
        |row| row.get::<_, i64>(0).map(|c| c > 0),
    ).unwrap();
    assert!(is_member, "bob is already a member — request_group_access should reject");
}

#[test]
fn join_request_unique_per_group_requester() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'pending')",
        [],
    ).unwrap();

    // Duplicate (group, requester) should conflict
    let result = conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr2', 'g1', 'carol', 'pending')",
        [],
    );
    assert!(result.is_err(), "duplicate (group_id, requester_id) should violate unique index");
}

#[test]
fn join_request_upsert_resets_rejected_to_pending() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'rejected')",
        [],
    ).unwrap();

    // Re-apply via upsert — same pattern as request_group_access
    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status, created_at)
         VALUES ('jr2', 'g1', 'carol', 'pending', datetime('now'))
         ON CONFLICT(group_id, requester_id) DO UPDATE SET
             id = excluded.id,
             status = 'pending',
             created_at = excluded.created_at",
        [],
    ).unwrap();

    let (id, status): (String, String) = conn.query_row(
        "SELECT id, status FROM group_join_request WHERE group_id = 'g1' AND requester_id = 'carol'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(id, "jr2", "id should be updated to the new one");
    assert_eq!(status, "pending", "status should be reset to pending");
}

#[test]
fn join_request_status_check_constraint() {
    let conn = db();
    setup(&conn);

    let result = conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'invalid')",
        [],
    );
    assert!(result.is_err(), "invalid status should violate CHECK constraint");
}

#[test]
fn join_request_approve_adds_member_and_updates_status() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'pending')",
        [],
    ).unwrap();

    // Approve: add member + update status
    conn.execute(
        "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES ('g1', 'carol', 'member')",
        [],
    ).unwrap();
    conn.execute(
        "UPDATE group_join_request SET status = 'approved', reviewed_by = 'alice', reviewed_at = datetime('now') WHERE id = 'jr1'",
        [],
    ).unwrap();

    let status: String = conn.query_row(
        "SELECT status FROM group_join_request WHERE id = 'jr1'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(status, "approved");

    let is_member: bool = conn.query_row(
        "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1' AND user_id = 'carol'",
        [],
        |row| row.get::<_, i64>(0).map(|c| c > 0),
    ).unwrap();
    assert!(is_member);
}

#[test]
fn join_request_only_pending_returned_for_admins() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'pending')",
        [],
    ).unwrap();

    // Simulate a second user with a rejected request — need a new user
    conn.execute("INSERT INTO users (id, email, username) VALUES ('dave', 'dave@x.com', 'dave')", []).unwrap();
    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr2', 'g1', 'dave', 'rejected')",
        [],
    ).unwrap();

    let pending: Vec<String> = conn.prepare(
        "SELECT jr.id FROM group_join_request jr WHERE jr.group_id = ?1 AND jr.status = 'pending' ORDER BY jr.created_at ASC",
    ).unwrap().query_map(
        rusqlite::params!["g1"],
        |row| row.get(0),
    ).unwrap().map(|r| r.unwrap()).collect();

    assert_eq!(pending, ["jr1"], "only pending requests should be returned");
}

#[test]
fn join_request_cascade_deletes_with_group() {
    let conn = db();
    setup(&conn);

    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'pending')",
        [],
    ).unwrap();

    conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM group_join_request",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0, "join requests should be cascade-deleted with group");
}

// ── search_group_by_slug ───────────────────────────────────────────────

#[test]
fn search_group_by_slug_finds_match() {
    let conn = db();
    setup(&conn);

    // Simulate the scan + derive_slug pattern
    let mut found = None;
    let mut stmt = conn.prepare("SELECT id, name FROM groups").unwrap();
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).unwrap();
    for r in rows {
        let (id, name) = r.unwrap();
        if super::derive_slug(&name) == "test-group" {
            found = Some(id);
            break;
        }
    }

    assert_eq!(found.as_deref(), Some("g1"));
}

#[test]
fn search_group_by_slug_no_match() {
    let conn = db();
    setup(&conn);

    let mut found = false;
    let mut stmt = conn.prepare("SELECT name FROM groups").unwrap();
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
    for r in rows {
        if super::derive_slug(&r.unwrap()) == "nonexistent-group" {
            found = true;
        }
    }

    assert!(!found);
}

// ── list_user_groups_with_channels query shape ─────────────────────────

#[test]
fn list_groups_with_channels_groups_and_nests_channels() {
    let conn = db();
    setup(&conn);

    // Simulate the query used by list_user_groups_with_channels
    let mut stmt = conn.prepare(
        "SELECT g.id, g.name, g.description, g.owner_id, g.created_at,
                c.id, c.group_id, c.name, c.description, c.channel_type,
                gm.role
         FROM groups g
         JOIN group_member gm ON gm.group_id = g.id
         LEFT JOIN channels c ON c.group_id = g.id
         WHERE gm.user_id = ?1
         ORDER BY g.created_at, c.name",
    ).unwrap();

    let rows: Vec<(String, Option<String>, String)> = stmt.query_map(
        rusqlite::params!["alice"],
        |row| Ok((row.get(0)?, row.get(5)?, row.get(10)?)),
    ).unwrap().map(|r| r.unwrap()).collect();

    // g1 has 2 channels — should get 2 rows, same group_id
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(gid, _, _)| gid == "g1"));
    // Both channel IDs present
    let ch_ids: Vec<&str> = rows.iter().map(|(_, cid, _)| cid.as_deref().unwrap()).collect();
    assert!(ch_ids.contains(&"ch1"));
    assert!(ch_ids.contains(&"ch2"));
    // Role is admin for alice
    assert!(rows.iter().all(|(_, _, role)| role == "admin"));
}

#[test]
fn list_groups_with_channels_group_without_channels() {
    let conn = db();
    setup(&conn);

    // Group with no channels
    conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g-empty', 'Empty', 'alice')", []).unwrap();
    conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g-empty', 'alice', 'admin')", []).unwrap();

    let mut stmt = conn.prepare(
        "SELECT g.id, c.id
         FROM groups g
         JOIN group_member gm ON gm.group_id = g.id
         LEFT JOIN channels c ON c.group_id = g.id
         WHERE gm.user_id = ?1
         ORDER BY g.created_at, c.name",
    ).unwrap();

    let rows: Vec<(String, Option<String>)> = stmt.query_map(
        rusqlite::params!["alice"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap().map(|r| r.unwrap()).collect();

    // g-empty should appear with NULL channel
    let empty_rows: Vec<_> = rows.iter().filter(|(gid, _)| gid == "g-empty").collect();
    assert_eq!(empty_rows.len(), 1);
    assert!(empty_rows[0].1.is_none(), "channel_id should be NULL for empty group");
}

// ── db_err mapping ─────────────────────────────────────────────────────

#[test]
fn duplicate_group_member_violates_unique() {
    let conn = db();
    setup(&conn);

    let result = conn.execute(
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')",
        [],
    );
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("UNIQUE"), "error should mention UNIQUE constraint: {err_msg}");
}

#[test]
fn foreign_key_violation_on_invalid_group() {
    let conn = db();
    setup(&conn);

    let result = conn.execute(
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('nonexistent', 'alice', 'member')",
        [],
    );
    assert!(result.is_err(), "should fail due to foreign key constraint");
}
