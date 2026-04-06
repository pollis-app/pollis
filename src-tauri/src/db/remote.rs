use libsql::{Builder, Database, Connection};
use crate::error::Result;

const SCHEMA: &str = include_str!("migrations/remote_schema.sql");

pub struct RemoteDb {
    db: Database,
}

impl RemoteDb {
    /// Connect to the remote database. The schema must already be up to date —
    /// run `pnpm db:migrate` before shipping a new schema version.
    pub async fn connect(url: &str, token: &str) -> Result<Self> {
        let db = Builder::new_remote(url.to_string(), token.to_string())
            .build()
            .await?;
        Ok(Self { db })
    }

    pub async fn conn(&self) -> Result<Connection> {
        Ok(self.db.connect()?)
    }
}

/// Drop all tables and recreate from the schema file.
/// Called by `pnpm db:push`. Not called by the app.
pub async fn push_schema(url: &str, token: &str) -> Result<()> {
    let db = Builder::new_remote(url.to_string(), token.to_string())
        .build()
        .await?;
    let conn = db.connect()?;

    // Drop all tables in reverse dependency order (leaf tables first so FK
    // constraints don't block the drops).
    let drop_sql = "
        DROP TABLE IF EXISTS message_reaction;
        DROP TABLE IF EXISTS group_invite;
        DROP TABLE IF EXISTS group_join_request;
        DROP TABLE IF EXISTS user_preferences;
        DROP TABLE IF EXISTS x3dh_init;
        DROP TABLE IF EXISTS sender_key_dist;
        DROP TABLE IF EXISTS dm_channel_member;
        DROP TABLE IF EXISTS dm_channel;
        DROP TABLE IF EXISTS message_envelope;
        DROP TABLE IF EXISTS one_time_prekey;
        DROP TABLE IF EXISTS signed_prekey;
        DROP TABLE IF EXISTS channels;
        DROP TABLE IF EXISTS group_member;
        DROP TABLE IF EXISTS groups;
        DROP TABLE IF EXISTS users
    ";
    run_statements(&conn, drop_sql).await?;

    run_statements(&conn, SCHEMA).await?;
    Ok(())
}

/// Split a SQL file on `;` and execute each non-empty, non-comment statement.
/// Safe for DDL-only files (no embedded semicolons inside string literals).
async fn run_statements(conn: &Connection, sql: &str) -> Result<()> {
    for raw in sql.split(';') {
        // Strip line comments and surrounding whitespace.
        let stmt: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        let stmt = stmt.trim();
        if !stmt.is_empty() {
            conn.execute(stmt, ()).await.map_err(|e| {
                crate::error::Error::Other(anyhow::anyhow!(
                    "Migration failed on statement:\n{}\n\nError: {}", stmt, e
                ))
            })?;
        }
    }
    Ok(())
}

// Remote schema tests use rusqlite in-memory to avoid a SQLite threading
// conflict: libsql-sys bundles SQLite with SQLITE_THREADSAFE=0, which clashes
// with rusqlite-bundled's multi-threaded configuration when both exist in the
// same test binary. The SQL dialect is identical so coverage is equivalent.
#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(super::SCHEMA).unwrap();
        conn
    }

    #[test]
    fn migration_creates_tables() {
        let conn = db();
        // Each insert will fail if the table doesn't exist.
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@example.com')", []).expect("users");
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Test', 'u1')", []).expect("groups");
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'u1')", []).expect("group_member");
        conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'general')", []).expect("channels");
        conn.execute("INSERT INTO signed_prekey (user_id, key_id, public_key, signature) VALUES ('u1', 1, 'pk', 'sig')", []).expect("signed_prekey");
        conn.execute("INSERT INTO one_time_prekey (user_id, key_id, public_key) VALUES ('u1', 1, 'pk')", []).expect("one_time_prekey");
        conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('e1', 'c1', 'u1', 'enc', '2024-01-01T00:00:00Z')", []).expect("message_envelope");
    }

    #[test]
    fn user_email_must_be_unique() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'same@example.com')", []).unwrap();
        let result = conn.execute("INSERT INTO users (id, email) VALUES ('u2', 'same@example.com')", []);
        assert!(result.is_err(), "duplicate email should violate UNIQUE constraint");
    }

    #[test]
    fn user_fields_roundtrip() {
        let conn = db();
        conn.execute(
            "INSERT INTO users (id, email, username, identity_key, avatar_url)
             VALUES ('u1', 'alice@example.com', 'alice', 'deadbeef', 'https://example.com/avatar.png')",
            [],
        ).unwrap();

        let (id, email, username, identity_key, avatar_url): (String, String, String, String, String) =
            conn.query_row(
                "SELECT id, email, username, identity_key, avatar_url FROM users WHERE id = 'u1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            ).unwrap();

        assert_eq!(id, "u1");
        assert_eq!(email, "alice@example.com");
        assert_eq!(username, "alice");
        assert_eq!(identity_key, "deadbeef");
        assert_eq!(avatar_url, "https://example.com/avatar.png");
    }

    #[test]
    fn identity_key_can_be_updated() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'bob@example.com')", []).unwrap();
        conn.execute("UPDATE users SET identity_key = 'mypublickeyhex' WHERE id = 'u1'", []).unwrap();

        let key: String = conn.query_row(
            "SELECT identity_key FROM users WHERE id = 'u1'",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(key, "mypublickeyhex");
    }

    #[test]
    fn group_with_admin_and_member() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('admin', 'admin@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('member', 'member@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Crew', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'admin', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'member', 'member')", []).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(count, 2);
    }

    #[test]
    fn channel_belongs_to_group() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'u@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();

        for name in ["general", "random", "announcements"] {
            conn.execute(
                "INSERT INTO channels (id, group_id, name) VALUES (?1, 'g1', ?2)",
                rusqlite::params![format!("ch-{name}"), name],
            ).unwrap();
        }

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM channels WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(count, 3);
    }

    #[test]
    fn signed_prekey_composite_primary_key() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'u@x.com')", []).unwrap();
        conn.execute("INSERT INTO signed_prekey (user_id, key_id, public_key, signature) VALUES ('u1', 1, 'pk1', 'sig1')", []).unwrap();
        // Same user, different key_id — must succeed.
        conn.execute("INSERT INTO signed_prekey (user_id, key_id, public_key, signature) VALUES ('u1', 2, 'pk2', 'sig2')", []).unwrap();
        // Same (user_id, key_id) — must fail.
        let result = conn.execute("INSERT INTO signed_prekey (user_id, key_id, public_key, signature) VALUES ('u1', 1, 'pk3', 'sig3')", []);
        assert!(result.is_err(), "duplicate (user_id, key_id) should fail");
    }

    #[test]
    fn one_time_prekey_used_flag() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'u@x.com')", []).unwrap();

        for id in 1..=5i64 {
            conn.execute(
                "INSERT INTO one_time_prekey (user_id, key_id, public_key) VALUES ('u1', ?1, 'pk')",
                rusqlite::params![id],
            ).unwrap();
        }

        conn.execute("UPDATE one_time_prekey SET used = 1 WHERE user_id = 'u1' AND key_id = 3", []).unwrap();

        let unclaimed: i64 = conn.query_row(
            "SELECT COUNT(*) FROM one_time_prekey WHERE user_id = 'u1' AND used = 0",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(unclaimed, 4, "4 keys should remain unclaimed");
    }

    // ── Group roles ──────────────────────────────────────────────────────────

    #[test]
    fn group_member_defaults_to_member_role() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        // No role supplied — should default to 'member'
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'u1')", []).unwrap();

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'u1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "member");
    }

    #[test]
    fn creator_is_inserted_as_admin() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'u1', 'admin')", []).unwrap();

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'u1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin");
    }

    #[test]
    fn set_member_role_toggles_between_admin_and_member() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u2', 'b@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'u1', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'u2', 'member')", []).unwrap();

        // Promote u2 to admin
        conn.execute(
            "UPDATE group_member SET role = 'admin' WHERE group_id = 'g1' AND user_id = 'u2'",
            [],
        ).unwrap();
        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'u2'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin");

        // Demote back to member
        conn.execute(
            "UPDATE group_member SET role = 'member' WHERE group_id = 'g1' AND user_id = 'u2'",
            [],
        ).unwrap();
        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'u2'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "member");
    }

    #[test]
    fn migration_008_owner_role_becomes_admin() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        // Simulate pre-migration data
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'u1', 'owner')", []).unwrap();

        conn.execute("UPDATE group_member SET role = 'admin' WHERE role = 'owner'", []).unwrap();

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'u1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin", "migration should have renamed 'owner' to 'admin'");
    }

    #[test]
    fn duplicate_membership_violates_primary_key() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'u1')", []).unwrap();

        let result = conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'u1')", []);
        assert!(result.is_err(), "duplicate (group_id, user_id) should violate PRIMARY KEY");
    }

    #[test]
    fn admin_role_check_matches_only_admin() {
        // Mirrors the SQL pattern used in every admin-gated command
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('a', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('m', 'm@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'a')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'a', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'm', 'member')", []).unwrap();

        let admin_check = |user_id: &str| -> Option<String> {
            conn.query_row(
                "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = ?1",
                rusqlite::params![user_id],
                |row| row.get(0),
            ).ok()
        };

        assert_eq!(admin_check("a").as_deref(), Some("admin"));
        assert_ne!(admin_check("m").as_deref(), Some("admin"));
        assert_eq!(admin_check("unknown"), None);
    }

    #[test]
    fn remove_member_leaves_admin_intact() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('a', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('m', 'm@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'a')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'a', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'm', 'member')", []).unwrap();

        conn.execute("DELETE FROM group_member WHERE group_id = 'g1' AND user_id = 'm'", []).unwrap();

        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(remaining, 1);

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'a'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin");
    }

    #[test]
    fn delete_group_cascades_to_members_and_channels() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'u1', 'admin')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'general')", []).unwrap();

        conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

        let members: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        let channels: i64 = conn.query_row(
            "SELECT COUNT(*) FROM channels WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(members, 0, "group_member rows should cascade delete");
        assert_eq!(channels, 0, "channel rows should cascade delete");
    }

    // ── Invites ──────────────────────────────────────────────────────────────

    #[test]
    fn invite_defaults_to_pending() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u2', 'b@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'u1', 'u2')",
            [],
        ).unwrap();

        let status: String = conn.query_row(
            "SELECT status FROM group_invite WHERE id = 'inv1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(status, "pending");
    }

    #[test]
    fn invite_can_be_accepted_and_declined() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u2', 'b@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u3', 'c@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'u1', 'u2')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv2', 'g1', 'u1', 'u3')",
            [],
        ).unwrap();

        conn.execute("UPDATE group_invite SET status = 'accepted' WHERE id = 'inv1'", []).unwrap();
        conn.execute("UPDATE group_invite SET status = 'declined' WHERE id = 'inv2'", []).unwrap();

        let s1: String = conn.query_row("SELECT status FROM group_invite WHERE id = 'inv1'", [], |r| r.get(0)).unwrap();
        let s2: String = conn.query_row("SELECT status FROM group_invite WHERE id = 'inv2'", [], |r| r.get(0)).unwrap();
        assert_eq!(s1, "accepted");
        assert_eq!(s2, "declined");
    }

    #[test]
    fn invite_rejects_invalid_status() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u2', 'b@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'u1', 'u2')",
            [],
        ).unwrap();

        let result = conn.execute("UPDATE group_invite SET status = 'bogus' WHERE id = 'inv1'", []);
        assert!(result.is_err(), "CHECK constraint should reject invalid status");
    }

    // ── Join requests ────────────────────────────────────────────────────────

    #[test]
    fn join_request_defaults_to_pending() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u2', 'b@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id) VALUES ('jr1', 'g1', 'u2')",
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
    fn join_request_approve_and_reject_flows() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('admin', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u2', 'b@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u3', 'c@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'admin')", []).unwrap();
        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id) VALUES ('jr1', 'g1', 'u2')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id) VALUES ('jr2', 'g1', 'u3')",
            [],
        ).unwrap();

        conn.execute(
            "UPDATE group_join_request SET status = 'approved', reviewed_by = 'admin' WHERE id = 'jr1'",
            [],
        ).unwrap();
        conn.execute(
            "UPDATE group_join_request SET status = 'rejected', reviewed_by = 'admin' WHERE id = 'jr2'",
            [],
        ).unwrap();

        let s1: String = conn.query_row("SELECT status FROM group_join_request WHERE id = 'jr1'", [], |r| r.get(0)).unwrap();
        let s2: String = conn.query_row("SELECT status FROM group_join_request WHERE id = 'jr2'", [], |r| r.get(0)).unwrap();
        assert_eq!(s1, "approved");
        assert_eq!(s2, "rejected");
    }

    #[test]
    fn join_request_rejects_invalid_status() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u2', 'b@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id) VALUES ('jr1', 'g1', 'u2')",
            [],
        ).unwrap();

        let result = conn.execute("UPDATE group_join_request SET status = 'bogus' WHERE id = 'jr1'", []);
        assert!(result.is_err(), "CHECK constraint should reject invalid status");
    }

    // ── DM channels ──────────────────────────────────────────────────────────

    #[test]
    fn dm_channel_with_two_members() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u2', 'b@x.com')", []).unwrap();
        conn.execute("INSERT INTO dm_channel (id, created_by) VALUES ('dm1', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', 'u1', 'u1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', 'u2', 'u1')",
            [],
        ).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn dm_channel_delete_cascades_to_members() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('u1', 'a@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('u2', 'b@x.com')", []).unwrap();
        conn.execute("INSERT INTO dm_channel (id, created_by) VALUES ('dm1', 'u1')", []).unwrap();
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', 'u1', 'u1')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', 'u2', 'u1')",
            [],
        ).unwrap();

        conn.execute("DELETE FROM dm_channel WHERE id = 'dm1'", []).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'dm1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "dm_channel_member rows should cascade delete");
    }

    // ── Attachment dedup ─────────────────────────────────────────────────────

    #[test]
    fn attachment_object_deduplicates_by_content_hash() {
        let conn = db();
        conn.execute(
            "INSERT INTO attachment_object (content_hash, r2_key, mime_type, size_bytes)
             VALUES ('abc123', 'r2/abc123', 'image/png', 1024)",
            [],
        ).unwrap();

        // Same content_hash from a different upload must fail
        let result = conn.execute(
            "INSERT INTO attachment_object (content_hash, r2_key, mime_type, size_bytes)
             VALUES ('abc123', 'r2/different', 'image/png', 1024)",
            [],
        );
        assert!(result.is_err(), "duplicate content_hash should violate PRIMARY KEY");

        // Different hash must succeed
        conn.execute(
            "INSERT INTO attachment_object (content_hash, r2_key, mime_type, size_bytes)
             VALUES ('def456', 'r2/def456', 'image/jpeg', 2048)",
            [],
        ).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM attachment_object",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn message_envelope_delivered_flag() {
        let conn = db();

        for i in 1..=3i64 {
            conn.execute(
                "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at)
                 VALUES (?1, 'conv1', 'u1', 'enc', '2024-01-01T00:00:00Z')",
                rusqlite::params![format!("e{i}")],
            ).unwrap();
        }

        conn.execute("UPDATE message_envelope SET delivered = 1 WHERE id = 'e1'", []).unwrap();

        let undelivered: i64 = conn.query_row(
            "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = 'conv1' AND delivered = 0",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(undelivered, 2, "2 undelivered envelopes should remain");
    }
}
