use rusqlite::Connection;

// #874: `read_last_messages` previews, against the local schema.

use super::read::last_messages_on;

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
    let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    // (conversation_id, id, content)
    last_messages_on(conn, &ids)
        .unwrap()
        .into_iter()
        .map(|m| (m.conversation_id, m.id, m.content.unwrap_or_default()))
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

    // Absence is the answer for an empty conversation.
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "conv-a");
}

#[test]
fn last_messages_breaks_sent_at_ties_the_same_way_the_page_read_does() {
    let conn = local_db();
    // Identical timestamps: the higher id wins, matching the page read.
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
fn last_messages_covers_every_requested_conversation() {
    let conn = local_db();
    let ids: Vec<String> = (0..25).map(|i| format!("conv-{i:02}")).collect();
    for (i, cid) in ids.iter().enumerate() {
        insert_local_message(&conn, &format!("m{i}"), cid, "hi", "2024-01-01T10:00:00Z");
    }

    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let rows = last_messages(&conn, &refs);

    // 25 conversations, one row each, in the order asked.
    assert_eq!(rows.len(), 25);
    assert!(rows.iter().zip(&ids).all(|(r, id)| &r.0 == id));
}

// #1039: `read_messages_after` must visit every row once across pages, then stop.

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

// #2171 / #2172: the indexed unread, newest-message and thread-summary reads return exactly
// what the shipped queries returned, on a DB with every edge case those reads must get right.
mod index_equivalence {
    use super::local_db;
    use crate::commands::messages::read::{
        last_messages_on, thread_summaries_on, LAST_MESSAGE_SQL, THREAD_SUMMARIES_SQL,
    };
    use crate::commands::messages::read_state::{unread_counts, UNREAD_COUNTS_SQL};
    use rusqlite::types::Value;
    use rusqlite::Connection;

    const ME: &str = "u-me";

    /// The shipped `unread_counts` SQL before #2171, kept as the oracle.
    const OLD_UNREAD_SQL: &str = "SELECT m.conversation_id, COUNT(*)
         FROM message m
         LEFT JOIN read_cursor c ON c.conversation_id = m.conversation_id
         WHERE m.sender_id != ?1
           AND m.deleted_at IS NULL
           AND (
                 c.conversation_id IS NULL
                 OR (m.sent_at, m.id) > (c.last_read_at, c.last_read_message_id)
               )
         GROUP BY m.conversation_id
         HAVING COUNT(*) > 0
         ORDER BY m.conversation_id";

    const COLS: &str = "id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at, thread_id, undecryptable, send_state, link_preview";

    /// The shipped `read_last_messages` window before #2171, kept as the oracle.
    fn old_last_messages_sql(n: usize) -> String {
        let ph = (1..=n).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
        format!(
            "SELECT {COLS} FROM (
               SELECT {COLS}, ROW_NUMBER() OVER (PARTITION BY conversation_id ORDER BY sent_at DESC, id DESC) AS rn
               FROM message WHERE conversation_id IN ({ph})
             ) WHERE rn = 1"
        )
    }

    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    fn put(
        conn: &Connection,
        id: &str,
        conv: &str,
        sender: &str,
        sent_at: &str,
        deleted: bool,
        thread_id: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, thread_id, sent_at, deleted_at)
             VALUES (?1, ?2, ?3, X'00', ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                conv,
                sender,
                if deleted { None } else { Some(format!("text {id}")) },
                thread_id,
                sent_at,
                if deleted { Some("2026-02-01T00:00:00Z") } else { None },
            ],
        )
        .unwrap();
    }

    fn cursor(conn: &Connection, conv: &str, at: &str, id: &str) {
        conn.execute(
            "INSERT OR REPLACE INTO read_cursor (conversation_id, last_read_at, last_read_message_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![conv, at, id],
        )
        .unwrap();
    }

    fn at(sec: u64) -> String {
        format!("2026-01-01T00:{:02}:{:02}Z", sec / 60, sec % 60)
    }

    /// Every case the three reads must get right, then a bulk of random rows whose timestamps
    /// come from a small range so `sent_at` ties are common.
    fn seed(conn: &Connection) {
        // No cursor: everything from others counts, own and deleted rows do not.
        put(conn, "a1", "c-nocursor", "u-them", &at(1), false, None);
        put(conn, "a2", "c-nocursor", ME, &at(2), false, None);
        put(conn, "a3", "c-nocursor", "u-them", &at(3), true, None);
        // Only own messages, with and without a cursor.
        put(conn, "b1", "c-own", ME, &at(1), false, None);
        put(conn, "b2", "c-own", ME, &at(2), false, None);
        put(conn, "b3", "c-own-read", ME, &at(1), false, None);
        cursor(conn, "c-own-read", &at(0), "b0");
        // Only deleted messages from others.
        put(conn, "d1", "c-deleted", "u-them", &at(1), true, None);
        put(conn, "d2", "c-deleted", "u-them", &at(2), true, None);
        // Cursor on the newest message.
        put(conn, "e1", "c-read", "u-them", &at(1), false, None);
        put(conn, "e2", "c-read", "u-them", &at(2), false, None);
        cursor(conn, "c-read", &at(2), "e2");
        // Ties on `sent_at`, cursor inside the tie: only higher ids are past it.
        for id in ["t1", "t2", "t3", "t4"] {
            put(conn, id, "c-ties", "u-them", &at(5), false, None);
        }
        put(conn, "t0", "c-ties", "u-them", &at(4), false, None);
        cursor(conn, "c-ties", &at(5), "t2");
        // A cursor on a row this device no longer holds.
        put(conn, "g1", "c-gone", "u-them", &at(1), false, None);
        put(conn, "g3", "c-gone", "u-them", &at(3), false, None);
        cursor(conn, "c-gone", &at(2), "g2");
        // A cursor for a conversation with no messages, and a conversation whose newest row is
        // deleted (its preview still shows that row).
        cursor(conn, "c-empty", &at(1), "x");
        put(conn, "h1", "c-newest-deleted", "u-them", &at(1), false, None);
        put(conn, "h2", "c-newest-deleted", "u-them", &at(2), true, None);
        // Threads: a root with live and deleted replies, a root whose replies are all deleted,
        // replies tied on `sent_at`, own replies, and a reply whose root this device lacks.
        put(conn, "r1", "c-threads", "u-them", &at(1), false, None);
        put(conn, "r2", "c-threads", ME, &at(2), false, None);
        put(conn, "r1a", "c-threads", "u-them", &at(3), false, Some("r1"));
        put(conn, "r1b", "c-threads", ME, &at(4), false, Some("r1"));
        put(conn, "r1c", "c-threads", "u-them", &at(4), false, Some("r1"));
        put(conn, "r1d", "c-threads", "u-them", &at(9), true, Some("r1"));
        put(conn, "r2a", "c-threads", "u-them", &at(5), true, Some("r2"));
        put(conn, "r2b", "c-threads", "u-them", &at(6), true, Some("r2"));
        put(conn, "r3a", "c-threads", "u-them", &at(7), false, Some("root-not-held"));
        // The same thread id in another conversation stays separate.
        put(conn, "s1", "c-threads-2", "u-them", &at(7), false, Some("r1"));

        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for c in 0..24u64 {
            let conv = format!("c-rand-{c:02}");
            let n = rng.below(150);
            let mut ids: Vec<(String, String)> = Vec::new();
            for i in 0..n {
                let id = format!("{conv}-m{:06}", rng.below(1000) * 1000 + i);
                let sent = at(rng.below(40));
                let sender = match rng.below(100) {
                    0..=14 => ME.to_string(),
                    k => format!("u-{}", k % 5),
                };
                let thread = if !ids.is_empty() && rng.below(100) < 10 {
                    Some(ids[rng.below(ids.len() as u64) as usize].0.clone())
                } else {
                    None
                };
                put(conn, &id, &conv, &sender, &sent, rng.below(100) < 5, thread.as_deref());
                ids.push((id, sent));
            }
            match rng.below(4) {
                // A cursor on a held row (often inside a tie).
                0 | 1 if !ids.is_empty() => {
                    let (id, sent) = &ids[rng.below(ids.len() as u64) as usize];
                    cursor(conn, &conv, sent, id);
                }
                // A cursor between rows.
                2 => cursor(conn, &conv, &at(rng.below(40)), "zz-not-held"),
                _ => {}
            }
        }
    }

    fn convs(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT DISTINCT conversation_id FROM message ORDER BY 1")
            .unwrap();
        let mut ids: Vec<String> =
            stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        ids.push("c-empty".into());
        ids.push("c-never-seen".into());
        ids
    }

    fn raw_rows(conn: &Connection, sql: &str, params: &[String]) -> Vec<Vec<Value>> {
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            (0..13).map(|i| r.get::<_, Value>(i)).collect()
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    fn text(v: &Value) -> String {
        match v {
            Value::Text(s) => s.clone(),
            other => panic!("not text: {other:?}"),
        }
    }

    fn assert_same_results(conn: &Connection) {
        // Unread counts.
        let old: Vec<(String, i64)> = conn
            .prepare(OLD_UNREAD_SQL)
            .unwrap()
            .query_map([ME], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let new: Vec<(String, i64)> = unread_counts(conn, ME)
            .unwrap()
            .into_iter()
            .map(|u| (u.conversation_id, u.count))
            .collect();
        assert_eq!(new, old, "unread counts differ from the shipped query");

        // Newest message per conversation: every column, row for row.
        let ids = convs(conn);
        let mut old_rows = raw_rows(conn, &old_last_messages_sql(ids.len()), &ids);
        let mut new_rows: Vec<Vec<Value>> = Vec::new();
        for id in &ids {
            new_rows.extend(raw_rows(conn, LAST_MESSAGE_SQL, std::slice::from_ref(id)));
        }
        old_rows.sort_by_key(|r| text(&r[1]));
        new_rows.sort_by_key(|r| text(&r[1]));
        assert_eq!(new_rows, old_rows, "newest rows differ from the shipped window");
        // And what the command hands back: in the order asked, empty conversations absent.
        let got: Vec<(String, String)> = last_messages_on(conn, &ids)
            .unwrap()
            .into_iter()
            .map(|m| (m.conversation_id, m.id))
            .collect();
        let want: Vec<(String, String)> = ids
            .iter()
            .filter_map(|c| old_rows.iter().find(|r| &text(&r[1]) == c))
            .map(|r| (text(&r[1]), text(&r[0])))
            .collect();
        assert_eq!(got, want);

        // Thread summaries: against a plain table scan, which no index can bend.
        let scan = THREAD_SUMMARIES_SQL.replace("FROM message", "FROM message NOT INDEXED");
        for conv in &ids {
            let mut old: Vec<(String, i64, Option<String>)> = conn
                .prepare(&scan)
                .unwrap()
                .query_map([conv], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            let mut new: Vec<(String, i64, Option<String>)> = thread_summaries_on(conn, conv)
                .unwrap()
                .into_iter()
                .map(|t| (t.thread_id, t.reply_count, t.last_reply_at))
                .collect();
            old.sort();
            new.sort();
            assert_eq!(new, old, "thread summaries differ in {conv}");
        }
    }

    #[test]
    fn indexed_reads_return_what_the_shipped_queries_returned() {
        let conn = local_db();
        seed(&conn);
        assert_same_results(&conn);

        // The fixtures hold what they claim, so the equality above is not vacuous.
        let unread: Vec<(String, i64)> = unread_counts(&conn, ME)
            .unwrap()
            .into_iter()
            .map(|u| (u.conversation_id, u.count))
            .collect();
        assert!(unread.contains(&("c-nocursor".into(), 1)), "{unread:?}");
        assert!(unread.contains(&("c-ties".into(), 2)), "{unread:?}");
        assert!(unread.contains(&("c-gone".into(), 1)), "{unread:?}");
        for absent in ["c-own", "c-own-read", "c-deleted", "c-read", "c-empty"] {
            assert!(!unread.iter().any(|(c, _)| c == absent), "{absent} badged: {unread:?}");
        }
        assert!(unread.iter().filter(|(c, _)| c.starts_with("c-rand")).count() > 5, "{unread:?}");

        let ids: Vec<String> = ["c-ties", "c-empty", "c-newest-deleted"].map(String::from).to_vec();
        let last: Vec<(String, String)> = last_messages_on(&conn, &ids)
            .unwrap()
            .into_iter()
            .map(|m| (m.conversation_id, m.id))
            .collect();
        assert_eq!(
            last,
            [("c-ties".into(), "t4".into()), ("c-newest-deleted".into(), "h2".into())]
        );

        let threads: Vec<(String, i64, Option<String>)> = thread_summaries_on(&conn, "c-threads")
            .unwrap()
            .into_iter()
            .map(|t| (t.thread_id, t.reply_count, t.last_reply_at))
            .collect();
        assert!(threads.contains(&("r1".into(), 3, Some(at(4)))), "{threads:?}");
        assert!(threads.contains(&("root-not-held".into(), 1, Some(at(7)))), "{threads:?}");
        assert!(!threads.iter().any(|t| t.0 == "r2"), "an all-deleted thread is listed: {threads:?}");
    }

    #[test]
    fn an_empty_database_has_no_unread_previews_or_threads() {
        let conn = local_db();
        assert!(unread_counts(&conn, ME).unwrap().is_empty());
        assert!(last_messages_on(&conn, &["c1".to_string()]).unwrap().is_empty());
        assert!(thread_summaries_on(&conn, "c1").unwrap().is_empty());
        assert_same_results(&conn);
    }

    fn index_names(conn: &Connection) -> Vec<String> {
        conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'message' ORDER BY 1",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    /// An upgrade swaps the old conversation index for the wide one; a downgrade (an older build
    /// opening the file) brings the old one back beside it, and the next upgrade drops it again.
    /// Results are the same at every step.
    #[test]
    fn the_index_swap_survives_upgrade_and_downgrade() {
        let conn = local_db();
        let names = index_names(&conn);
        assert!(names.iter().any(|n| n == "idx_message_conversation_order"), "{names:?}");
        assert!(names.iter().any(|n| n == "idx_message_conversation_thread"), "{names:?}");
        assert!(!names.iter().any(|n| n == "idx_message_conversation"), "{names:?}");
        seed(&conn);

        // What an older build's schema does on open.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_message_conversation ON message(conversation_id, sent_at);",
        )
        .unwrap();
        assert_same_results(&conn);

        crate::db::local::apply_local_schema(&conn).unwrap();
        let names = index_names(&conn);
        assert!(!names.iter().any(|n| n == "idx_message_conversation"), "{names:?}");
        assert!(names.iter().any(|n| n == "idx_message_conversation_order"), "{names:?}");
        assert_same_results(&conn);
    }

    fn plan(conn: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let params: Vec<String> = (0..stmt.parameter_count()).map(|_| "x".to_string()).collect();
        stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| r.get::<_, String>(3))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// Every read of `message` in the three queries goes through an index holding every column it
    /// needs: no table scan, no sort step, no table lookup per row.
    #[test]
    fn the_reads_are_answered_from_the_indexes() {
        let conn = local_db();
        seed(&conn);

        let unread = plan(&conn, UNREAD_COUNTS_SQL);
        let reads: Vec<&String> = unread
            .iter()
            .filter(|s| {
                s.starts_with("SCAN m") || s.starts_with("SEARCH m") || s.contains(" message")
            })
            .collect();
        // The loose scan's two lookups, then the count in each `CASE` arm.
        assert!(reads.len() >= 4, "{unread:?}");
        for step in &reads {
            assert!(
                step.starts_with("SEARCH")
                    && step.contains("COVERING INDEX idx_message_conversation_order"),
                "unread reads `message` without a covering index range: {step}\n{unread:?}"
            );
        }
        // The only sort is the final ORDER BY, over one row per conversation.
        let sorts: Vec<&String> = unread.iter().filter(|s| s.contains("TEMP B-TREE")).collect();
        assert!(
            sorts.iter().all(|s| s.as_str() == "USE TEMP B-TREE FOR ORDER BY") && sorts.len() <= 1,
            "{unread:?}"
        );

        let last = plan(&conn, LAST_MESSAGE_SQL);
        assert!(last.iter().any(|s| s.contains("idx_message_conversation_order")), "{last:?}");
        assert!(!last.iter().any(|s| s.contains("TEMP B-TREE")), "the index gives the order: {last:?}");

        let threads = plan(&conn, THREAD_SUMMARIES_SQL);
        assert!(
            threads.iter().any(|s| s.contains("COVERING INDEX idx_message_conversation_thread")),
            "{threads:?}"
        );
        assert!(!threads.iter().any(|s| s.contains("TEMP B-TREE")), "{threads:?}");
    }
}
