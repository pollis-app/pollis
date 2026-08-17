//! #880 — one id names exactly one kind of conversation, and the DATABASE is
//! what says so.
//!
//! `dm_channel`, `groups` and `channels` are three tables with three separate
//! primary keys, so SQLite accepts the same id in all three. `is_member` ORs
//! across all three on one id, so a DM whose id is a victim group's id grants
//! membership of that group (#879, and the audit finding behind it). #879 closed
//! that with `conversation_id_taken`, a check every `create_*` path must
//! remember to call — code discipline, ranked LAST by CLAUDE.md.
//!
//! This suite pins the durable half: the `conversation` registry, claimed in the
//! SAME transaction as the row it names, so a second claim of one id cannot
//! commit. Two kinds of test live here and they are deliberately separated:
//!
//!   * **guard tests** — the `conversation_id_taken` chokepoint, which turns a
//!     rejected claim into a clean `Forbidden` instead of a 500. Removing the
//!     guard fails exactly these.
//!   * **registry tests** — what holds when the guard is *not* consulted at all.
//!     They assert only that no row was written, which is true whether the guard
//!     refused it or the registry's primary key did. These are what make the
//!     invariant a property of the schema rather than of anyone's memory.
//!
//! The backfill tests at the bottom drive the real migration SQL against a
//! database stamped at the PREVIOUS version, which is the only way to prove what
//! happens to rows that already exist in production.
//!
//! `foreign_keys=OFF` throughout: that is what production Turso does, so a test
//! must not borrow enforcement no deploy has.

use libsql::Connection;
use pollis_delivery::db::Db;
use pollis_delivery::groups::{
    apply_create_channel, apply_create_group, CreateChannelBody, CreateGroupBody,
};
use pollis_delivery::profile::{apply_create_dm, CreateDmBody};
use pollis_delivery::writes::WriteOutcome;

const OWNER: &str = "owner-1";
const MALLORY: &str = "mallory-1";

// ── harness ──────────────────────────────────────────────────────────────────

/// A database at the CURRENT schema (baseline + every migration), the shape a
/// deployed DS talks to.
async fn fresh() -> Db {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap())
        .await
        .expect("local db");
    let conn = db.conn().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF;")
        .await
        .expect("pragma");
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES \
           ('owner-1','owner@x','owner'),('mallory-1','mallory@x','mallory');",
    )
    .await
    .expect("seed users");
    db
}

async fn count(conn: &Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.expect("count");
    rows.next()
        .await
        .expect("row")
        .expect("row")
        .get(0)
        .expect("count")
}

/// The registry's verdict for one id: `Some(kind)` or `None` when unclaimed.
async fn claimed_kind(conn: &Connection, id: &str) -> Option<String> {
    let mut rows = conn
        .query(
            "SELECT kind FROM conversation WHERE id = ?1",
            libsql::params![id.to_string()],
        )
        .await
        .expect("query");
    rows.next()
        .await
        .expect("row")
        .map(|r| r.get::<String>(0).expect("kind"))
}

fn group_body(id: &str, text: Option<&str>, voice: Option<&str>) -> CreateGroupBody {
    CreateGroupBody {
        id: id.to_string(),
        name: "A Group".to_string(),
        description: None,
        owner_id: Some(OWNER.to_string()),
        default_text_channel_id: text.map(str::to_string),
        default_voice_channel_id: voice.map(str::to_string),
        created_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

fn channel_body(id: &str, group_id: &str) -> CreateChannelBody {
    CreateChannelBody {
        id: id.to_string(),
        group_id: group_id.to_string(),
        name: "chan".to_string(),
        description: None,
        channel_type: "text".to_string(),
        creator_id: Some(OWNER.to_string()),
    }
}

fn dm_body(id: &str, creator: &str) -> CreateDmBody {
    CreateDmBody {
        id: id.to_string(),
        creator_id: creator.to_string(),
        member_ids: vec![creator.to_string()],
        created_at: "2026-01-02T00:00:00Z".to_string(),
    }
}

// ── the namespace itself ─────────────────────────────────────────────────────

/// The positive case first, so nothing below can pass by refusing everything:
/// three fresh ids, one of each kind, all created and all registered.
#[tokio::test]
async fn every_kind_of_conversation_is_still_creatable_and_lands_in_the_registry() {
    let db = fresh().await;
    let conn = db.conn().unwrap();

    let g = apply_create_group(&conn, None, &group_body("grp-1", None, None))
        .await
        .expect("group");
    let c = apply_create_channel(&conn, None, &channel_body("chan-1", "grp-1"))
        .await
        .expect("channel");
    let d = apply_create_dm(&conn, None, &dm_body("dm-1", OWNER))
        .await
        .expect("dm");

    assert!(matches!(g, WriteOutcome::Ok), "group create: {g:?}");
    assert!(matches!(c, WriteOutcome::Ok), "channel create: {c:?}");
    assert!(matches!(d, WriteOutcome::Ok), "dm create: {d:?}");

    assert_eq!(claimed_kind(&conn, "grp-1").await.as_deref(), Some("group"));
    assert_eq!(
        claimed_kind(&conn, "chan-1").await.as_deref(),
        Some("channel")
    );
    assert_eq!(claimed_kind(&conn, "dm-1").await.as_deref(), Some("dm"));
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM conversation").await, 3);
}

/// GUARD. The attack in its original form: a DM named after a victim's group.
/// Fails if `conversation_id_taken` is removed from `apply_create_dm`.
#[tokio::test]
async fn a_dm_cannot_be_created_with_an_existing_groups_id() {
    let db = fresh().await;
    let conn = db.conn().unwrap();
    apply_create_group(&conn, None, &group_body("victim-group", None, None))
        .await
        .expect("group");

    let outcome = apply_create_dm(&conn, None, &dm_body("victim-group", MALLORY))
        .await
        .expect("dm");

    assert!(
        matches!(outcome, WriteOutcome::Forbidden),
        "a DM naming an existing group must be refused, got {outcome:?}"
    );
}

/// REGISTRY. The same attack, asserted the way that survives the guard being
/// gone: whatever refuses it, no `dm_channel` row may exist afterwards and the
/// id must still name the group. This is the invariant — one id, one kind.
#[tokio::test]
async fn a_dm_naming_a_group_leaves_no_row_behind() {
    let db = fresh().await;
    let conn = db.conn().unwrap();
    apply_create_group(&conn, None, &group_body("victim-group", None, None))
        .await
        .expect("group");

    // `Err` (the registry's primary key) and `Ok(Forbidden)` (the guard) are both
    // acceptable refusals; a committed row is not.
    let _ = apply_create_dm(&conn, None, &dm_body("victim-group", MALLORY)).await;

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM dm_channel").await,
        0,
        "the DM must not exist: with the row present, is_member(victim-group, mallory) is true"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = 'victim-group'"
        )
        .await,
        0,
        "membership of the victim's group must not have been granted"
    );
    assert_eq!(
        claimed_kind(&conn, "victim-group").await.as_deref(),
        Some("group"),
        "the id must still name exactly what it named before"
    );
}

/// REGISTRY. A channel may not name an existing DM either — the attack is
/// symmetric across all three tables, not a property of DMs.
#[tokio::test]
async fn a_channel_cannot_take_an_existing_dms_id() {
    let db = fresh().await;
    let conn = db.conn().unwrap();
    apply_create_group(&conn, None, &group_body("grp-1", None, None))
        .await
        .expect("group");
    apply_create_dm(&conn, None, &dm_body("a-dm", OWNER))
        .await
        .expect("dm");

    let _ = apply_create_channel(&conn, None, &channel_body("a-dm", "grp-1")).await;

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM channels WHERE id = 'a-dm'").await,
        0,
        "a channel must not be able to name a DM"
    );
    assert_eq!(claimed_kind(&conn, "a-dm").await.as_deref(), Some("dm"));
}

/// REGISTRY, and the case the guard structurally cannot see. The registry is
/// append-only: an id survives its conversation. Here only the registry row
/// exists — no `groups`/`channels`/`dm_channel` row for that id at all — which
/// is exactly what `conversation_id_taken`'s three-table query looked for. The
/// claim must still be refused, and the group must not be half-created.
#[tokio::test]
async fn an_id_whose_conversation_is_gone_can_never_be_claimed_again() {
    let db = fresh().await;
    let conn = db.conn().unwrap();
    conn.execute(
        "INSERT INTO conversation (id, kind) VALUES ('retired-dm', 'dm')",
        (),
    )
    .await
    .expect("seed registry");

    let _ = apply_create_group(&conn, None, &group_body("retired-dm", None, None)).await;

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM groups").await,
        0,
        "a retired id must not be re-registered: members' clients may still hold \
         MLS state keyed to it"
    );
    assert_eq!(claimed_kind(&conn, "retired-dm").await.as_deref(), Some("dm"));
}

// ── the default channels created alongside a group ───────────────────────────

/// REGISTRY. The gap #879 left open: `create_group` checked its OWN id but
/// inserted `default_text_channel_id` / `default_voice_channel_id` — both taken
/// straight from the request body — with no check at all. Naming a victim's
/// group as your new group's default channel reaches `is_member` through the
/// channel leg (`channels JOIN group_member ON gm.group_id = c.group_id`) and
/// grants membership just as surely as the DM route did.
#[tokio::test]
async fn a_default_channel_cannot_name_an_existing_group() {
    let db = fresh().await;
    let conn = db.conn().unwrap();
    apply_create_group(&conn, None, &group_body("victim-group", None, None))
        .await
        .expect("group");

    let _ = apply_create_group(
        &conn,
        None,
        &group_body("mallorys-group", Some("victim-group"), None),
    )
    .await;

    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM channels WHERE id = 'victim-group'"
        )
        .await,
        0,
        "the default channel must not be able to name the victim's group"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM groups WHERE id = 'mallorys-group'"
        )
        .await,
        0,
        "and the refusal must take the whole creation with it — the group, its \
         admin membership and both channels are one transaction"
    );
}

/// REGISTRY. Same gap via the voice channel, and against an existing CHANNEL
/// rather than a group — both legs of the pair are attacker-controlled.
#[tokio::test]
async fn a_default_voice_channel_cannot_name_an_existing_channel() {
    let db = fresh().await;
    let conn = db.conn().unwrap();
    apply_create_group(&conn, None, &group_body("grp-1", Some("chan-1"), None))
        .await
        .expect("group");

    let _ = apply_create_group(
        &conn,
        None,
        &group_body("mallorys-group", None, Some("chan-1")),
    )
    .await;

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM channels WHERE id = 'chan-1'").await,
        1,
        "the victim's channel must remain the only holder of its id"
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM groups").await, 1);
}

/// REGISTRY. One request naming the same id twice — a group and its own default
/// channel, or both default channels. Nothing exists yet, so no lookup of
/// existing rows can catch it; the registry's primary key is what does.
#[tokio::test]
async fn one_request_cannot_name_the_same_id_twice() {
    let db = fresh().await;
    let conn = db.conn().unwrap();

    let _ = apply_create_group(&conn, None, &group_body("dup", Some("dup"), None)).await;
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM groups").await,
        0,
        "a group may not also be its own channel"
    );

    let _ = apply_create_group(
        &conn,
        None,
        &group_body("grp-2", Some("same"), Some("same")),
    )
    .await;
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM groups WHERE id = 'grp-2'").await,
        0,
        "the text and voice channels may not share one id"
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM conversation").await, 0);
}

/// GUARD. The same two cases, asserted as the clean 403 the endpoint should
/// return rather than a 500 from a constraint violation. Fails if the
/// default-channel checks are removed from `apply_create_group`.
#[tokio::test]
async fn a_refused_group_creation_is_forbidden_not_an_error() {
    let db = fresh().await;
    let conn = db.conn().unwrap();
    apply_create_group(&conn, None, &group_body("victim-group", None, None))
        .await
        .expect("group");

    let taken = apply_create_group(
        &conn,
        None,
        &group_body("mallorys-group", Some("victim-group"), None),
    )
    .await
    .expect("no error");
    assert!(matches!(taken, WriteOutcome::Forbidden), "got {taken:?}");

    let dup = apply_create_group(&conn, None, &group_body("dup", Some("dup"), None))
        .await
        .expect("no error");
    assert!(matches!(dup, WriteOutcome::Forbidden), "got {dup:?}");
}

// ── the backfill ─────────────────────────────────────────────────────────────

/// Every main-DB script up to but excluding #880's, i.e. the schema production
/// had the moment before this migration ran.
fn schema_before_880() -> Vec<&'static str> {
    let mut out = vec![pollis_schema::BASELINE_SQL];
    out.extend(
        pollis_schema::POST_BASELINE_MIGRATIONS
            .iter()
            .filter(|(v, _, _)| *v < CONVERSATION_NAMESPACE_VERSION)
            .map(|(_, _, sql)| *sql),
    );
    out
}

const CONVERSATION_NAMESPACE_VERSION: u32 = 16;

/// The real migration SQL, not a paraphrase of it.
fn conversation_namespace_sql() -> &'static str {
    pollis_schema::POST_BASELINE_MIGRATIONS
        .iter()
        .find(|(v, _, _)| *v == CONVERSATION_NAMESPACE_VERSION)
        .map(|(_, _, sql)| *sql)
        .expect("migration 000016 is listed")
}

/// A database stamped at the previous version and seeded with `sql`, then
/// migrated. Returns the connection so the caller can inspect the result.
async fn backfilled(seed: &str) -> Connection {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .expect("db");
    let conn = db.connect().expect("conn");
    conn.execute_batch("PRAGMA foreign_keys=OFF;")
        .await
        .expect("pragma");
    for sql in schema_before_880() {
        conn.execute_batch(sql).await.expect("pre-880 schema");
    }
    if !seed.is_empty() {
        conn.execute_batch(seed).await.expect("seed");
    }
    conn.execute_batch(conversation_namespace_sql())
        .await
        .expect("migration");
    // Leak the handle: the connection borrows the in-memory database.
    std::mem::forget(db);
    conn
}

/// The rows production actually has: a group with two channels, a group with
/// none, and a DM. Exactly one registry row each, with the right kind.
#[tokio::test]
async fn the_backfill_registers_every_existing_conversation_exactly_once() {
    let conn = backfilled(
        "INSERT INTO groups (id, name, owner_id, created_at) VALUES \
           ('g-with-channels','G1','u1','2026-01-01'),\
           ('g-empty','G2','u1','2026-01-02');\
         INSERT INTO channels (id, group_id, name, created_at) VALUES \
           ('c-text','g-with-channels','general','2026-01-01'),\
           ('c-voice','g-with-channels','voice','2026-01-01');\
         INSERT INTO dm_channel (id, created_by, created_at) VALUES \
           ('d-1','u1','2026-01-03');",
    )
    .await;

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM conversation").await,
        5,
        "one row per conversation, no more and no fewer"
    );
    assert_eq!(
        claimed_kind(&conn, "g-with-channels").await.as_deref(),
        Some("group")
    );
    assert_eq!(claimed_kind(&conn, "g-empty").await.as_deref(), Some("group"));
    assert_eq!(claimed_kind(&conn, "c-text").await.as_deref(), Some("channel"));
    assert_eq!(
        claimed_kind(&conn, "c-voice").await.as_deref(),
        Some("channel")
    );
    assert_eq!(claimed_kind(&conn, "d-1").await.as_deref(), Some("dm"));
}

/// An empty database backfills to nothing — the migration must not invent rows.
#[tokio::test]
async fn the_backfill_of_an_empty_database_is_empty() {
    let conn = backfilled("").await;
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM conversation").await, 0);
}

/// The edge case that decides whether the migration can be trusted against real
/// data: an id ALREADY shared by two tables (a pre-#879 attack). The migration
/// must not fail — a migration that aborts blocks the release for everyone — and
/// the id must go to whichever conversation had it FIRST, so a squatter never
/// inherits the victim's id.
#[tokio::test]
async fn a_pre_existing_collision_backfills_to_the_older_conversation() {
    let conn = backfilled(
        "INSERT INTO groups (id, name, owner_id, created_at) VALUES \
           ('shared','G','u1','2026-01-01');\
         INSERT INTO dm_channel (id, created_by, created_at) VALUES \
           ('shared','mallory','2026-06-01');",
    )
    .await;

    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM conversation WHERE id = 'shared'").await,
        1
    );
    assert_eq!(
        claimed_kind(&conn, "shared").await.as_deref(),
        Some("group"),
        "the original conversation keeps its id; the later squatter does not"
    );
}

/// The mirror of the above — order decides, not table precedence. If the DM came
/// first, the DM keeps the id.
#[tokio::test]
async fn a_pre_existing_collision_is_resolved_by_age_not_by_table_order() {
    let conn = backfilled(
        "INSERT INTO dm_channel (id, created_by, created_at) VALUES \
           ('shared','u1','2026-01-01');\
         INSERT INTO groups (id, name, owner_id, created_at) VALUES \
           ('shared','G','mallory','2026-06-01');",
    )
    .await;

    assert_eq!(claimed_kind(&conn, "shared").await.as_deref(), Some("dm"));
}

/// The registry's own constraint, stated directly: one id, one kind, and the
/// database is what refuses the second one.
#[tokio::test]
async fn the_registry_refuses_a_second_kind_for_one_id() {
    let conn = backfilled("").await;
    conn.execute("INSERT INTO conversation (id, kind) VALUES ('x', 'group')", ())
        .await
        .expect("first claim");

    let second = conn
        .execute("INSERT INTO conversation (id, kind) VALUES ('x', 'dm')", ())
        .await;

    assert!(second.is_err(), "a second kind for one id must be refused");
    assert_eq!(claimed_kind(&conn, "x").await.as_deref(), Some("group"));
}

/// `kind` is closed: the three legs `is_member` ORs over are the only kinds
/// there are, so a fourth cannot be smuggled in.
#[tokio::test]
async fn the_registry_refuses_an_unknown_kind() {
    let conn = backfilled("").await;
    assert!(conn
        .execute(
            "INSERT INTO conversation (id, kind) VALUES ('x', 'voice-room')",
            ()
        )
        .await
        .is_err());
}
