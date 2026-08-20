//! #908 — a seeded watermark must mean what its comment says it means.
//!
//! `conversation_watermark.last_fetched_at` is a **message cursor**, compared
//! **lexically** against `message_envelope.sent_at`: envelope GC deletes rows
//! below `MIN(last_fetched_at)` over the member devices, and ingest fetches
//! `sent_at > last_fetched_at`. Every other writer of that column puts RFC 3339
//! in it — clients via `pollis_core::commands::messages::envelope_sent_at`, the
//! DS via `messages::now_rfc3339`.
//!
//! The three server-side *seed* paths did not. They wrote SQLite's
//! `datetime('now')`, i.e. `YYYY-MM-DD HH:MM:SS`, and a space (0x20) sorts below
//! a `T` (0x54). So for any envelope sharing the seed's calendar day the seeded
//! cursor compared as **older** — the exact opposite of the "this device has
//! already consumed everything up to now" the seed exists to express.
//!
//! It was fail-safe (over-pin, under-collect: envelopes were kept, never lost),
//! which is why it went unnoticed. The tests below are about the seed doing its
//! job, not about damage.
//!
//! The pre-existing GC unit tests could not have caught this: they seed BOTH
//! sides of the comparison with `datetime('now', …)`, so they are internally
//! consistent in a format production never mixes. These drive a REAL seed path
//! against a REAL envelope stamp.


use libsql::Connection;

mod common;

/// A day is the resolution at which the defect bites: the space-vs-`T`
/// comparison only decides anything once the `YYYY-MM-DD` prefix matches, so an
/// envelope from a *previous* day sorts below the seed either way and proves
/// nothing.
///
/// Hence the **start of the current UTC day** rather than a relative offset like
/// "an hour ago": an offset straddles midnight, and a test that silently stops
/// exercising the defect for one hour in every twenty-four is worse than no test.
/// This instant is always earlier than now and always shares now's `YYYY-MM-DD`.
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

/// One user, one group, one channel, and one envelope sent at the start of the
/// current UTC day — earlier than, and on the same day as, the registration below.
async fn seed_channel_with_an_envelope_from_earlier_today(conn: &Connection) -> String {
    conn.execute(
        "INSERT INTO users (id, email, username) VALUES ('alice', 'a@example.com', 'alice')",
        (),
    )
    .await
    .expect("user");
    conn.execute(
        "INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'alice')",
        (),
    )
    .await
    .expect("group");
    conn.execute(
        "INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'general')",
        (),
    )
    .await
    .expect("channel");
    conn.execute(
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'owner')",
        (),
    )
    .await
    .expect("membership");

    let sent_at = rfc3339(earlier_today());
    conn.execute(
        "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) \
         VALUES ('m-old', 'c1', 'alice', 'ct', ?1)",
        libsql::params![sent_at.clone()],
    )
    .await
    .expect("envelope");
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
        .expect("register_device seeds a watermark for every channel the user is in")
        .get::<String>(0)
        .expect("text cursor")
}

/// The defect at the value level, through the real `POST /v1/auth/register-device`
/// implementation: the cursor it seeds must sort ABOVE an envelope sent earlier
/// the same day. Against `datetime('now')` it sorts below, because
/// `"2026-08-17 09:00:00" < "2026-08-17T08:00:00.0+00:00"`.
#[tokio::test]
async fn a_seeded_cursor_outsorts_an_envelope_from_earlier_the_same_day() {
    let db = fresh_db().await;
    let conn = db.conn().await.expect("checkout");
    let envelope_sent_at = seed_channel_with_an_envelope_from_earlier_today(&conn).await;

    pollis_delivery::bootstrap::apply_register_device(&conn, "alice", "dev-new", "New")
        .await
        .expect("register device");

    let cursor = seeded_cursor(&conn).await;
    assert!(
        cursor > envelope_sent_at,
        "a watermark seeded at join must sort ABOVE everything already sent — that is \
         the whole claim the seed makes. Seeded {cursor:?}, envelope {envelope_sent_at:?}"
    );

    // The negative control, so this is about the ORDERING RULE and not about two
    // strings happening to differ: what `datetime('now')` would have written for
    // this very instant. It loses to the envelope, which is the defect.
    let sqlite_shaped = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    assert!(
        sqlite_shaped < envelope_sent_at,
        "premise of the guard: a `datetime('now')` seed ({sqlite_shaped}) sorts BELOW an \
         RFC 3339 envelope from earlier the same day ({envelope_sent_at})"
    );
}

/// …and what that value is FOR: the envelope-GC gate. A device that registers
/// into a conversation is declaring it has consumed the backlog, so the backlog
/// stops being pinned on its behalf and becomes collectable. With the
/// `datetime('now')` seed the sweep kept it forever.
#[tokio::test]
async fn a_freshly_registered_device_stops_pinning_the_backlog() {
    let db = fresh_db().await;
    let conn = db.conn().await.expect("checkout");
    seed_channel_with_an_envelope_from_earlier_today(&conn).await;

    pollis_delivery::bootstrap::apply_register_device(&conn, "alice", "dev-new", "New")
        .await
        .expect("register device");

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

/// The chokepoint stays a chokepoint. `datetime('now')` is correct for
/// `reported_at` (compared against `datetime('now', ?)`) and wrong for
/// `last_fetched_at` (compared against RFC 3339 `sent_at`), and the two sit side
/// by side in the same five INSERTs — which is exactly how the wrong one gets
/// copied into a sixth. A grep, because no type distinguishes two TEXT columns.
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
            let text = std::fs::read_to_string(&path).expect("read source");
            let rel = path.strip_prefix(root).expect("under manifest").to_path_buf();
            // Statements are written across several lines, so normalise the file
            // to one line and look at each INSERT into the table as a whole.
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            for stmt in flat.split("INSERT ").skip(1) {
                if !stmt.contains("conversation_watermark") {
                    continue;
                }
                // The column list ends at the closing paren; the values follow.
                let Some((columns, values)) = stmt.split_once(')') else {
                    continue;
                };
                if !columns.contains("last_fetched_at") {
                    continue;
                }
                // `datetime('now'` anywhere before `reported_at`'s value means the
                // cursor is being written in SQLite's format.
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
            // The direct form: a Rust-side seed that stopped using the chokepoint.
            //
            // Gated on the file actually INSERTing into the table (#987). Since
            // the read migration a module can name both `conversation_watermark`
            // and `last_fetched_at` while only ever SELECTing them — the
            // envelope fetch correlates on the watermark to answer "strictly past
            // THAT conversation's cursor". A reader has no cursor to format, so
            // flagging it would make this tripwire cry wolf, which is how a
            // tripwire stops being read.
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
