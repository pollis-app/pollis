use rusqlite::Connection;

use crate::db::BASELINE_SQL as BASELINE;

use crate::db::queries::MESSAGES_BY_SENDER as QUERY_MESSAGES_BY_SENDER;
use crate::db::queries::CHANNEL_PREVIEWS as QUERY_CHANNEL_PREVIEWS;

// Both queries operate on the remote schema. Tests use rusqlite in-memory
// (same SQLite dialect, no libsql threading conflict in test binaries).
fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    conn.execute_batch(BASELINE).unwrap();
    conn
}

fn setup(conn: &Connection) {
    // Users
    conn.execute("INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice')", []).unwrap();
    conn.execute("INSERT INTO users (id, email, username) VALUES ('bob',   'bob@x.com',   'bob')", []).unwrap();

    // Groups (alphabetical names so ordering is deterministic)
    conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g-personal', 'personal', 'alice')", []).unwrap();
    conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g-work',     'work',     'alice')", []).unwrap();

    // Both users are members of both groups
    for gid in ["g-personal", "g-work"] {
        for uid in ["alice", "bob"] {
            conn.execute(
                "INSERT INTO group_member (group_id, user_id) VALUES (?1, ?2)",
                rusqlite::params![gid, uid],
            ).unwrap();
        }
    }

    // Channels (alphabetical names within groups)
    conn.execute("INSERT INTO channels (id, group_id, name, created_at) VALUES ('ch-personal-random',  'g-personal', 'random',      '2024-01-01T00:00:00Z')", []).unwrap();
    conn.execute("INSERT INTO channels (id, group_id, name, created_at) VALUES ('ch-work-engineering', 'g-work',     'engineering', '2024-01-01T00:00:00Z')", []).unwrap();
    conn.execute("INSERT INTO channels (id, group_id, name, created_at) VALUES ('ch-work-general',     'g-work',     'general',     '2024-01-01T00:00:00Z')", []).unwrap();

    // Messages — alice sends in all three channels, bob sends once in work/general
    conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m1', 'ch-work-general',     'alice', 'hello team',  '2024-01-01T10:00:00Z')", []).unwrap();
    conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m2', 'ch-work-general',     'bob',   'hi alice',    '2024-01-01T10:01:00Z')", []).unwrap();
    conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m3', 'ch-work-engineering', 'alice', 'ship it',     '2024-01-02T09:00:00Z')", []).unwrap();
    conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m4', 'ch-personal-random',  'alice', 'lol',         '2024-01-03T12:00:00Z')", []).unwrap();
    conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m5', 'ch-work-general',     'alice', 'see you all', '2024-01-04T17:00:00Z')", []).unwrap();
}

#[test]
fn messages_by_sender_ordered_by_group_then_channel_then_time() {
    let conn = db();
    setup(&conn);

    let mut stmt = conn.prepare(QUERY_MESSAGES_BY_SENDER).unwrap();
    // (group_name, channel_name, sent_at)
    let results: Vec<(String, String, String)> = stmt.query_map(
        rusqlite::params!["alice"],
        |row| Ok((row.get(1)?, row.get(3)?, row.get(7)?)),
    ).unwrap().map(|r| r.unwrap()).collect();

    // alice sent 4 messages: m1, m3, m4, m5 (not m2 which is bob's)
    // Expected order: personal/random, work/engineering, work/general (x2 by time)
    assert_eq!(results.len(), 4);
    assert_eq!(results[0], ("personal".into(), "random".into(),      "2024-01-03T12:00:00Z".into()));
    assert_eq!(results[1], ("work".into(),     "engineering".into(), "2024-01-02T09:00:00Z".into()));
    assert_eq!(results[2], ("work".into(),     "general".into(),     "2024-01-01T10:00:00Z".into()));
    assert_eq!(results[3], ("work".into(),     "general".into(),     "2024-01-04T17:00:00Z".into()));
}

#[test]
fn messages_by_sender_excludes_other_senders() {
    let conn = db();
    setup(&conn);

    let mut stmt = conn.prepare(QUERY_MESSAGES_BY_SENDER).unwrap();
    let count = stmt.query_map(rusqlite::params!["bob"], |_| Ok(()))
        .unwrap().count();

    // Bob only sent m2
    assert_eq!(count, 1);
}

#[test]
fn channel_previews_ordered_most_recent_first() {
    let conn = db();
    setup(&conn);

    let mut stmt = conn.prepare(QUERY_CHANNEL_PREVIEWS).unwrap();
    // (channel_id, last_message, last_sender_username)
    let results: Vec<(String, Option<String>, Option<String>)> = stmt.query_map(
        rusqlite::params!["alice"],
        |row| Ok((row.get(2)?, row.get(4)?, row.get(7)?)),
    ).unwrap().map(|r| r.unwrap()).collect();

    assert_eq!(results.len(), 3, "alice belongs to 3 channels");

    // Most recent activity:
    //   work/general     — m5 at 2024-01-04 (sender: alice)
    //   personal/random  — m4 at 2024-01-03 (sender: alice)
    //   work/engineering — m3 at 2024-01-02 (sender: alice)
    let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(ids, ["ch-work-general", "ch-personal-random", "ch-work-engineering"]);

    let (_, msg, sender) = &results[0];
    assert_eq!(msg.as_deref(), Some("see you all"));
    assert_eq!(sender.as_deref(), Some("alice"));
}

#[test]
fn channel_previews_last_message_is_most_recent_not_first() {
    let conn = db();
    setup(&conn);

    // work/general has m1 (alice), m2 (bob), m5 (alice) — preview should show m5
    let mut stmt = conn.prepare(QUERY_CHANNEL_PREVIEWS).unwrap();
    let results: Vec<(String, Option<String>, Option<String>)> = stmt.query_map(
        rusqlite::params!["bob"],
        |row| Ok((row.get(2)?, row.get(4)?, row.get(7)?)),
    ).unwrap().map(|r| r.unwrap()).collect();

    let general = results.iter().find(|(id, _, _)| id == "ch-work-general").unwrap();
    assert_eq!(general.1.as_deref(), Some("see you all"), "preview should show m5 not m1 or m2");
    assert_eq!(general.2.as_deref(), Some("alice"), "sender of last message is alice");
}

#[test]
fn channel_previews_empty_channel_appears_last() {
    let conn = db();
    setup(&conn);

    // Add an empty channel both users are in (via g-work membership)
    conn.execute("INSERT INTO channels (id, group_id, name, created_at) VALUES ('ch-work-quiet', 'g-work', 'quiet', '2024-01-01T00:00:00Z')", []).unwrap();

    let mut stmt = conn.prepare(QUERY_CHANNEL_PREVIEWS).unwrap();
    let results: Vec<(String, Option<String>)> = stmt.query_map(
        rusqlite::params!["bob"],
        |row| Ok((row.get(2)?, row.get(4)?)),
    ).unwrap().map(|r| r.unwrap()).collect();

    let last = results.last().unwrap();
    assert_eq!(last.0, "ch-work-quiet");
    assert!(last.1.is_none(), "empty channel has no last_message");
}

#[test]
fn channel_previews_excludes_channels_user_is_not_in() {
    let conn = db();
    setup(&conn);

    // Carol has her own group — bob is not a member
    conn.execute("INSERT INTO users (id, email, username) VALUES ('carol', 'carol@x.com', 'carol')", []).unwrap();
    conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g-secret', 'secret', 'carol')", []).unwrap();
    conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g-secret', 'carol')", []).unwrap();
    conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('ch-secret', 'g-secret', 'private')", []).unwrap();

    let mut stmt = conn.prepare(QUERY_CHANNEL_PREVIEWS).unwrap();
    let channel_ids: Vec<String> = stmt.query_map(
        rusqlite::params!["bob"],
        |row| row.get(2),
    ).unwrap().map(|r| r.unwrap()).collect();

    assert!(!channel_ids.contains(&"ch-secret".to_string()));
}
