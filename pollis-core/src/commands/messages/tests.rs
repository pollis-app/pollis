use rusqlite::Connection;

// #987 removed the first half of this file. It exercised the SQL behind
// `list_messages_by_sender` and `list_channel_previews`, two commands #987
// deleted: both keyed on `message_envelope.sender_id`, which sealed sender
// (#607) has written as the literal string "sealed" for every send since, so
// neither could match a real user any more. The queries went with the commands
// and the fixtures went with the queries.

// ── #874: batched last-message previews ──────────────────────────────────────
//
// The sidebar's preview rows used to issue one `read_channel_messages` /
// `read_dm_messages` IPC call EACH. `read_last_messages` collapses them into a
// single call whose entire correctness lives in one SQL string, so that string
// is what these exercise — against the LOCAL schema, which is where the
// `message` table actually lives.

use super::read::last_messages_sql;

/// The local (device) SQLite schema, as `LocalDb::open` applies it.
fn local_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::local::apply_local_schema(&conn).unwrap();
    conn
}

fn insert_local_message(conn: &Connection, id: &str, conversation_id: &str, content: &str, sent_at: &str) {
    conn.execute(
        "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, received_at)
         VALUES (?1, ?2, 'alice', X'00', ?3, ?4, ?4)",
        rusqlite::params![id, conversation_id, content, sent_at],
    ).unwrap();
}

fn last_messages(conn: &Connection, ids: &[&str]) -> Vec<(String, String, String)> {
    let sql = last_messages_sql(ids.len());
    let mut stmt = conn.prepare(&sql).unwrap();
    // (conversation_id, id, content)
    stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
        Ok((row.get(1)?, row.get(0)?, row.get::<_, Option<String>>(4)?.unwrap_or_default()))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

#[test]
fn last_messages_returns_exactly_one_newest_row_per_conversation() {
    let conn = local_db();
    insert_local_message(&conn, "m1", "conv-a", "first",  "2024-01-01T10:00:00Z");
    insert_local_message(&conn, "m2", "conv-a", "newest", "2024-01-04T17:00:00Z");
    insert_local_message(&conn, "m3", "conv-a", "middle", "2024-01-02T09:00:00Z");
    insert_local_message(&conn, "m4", "conv-b", "only-b", "2024-01-03T12:00:00Z");

    let mut rows = last_messages(&conn, &["conv-a", "conv-b"]);
    rows.sort();

    assert_eq!(rows.len(), 2, "one row per conversation, not one per message");
    assert_eq!(rows[0], ("conv-a".to_string(), "m2".to_string(), "newest".to_string()));
    assert_eq!(rows[1], ("conv-b".to_string(), "m4".to_string(), "only-b".to_string()));
}

#[test]
fn last_messages_omits_conversations_with_no_messages() {
    let conn = local_db();
    insert_local_message(&conn, "m1", "conv-a", "hello", "2024-01-01T10:00:00Z");

    let rows = last_messages(&conn, &["conv-a", "conv-empty"]);

    // Absence IS the answer for an empty conversation — the caller keys by
    // conversation_id, so a null placeholder would be a second spelling of it.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "conv-a");
}

#[test]
fn last_messages_breaks_sent_at_ties_the_same_way_the_page_read_does() {
    let conn = local_db();
    // Identical timestamps: the page read orders `sent_at DESC, id DESC`, so
    // the higher id wins. A preview disagreeing with the top of the opened
    // conversation is the bug this pins shut.
    insert_local_message(&conn, "m-aaa", "conv-a", "lower id",  "2024-01-01T10:00:00Z");
    insert_local_message(&conn, "m-zzz", "conv-a", "higher id", "2024-01-01T10:00:00Z");

    let rows = last_messages(&conn, &["conv-a"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "m-zzz");

    // And the page read agrees.
    let top: String = conn.query_row(
        "SELECT id FROM message WHERE conversation_id = 'conv-a' ORDER BY sent_at DESC, id DESC LIMIT 1",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(top, rows[0].1);
}

#[test]
fn last_messages_covers_every_requested_conversation_in_one_statement() {
    let conn = local_db();
    let ids: Vec<String> = (0..25).map(|i| format!("conv-{i:02}")).collect();
    for (i, cid) in ids.iter().enumerate() {
        insert_local_message(&conn, &format!("m{i}"), cid, "hi", "2024-01-01T10:00:00Z");
    }

    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let rows = last_messages(&conn, &refs);

    // 25 conversations, ONE prepared statement. This is the whole point of the
    // command: the previous shape was 25 separate IPC round trips.
    assert_eq!(rows.len(), 25);
}

// ── #1039: the forward page read ─────────────────────────────────────────────
//
// `read_messages_after` is what fills the hole a jump-to-message leaves
// between the anchor's page and the live newest page. Its contract is a paging
// one — feed `next_cursor` back in and you visit every row once, then stop —
// so that is what is pinned, not any single page.

use super::read::read_after_on;
use super::types::MessageCursor;

fn cursor(sent_at: &str, id: &str) -> MessageCursor {
    MessageCursor { sent_at: sent_at.to_string(), id: id.to_string() }
}

/// Walk forward from `start` in pages of `limit`, the way the renderer does,
/// returning every id in the order it was handed back (each page newest-first).
fn walk_forward(conn: &Connection, start: MessageCursor, limit: i64) -> (Vec<String>, usize) {
    let mut cursor = start;
    let mut ids = Vec::new();
    let mut pages = 0;
    loop {
        let (page, next) = read_after_on(conn, "conv-a", &cursor, limit).unwrap();
        pages += 1;
        ids.extend(page.iter().map(|m| m.id.clone()));
        match next {
            Some(c) => cursor = c,
            None => return (ids, pages),
        }
        assert!(pages < 100, "the forward walk never terminated");
    }
}

#[test]
fn read_after_pages_forward_to_the_tail_without_skipping_or_repeating() {
    let conn = local_db();
    for i in 0..23 {
        insert_local_message(
            &conn,
            &format!("m{i:02}"),
            "conv-a",
            "hi",
            &format!("2024-01-01T10:{i:02}:00Z"),
        );
    }
    // Another conversation's rows must never leak into the page.
    insert_local_message(&conn, "other", "conv-b", "hi", "2024-01-01T10:05:00Z");

    let (ids, pages) = walk_forward(&conn, cursor("2024-01-01T10:04:00Z", "m04"), 5);

    // Exclusive of the cursor row, every later row exactly once, in log order
    // once each page's newest-first is unwound.
    let mut expected: Vec<String> = (5..23).map(|i| format!("m{i:02}")).collect();
    let mut got = ids.clone();
    got.sort();
    expected.sort();
    assert_eq!(got, expected);
    assert_eq!(ids.len(), 18, "no row visited twice");
    // 18 rows at 5 a page: three full pages, one of three, and — because the
    // fourth was short — no fifth call.
    assert_eq!(pages, 4);
}

#[test]
fn read_after_page_is_newest_first_and_the_cursor_is_its_newest_row() {
    let conn = local_db();
    for i in 0..4 {
        insert_local_message(&conn, &format!("m{i}"), "conv-a", "hi", &format!("2024-01-01T10:0{i}:00Z"));
    }

    let (page, next) = read_after_on(&conn, "conv-a", &cursor("2024-01-01T10:00:00Z", "m0"), 2).unwrap();

    let ids: Vec<&str> = page.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["m2", "m1"], "the two OLDEST rows after the cursor, newest-first");
    let next = next.expect("a full page carries a continuation");
    assert_eq!(next.id, "m2", "the continuation is the newest row returned");
    assert_eq!(next.sent_at, "2024-01-01T10:02:00Z");
}

#[test]
fn read_after_exactly_full_last_page_ends_on_the_next_empty_call() {
    let conn = local_db();
    for i in 0..4 {
        insert_local_message(&conn, &format!("m{i}"), "conv-a", "hi", &format!("2024-01-01T10:0{i}:00Z"));
    }

    // Two rows remain after m1 and the page size is two: the page is full, so
    // the read cannot yet know it is the tail.
    let (page, next) = read_after_on(&conn, "conv-a", &cursor("2024-01-01T10:01:00Z", "m1"), 2).unwrap();
    assert_eq!(page.len(), 2);
    let next = next.expect("a full page always continues");

    // The next call is the one that says so: empty, and no cursor.
    let (tail, none) = read_after_on(&conn, "conv-a", &next, 2).unwrap();
    assert!(tail.is_empty());
    assert!(none.is_none(), "an empty page never carries a continuation");
}

#[test]
fn read_after_breaks_sent_at_ties_on_id_like_the_backward_read() {
    let conn = local_db();
    // Three rows at ONE instant. The backward read orders `sent_at DESC, id
    // DESC`, so its cursor at `m-b` means "everything before m-b in that
    // order" — and forward from `m-b` must be exactly the complement.
    for id in ["m-a", "m-b", "m-c"] {
        insert_local_message(&conn, id, "conv-a", "hi", "2024-01-01T10:00:00Z");
    }

    let (page, _) = read_after_on(&conn, "conv-a", &cursor("2024-01-01T10:00:00Z", "m-b"), 10).unwrap();
    let ids: Vec<&str> = page.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["m-c"], "only the higher id at the same instant is 'after' the cursor");
}
