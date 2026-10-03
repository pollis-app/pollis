// The remaining tests pin schema constraints (CHECK, UNIQUE, PRIMARY KEY, FK,
// DEFAULT, ON DELETE CASCADE) against the shipped remote schema, plus the
// derive_slug contract. Query-simulation tests for the client SQL #987 removed
// were deleted.
use rusqlite::Connection;

/// The SHIPPED remote schema — baseline plus every numbered migration, in the
/// order `scripts/db-apply.sh` applies them. #875: this used to be `BASELINE`
/// plus a hand-written `EXTRA_TABLES` block standing in for the later
/// migrations, which is how a fixture ends up laxer than production.
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

    conn.execute("INSERT INTO conversation (id, kind) VALUES ('g1', 'group')", []).unwrap();
    conn.execute("INSERT INTO groups (id, name, description, owner_id) VALUES ('g1', 'Test Group', 'a group', 'alice')", []).unwrap();
    conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')", []).unwrap();
    conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')", []).unwrap();

    conn.execute("INSERT INTO conversation (id, kind) VALUES ('ch1', 'channel')", []).unwrap();
    conn.execute("INSERT INTO channels (id, group_id, name, channel_type) VALUES ('ch1', 'g1', 'general', 'text')", []).unwrap();
    conn.execute("INSERT INTO conversation (id, kind) VALUES ('ch2', 'channel')", []).unwrap();
    conn.execute("INSERT INTO channels (id, group_id, name, channel_type) VALUES ('ch2', 'g1', 'random', 'text')", []).unwrap();
}

// ── derive_slug ────────────────────────────────────────────────────────
//
// The function moved to `pollis-api` in #987 so the client and the DS derive a
// slug from ONE implementation — `POST /v1/read/group-by-slug` runs it
// server-side now, and two copies that agree today are exactly how a lookup
// starts silently missing. These cases stay here because they are the client's
// contract with it.

#[test]
fn slug_simple_name() {
    assert_eq!(pollis_api::directory::derive_slug("Test Group"), "test-group");
}

#[test]
fn slug_special_characters_stripped() {
    assert_eq!(pollis_api::directory::derive_slug("Hello, World!"), "hello-world");
}

#[test]
fn slug_multiple_spaces_collapsed() {
    assert_eq!(pollis_api::directory::derive_slug("a   b"), "a-b");
}

#[test]
fn slug_leading_trailing_hyphens_trimmed() {
    assert_eq!(pollis_api::directory::derive_slug("-test-"), "test");
}

#[test]
fn slug_consecutive_hyphens_collapsed() {
    assert_eq!(pollis_api::directory::derive_slug("a---b"), "a-b");
}

#[test]
fn slug_mixed_case_lowered() {
    assert_eq!(pollis_api::directory::derive_slug("My Cool Group"), "my-cool-group");
}

#[test]
fn slug_already_clean() {
    assert_eq!(pollis_api::directory::derive_slug("simple"), "simple");
}

#[test]
fn slug_unicode_stripped() {
    assert_eq!(pollis_api::directory::derive_slug("café"), "caf");
}

// ── column defaults ────────────────────────────────────────────────────

#[test]
fn channel_type_defaults_to_text() {
    let conn = db();
    setup(&conn);

    // Insert channel without explicit channel_type
    conn.execute("INSERT INTO conversation (id, kind) VALUES ('ch-no-type', 'channel')", []).unwrap();
    conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('ch-no-type', 'g1', 'untyped')", []).unwrap();

    let ct: String = conn.query_row(
        "SELECT channel_type FROM channels WHERE id = 'ch-no-type'",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(ct, "text");
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

// ── invites ────────────────────────────────────────────────────────────

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

// ── join requests ──────────────────────────────────────────────────────

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

// ── membership constraints ─────────────────────────────────────────────

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
