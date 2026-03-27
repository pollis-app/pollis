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
    fn group_with_owner_and_member() {
        let conn = db();
        conn.execute("INSERT INTO users (id, email) VALUES ('owner', 'owner@x.com')", []).unwrap();
        conn.execute("INSERT INTO users (id, email) VALUES ('member', 'member@x.com')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Crew', 'owner')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'owner', 'owner')", []).unwrap();
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
