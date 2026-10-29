//! #908 — a seeded watermark must mean what its comment says it means.
//! `last_fetched_at` is compared lexically with RFC 3339 `sent_at`, so seeds must be RFC 3339.


use libsql::Connection;

mod common;

/// Start of the current UTC day: same `YYYY-MM-DD` as now, so the space-vs-`T` compare bites.
fn earlier_today() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists")
        .and_utc()
}

fn rfc3339(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, false)
}

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    pollis_schema::apply::single_db(&db.conn().await.expect("checkout"))
        .await
        .expect("schema");
    db
}

async fn seed_channel_with_an_envelope_from_earlier_today(conn: &Connection) -> String {
    conn.execute(
        "INSERT INTO users (id, email, username) VALUES ('alice', 'a@example.com', 'alice')",
        (),
    )
    .await
    .expect("user");
    conn.execute_batch(
        "INSERT INTO conversation (id, kind) VALUES ('g1', 'group');
         INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'alice');
         INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');
         INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'general');",
    )
    .await
    .expect("group + channel");
    conn.execute(
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'owner')",
        (),
    )
    .await
    .expect("membership");

    let sent_at = rfc3339(earlier_today());
    conn.execute(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, seq) \
         VALUES ('m-old', 'c1', 'alice', 'ct', ?1, 1)",
        libsql::params![sent_at.clone()],
    )
    .await
    .expect("envelope");
    // Advance `conversation_seq` as `insert_envelope_with_seq` does (#1087); the seed reads it.
    conn.execute(
        "INSERT INTO conversation_seq (conversation_id, next_seq) VALUES ('c1', 1)",
        (),
    )
    .await
    .expect("sequence counter");
    sent_at
}

async fn seeded_cursor(conn: &Connection) -> String {
    let mut rows = conn
        .query(
            "SELECT last_fetched_at FROM conversation_watermark \
             WHERE conversation_id = 'c1' AND device_id = 'dev-new'",
            (),
        )
        .await
        .expect("query watermark");
    rows.next()
        .await
        .expect("row result")
        .expect("seeding writes a watermark for every channel the user is in")
        .get::<String>(0)
        .expect("text cursor")
}

/// The seeded cursor must sort above an envelope sent earlier the same day.
#[tokio::test]
async fn a_seeded_cursor_outsorts_an_envelope_from_earlier_the_same_day() {
    let db = fresh_db().await;
    let conn = db.conn().await.expect("checkout");
    let envelope_sent_at = seed_channel_with_an_envelope_from_earlier_today(&conn).await;

    // register-device creates the row; cert publish seeds the watermarks (#1092).
    pollis_delivery::bootstrap::apply_register_device(&conn, "alice", "dev-new", "New")
        .await
        .expect("register device");
    pollis_delivery::bootstrap::seed_conversation_watermarks(&conn, "alice", "dev-new")
        .await
        .expect("seed watermarks");

    let cursor = seeded_cursor(&conn).await;
    assert!(
        cursor > envelope_sent_at,
        "a watermark seeded at join must sort ABOVE everything already sent — that is \
         the whole claim the seed makes. Seeded {cursor:?}, envelope {envelope_sent_at:?}"
    );

    // Negative control: what `datetime('now')` would have written loses to the envelope.
    let sqlite_shaped = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    assert!(
        sqlite_shaped < envelope_sent_at,
        "premise of the guard: a `datetime('now')` seed ({sqlite_shaped}) sorts BELOW an \
         RFC 3339 envelope from earlier the same day ({envelope_sent_at})"
    );
}

/// A registering device has consumed the backlog, so it becomes collectable.
#[tokio::test]
async fn a_freshly_registered_device_stops_pinning_the_backlog() {
    let db = fresh_db().await;
    let conn = db.conn().await.expect("checkout");
    seed_channel_with_an_envelope_from_earlier_today(&conn).await;

    pollis_delivery::bootstrap::apply_register_device(&conn, "alice", "dev-new", "New")
        .await
        .expect("register device");
    pollis_delivery::bootstrap::seed_conversation_watermarks(&conn, "alice", "dev-new")
        .await
        .expect("seed watermarks");

    pollis_delivery::messages::sweep_envelope_gc(&conn, "-12 months")
        .await
        .expect("sweep");

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = 'c1'",
            (),
        )
        .await
        .expect("count");
    let remaining: i64 = rows
        .next()
        .await
        .expect("row result")
        .expect("count row")
        .get(0)
        .expect("integer");

    assert_eq!(
        remaining, 0,
        "the only member device reported a cursor above this envelope when it \
         registered, so GC must collect it; a seed that sorts below the envelope \
         pins it instead"
    );
}

/// `datetime('now')` is right for `reported_at`, wrong for `last_fetched_at`; grep keeps them apart.
#[test]
fn no_seed_writes_a_sqlite_datetime_into_the_cursor_column() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    let mut stack = vec![root.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let raw = std::fs::read_to_string(&path).expect("read source");
            let rel = path.strip_prefix(root).expect("under manifest").to_path_buf();
            // Code only: comments explaining the rule contain every trigger word (#1087).
            let text: String = raw
                .lines()
                .map(|l| match l.find("//") {
                    Some(i) => &l[..i],
                    None => l,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            for stmt in flat.split("INSERT ").skip(1) {
                if !stmt.contains("conversation_watermark") {
                    continue;
                }
                let Some((columns, values)) = stmt.split_once(')') else {
                    continue;
                };
                if !columns.contains("last_fetched_at") {
                    continue;
                }
                let cursor_region = values.split("reported_at").next().unwrap_or(values);
                let head = cursor_region.split("ON CONFLICT").next().unwrap_or(cursor_region);
                let cursor_slot = head.split("datetime('now')").collect::<Vec<_>>();
                if cursor_slot.len() > 2 {
                    offenders.push(format!(
                        "{}: an INSERT into conversation_watermark puts datetime('now') in \
                         the cursor slot",
                        rel.display()
                    ));
                }
            }
            // A Rust-side seed bypassing the chokepoint; gated on INSERTs, since readers name the column too (#987).
            let inserts_watermarks = flat
                .split("INSERT ")
                .skip(1)
                .any(|stmt| stmt.contains("conversation_watermark"));
            if rel != std::path::Path::new("src/messages.rs")
                && inserts_watermarks
                && text.contains("last_fetched_at")
                && !text.contains("seeded_watermark_cursor")
                && !text.contains("body.last_fetched_at")
            {
                offenders.push(format!(
                    "{}: seeds conversation_watermark without going through \
                     `messages::seeded_watermark_cursor`",
                    rel.display()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "`last_fetched_at` is compared lexically against RFC 3339 `sent_at`; SQLite's \
         `datetime('now')` sorts below all of it (#908). Use \
         `messages::seeded_watermark_cursor`:\n  {}",
        offenders.join("\n  ")
    );
}
