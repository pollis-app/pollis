//! Channel categories and channel order (#2031): who may arrange a group, that a
//! category delete never takes a channel with it, that two admins dragging at
//! once cannot produce a duplicate or missing position, and that the DS returns
//! channels in display order for a client that knows nothing of categories.

use libsql::Connection;
use pollis_api::directory::{ChannelWire, GroupWithChannelsWire};
use pollis_api::groups::{
    CreateChannelBody, CreateChannelCategoryBody, DeleteChannelCategoryBody, DeleteGroupBody,
    MoveChannelBody, MoveChannelCategoryBody, RenameChannelCategoryBody,
};
use pollis_delivery::channel_order::{
    apply_create_category, apply_delete_category, apply_move_category, apply_move_channel,
    apply_rename_category,
};
use pollis_delivery::directory::{categories_of, groups_with_channels};
use pollis_delivery::groups::{apply_create_channel, apply_delete_group};
use pollis_delivery::writes::WriteOutcome;

mod common;

const SEED: &[&str] = &[
    "INSERT INTO users (id, email, username, account_id_pub) VALUES ('alice', 'a@x.com', 'alice', X'01')",
    "INSERT INTO users (id, email, username, account_id_pub) VALUES ('bob', 'b@x.com', 'bob', X'01')",
    "INSERT INTO users (id, email, username, account_id_pub) VALUES ('carol', 'c@x.com', 'carol', X'01')",
    "INSERT INTO users (id, email, username, account_id_pub) VALUES ('dave', 'd@x.com', 'dave', X'01')",
    "INSERT INTO conversation (id, kind) VALUES ('g1', 'group')",
    "INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'One', 'alice')",
    "INSERT INTO conversation (id, kind) VALUES ('g2', 'group')",
    "INSERT INTO groups (id, name, owner_id) VALUES ('g2', 'Two', 'carol')",
    // alice and dave administer g1, bob is a plain member; carol administers g2 only.
    "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')",
    "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'dave', 'admin')",
    "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')",
    "INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'carol', 'admin')",
    "INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'bob', 'member')",
];

async fn seed(c: &Connection) {
    pollis_schema::apply::single_db(c).await.unwrap();
    for sql in SEED {
        c.execute(sql, ()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    // Pre-#2031 channels: no category, no position, distinct creation times.
    for (id, group, at) in [
        ("c1", "g1", "2026-01-01 00:00:01"),
        ("c2", "g1", "2026-01-01 00:00:02"),
        ("c3", "g1", "2026-01-01 00:00:03"),
        ("c4", "g1", "2026-01-01 00:00:04"),
        ("x1", "g2", "2026-01-01 00:00:05"),
    ] {
        c.execute(
            "INSERT INTO conversation (id, kind) VALUES (?1, 'channel')",
            libsql::params![id],
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO channels (id, group_id, name, channel_type, created_at) VALUES (?1, ?2, ?1, 'text', ?3)",
            libsql::params![id, group, at],
        )
        .await
        .unwrap();
    }
}

async fn conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    // Production parity: Turso runs with foreign keys off.
    c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
    seed(&c).await;
    c
}

fn ok(o: WriteOutcome) -> bool {
    matches!(o, WriteOutcome::Ok)
}

fn forbidden(o: WriteOutcome) -> bool {
    matches!(o, WriteOutcome::Forbidden)
}

fn invalid(o: WriteOutcome) -> bool {
    matches!(o, WriteOutcome::Invalid(_))
}

async fn create_cat(c: &Connection, actor: &str, group: &str, id: &str, name: &str) -> WriteOutcome {
    let body = CreateChannelCategoryBody {
        id: id.into(),
        group_id: group.into(),
        name: name.into(),
        requester_id: None,
    };
    apply_create_category(c, Some(actor), &body).await.unwrap()
}

async fn rename_cat(c: &Connection, actor: &str, group: &str, id: &str, name: &str) -> WriteOutcome {
    let body = RenameChannelCategoryBody {
        category_id: id.into(),
        group_id: group.into(),
        name: name.into(),
        requester_id: None,
    };
    apply_rename_category(c, Some(actor), &body).await.unwrap()
}

async fn delete_cat(c: &Connection, actor: &str, group: &str, id: &str) -> WriteOutcome {
    let body = DeleteChannelCategoryBody {
        category_id: id.into(),
        group_id: group.into(),
        requester_id: None,
    };
    apply_delete_category(c, Some(actor), &body).await.unwrap()
}

async fn move_cat(c: &Connection, actor: &str, group: &str, id: &str, position: u32) -> WriteOutcome {
    let body = MoveChannelCategoryBody {
        category_id: id.into(),
        group_id: group.into(),
        position,
        requester_id: None,
    };
    apply_move_category(c, Some(actor), &body).await.unwrap()
}

fn move_body(group: &str, channel: &str, category: Option<&str>, position: u32) -> MoveChannelBody {
    MoveChannelBody {
        channel_id: channel.into(),
        group_id: group.into(),
        category_id: category.map(str::to_string),
        position,
        requester_id: None,
    }
}

async fn move_ch(
    c: &Connection,
    actor: &str,
    group: &str,
    channel: &str,
    category: Option<&str>,
    position: u32,
) -> WriteOutcome {
    apply_move_channel(c, Some(actor), &move_body(group, channel, category, position))
        .await
        .unwrap()
}

async fn create_ch(c: &Connection, actor: &str, group: &str, id: &str, category: Option<&str>) -> WriteOutcome {
    let body = CreateChannelBody {
        id: id.into(),
        group_id: group.into(),
        name: id.into(),
        description: None,
        channel_type: "text".into(),
        creator_id: None,
        category_id: category.map(str::to_string),
    };
    apply_create_channel(c, Some(actor), &body).await.unwrap()
}

/// The group as `user` reads it at launch.
async fn group(c: &Connection, user: &str, group_id: &str) -> GroupWithChannelsWire {
    groups_with_channels(c, user)
        .await
        .unwrap()
        .into_iter()
        .find(|g| g.group.id == group_id)
        .expect("the reader is a member")
}

/// `[(category or "-", [channel ids])]` in display order, uncategorised first.
async fn layout(c: &Connection, group_id: &str) -> Vec<(String, Vec<String>)> {
    let g = group(c, "alice", group_id).await;
    blocks(&g)
}

fn blocks(g: &GroupWithChannelsWire) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = vec![("-".into(), Vec::new())];
    for cat in &g.categories {
        out.push((cat.id.clone(), Vec::new()));
    }
    for ch in &g.channels {
        let key = ch.category_id.clone().unwrap_or_else(|| "-".into());
        out.iter_mut()
            .find(|(k, _)| *k == key)
            .expect("a channel names a listed category")
            .1
            .push(ch.id.clone());
    }
    out
}

fn ids(chs: &[ChannelWire]) -> Vec<&str> {
    chs.iter().map(|c| c.id.as_str()).collect()
}

fn b(key: &str, chs: &[&str]) -> (String, Vec<String>) {
    (key.to_string(), chs.iter().map(|s| s.to_string()).collect())
}

async fn count(c: &Connection, sql: &str) -> i64 {
    let mut r = c.query(sql, ()).await.unwrap();
    r.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// Every block of `group_id` numbered 0..n with no gap and no duplicate.
async fn assert_positions_contiguous(c: &Connection, group_id: &str) {
    let mut r = c
        .query(
            "SELECT COALESCE(category_id, ''), position FROM channels WHERE group_id = ?1 \
             ORDER BY COALESCE(category_id, ''), position",
            libsql::params![group_id],
        )
        .await
        .unwrap();
    let mut per_block: std::collections::BTreeMap<String, Vec<Option<i64>>> = Default::default();
    while let Some(row) = r.next().await.unwrap() {
        per_block
            .entry(row.get::<String>(0).unwrap())
            .or_default()
            .push(row.get::<Option<i64>>(1).unwrap());
    }
    for (block, positions) in per_block {
        if positions.iter().all(Option::is_none) {
            continue;
        }
        let want: Vec<Option<i64>> = (0..positions.len() as i64).map(Some).collect();
        assert_eq!(positions, want, "block {block:?} is not numbered 0..n");
    }
}

// ── Reads ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_group_nobody_arranged_lists_by_creation_time() {
    let c = conn().await;
    // A rename that would change an alphabetical order changes nothing here.
    c.execute("UPDATE channels SET name = 'aaa' WHERE id = 'c4'", ()).await.unwrap();
    let g = group(&c, "bob", "g1").await;
    assert_eq!(ids(&g.channels), ["c1", "c2", "c3", "c4"]);
    assert!(g.categories.is_empty());
    assert!(g.channels.iter().all(|ch| ch.category_id.is_none()));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM channels WHERE position IS NOT NULL").await, 0);
}

#[tokio::test]
async fn channels_come_back_in_display_order_for_an_old_client() {
    let c = conn().await;
    assert!(ok(create_cat(&c, "alice", "g1", "k1", "Campaign").await));
    assert!(ok(create_cat(&c, "alice", "g1", "k2", "Voice").await));
    assert!(ok(move_ch(&c, "alice", "g1", "c1", Some("k2"), 0).await));
    assert!(ok(move_ch(&c, "alice", "g1", "c2", Some("k1"), 0).await));
    assert!(ok(move_ch(&c, "alice", "g1", "c4", None, 0).await));

    let g = group(&c, "bob", "g1").await;
    // Uncategorised first, then each category in category order.
    assert_eq!(ids(&g.channels), ["c4", "c3", "c2", "c1"]);

    // A client that predates categories reads the same flat order: its wire
    // types have neither field, and serde ignores what it doesn't know.
    #[derive(serde::Deserialize)]
    struct OldChannel {
        id: String,
    }
    #[derive(serde::Deserialize)]
    struct OldGroup {
        channels: Vec<OldChannel>,
    }
    let old: OldGroup = serde_json::from_value(serde_json::to_value(&g).unwrap()).unwrap();
    let flat: Vec<String> = old.channels.into_iter().map(|c| c.id).collect();
    assert_eq!(flat, ["c4", "c3", "c2", "c1"]);

    // And the group read (mobile's channel list) agrees.
    let cats = categories_of(&c, "g1").await.unwrap();
    assert_eq!(cats.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["k1", "k2"]);
}

#[tokio::test]
async fn a_new_client_reads_an_old_ds_answer() {
    // A DS that predates categories sends neither field.
    let ch: ChannelWire = serde_json::from_value(serde_json::json!({
        "id": "c", "group_id": "g", "name": "n", "channel_type": "text"
    }))
    .unwrap();
    assert_eq!(ch.category_id, None);
    let g: GroupWithChannelsWire = serde_json::from_value(serde_json::json!({
        "id": "g", "name": "n", "owner_id": "o", "created_at": "t", "channels": [], "role": "member"
    }))
    .unwrap();
    assert!(g.categories.is_empty());
}

#[tokio::test]
async fn categories_are_read_by_members_only() {
    let c = conn().await;
    assert!(ok(create_cat(&c, "carol", "g2", "k9", "Secret plans").await));
    // alice is not in g2: the launch read carries no g2 and no g2 category.
    let mine = groups_with_channels(&c, "alice").await.unwrap();
    assert!(mine.iter().all(|g| g.group.id != "g2"));
    assert!(mine.iter().all(|g| g.categories.iter().all(|k| k.group_id != "g2")));
    // bob is, and sees it.
    assert_eq!(group(&c, "bob", "g2").await.categories.len(), 1);
}

// ── Create, rename, delete, reorder: who may ───────────────────────────────

#[tokio::test]
async fn only_an_admin_of_the_group_creates_a_category() {
    let c = conn().await;
    assert!(forbidden(create_cat(&c, "bob", "g1", "k1", "Mine").await), "a member");
    assert!(forbidden(create_cat(&c, "carol", "g1", "k1", "Mine").await), "an admin of another group");
    assert!(forbidden(create_cat(&c, "alice", "nope", "k1", "Mine").await), "no such group");
    assert_eq!(count(&c, "SELECT COUNT(*) FROM channel_category").await, 0);

    assert!(ok(create_cat(&c, "alice", "g1", "k1", "Campaign").await));
    // Names need not be unique; new categories go to the bottom.
    assert!(ok(create_cat(&c, "dave", "g1", "k2", "Campaign").await));
    assert!(forbidden(create_cat(&c, "alice", "g1", "k1", "Again").await), "a taken id");
    let cats = categories_of(&c, "g1").await.unwrap();
    assert_eq!(cats.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["k1", "k2"]);
}

#[tokio::test]
async fn a_category_name_follows_the_group_name_rule() {
    let c = conn().await;
    let long = "x".repeat(101);
    for bad in ["", "   ", "a\nb", "<b>", "\u{202E}abc", long.as_str()] {
        assert!(invalid(create_cat(&c, "alice", "g1", "k1", bad).await), "{bad:?}");
    }
    assert!(ok(create_cat(&c, "alice", "g1", "k1", "Ops & logistics").await));
    for bad in ["", "a\u{0007}"] {
        assert!(invalid(rename_cat(&c, "alice", "g1", "k1", bad).await), "{bad:?}");
    }
}

#[tokio::test]
async fn only_an_admin_renames_reorders_or_deletes_a_category() {
    let c = conn().await;
    assert!(ok(create_cat(&c, "alice", "g1", "k1", "One").await));
    assert!(ok(create_cat(&c, "alice", "g1", "k2", "Two").await));
    assert!(ok(create_cat(&c, "carol", "g2", "k9", "Theirs").await));

    assert!(forbidden(rename_cat(&c, "bob", "g1", "k1", "Mine").await));
    assert!(forbidden(move_cat(&c, "bob", "g1", "k2", 0).await));
    assert!(forbidden(delete_cat(&c, "bob", "g1", "k1").await));
    // An admin of g1 naming g2's category, or claiming it under g1.
    assert!(forbidden(rename_cat(&c, "alice", "g2", "k9", "Mine").await));
    assert!(forbidden(rename_cat(&c, "alice", "g1", "k9", "Mine").await));
    assert!(forbidden(move_cat(&c, "alice", "g1", "k9", 0).await));
    assert!(forbidden(delete_cat(&c, "alice", "g1", "k9").await));
    assert!(forbidden(delete_cat(&c, "alice", "g2", "k9").await));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM channel_category").await, 3);
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM channel_category WHERE name IN ('One', 'Two', 'Theirs')").await,
        3
    );

    assert!(ok(rename_cat(&c, "alice", "g1", "k1", "Uno").await));
    assert!(ok(move_cat(&c, "dave", "g1", "k2", 0).await));
    let cats = categories_of(&c, "g1").await.unwrap();
    assert_eq!(
        cats.iter().map(|k| (k.id.as_str(), k.name.as_str())).collect::<Vec<_>>(),
        [("k2", "Two"), ("k1", "Uno")]
    );
    assert!(ok(move_cat(&c, "alice", "g1", "k2", 99).await), "past the end is the bottom");
    let cats = categories_of(&c, "g1").await.unwrap();
    assert_eq!(cats.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["k1", "k2"]);
}

// ── Moving channels ─────────────────────────────────────────────────────────

#[tokio::test]
async fn an_admin_moves_channels_between_blocks() {
    let c = conn().await;
    assert!(ok(create_cat(&c, "alice", "g1", "k1", "Campaign").await));
    assert!(ok(create_cat(&c, "alice", "g1", "k2", "Empty").await));

    assert!(ok(move_ch(&c, "alice", "g1", "c2", Some("k1"), 0).await));
    assert!(ok(move_ch(&c, "alice", "g1", "c3", Some("k1"), 0).await));
    assert_eq!(layout(&c, "g1").await, [b("-", &["c1", "c4"]), b("k1", &["c3", "c2"]), b("k2", &[])]);

    // Within a block.
    assert!(ok(move_ch(&c, "alice", "g1", "c3", Some("k1"), 1).await));
    assert_eq!(layout(&c, "g1").await[1], b("k1", &["c2", "c3"]));
    // Back to the top block, at a position.
    assert!(ok(move_ch(&c, "dave", "g1", "c2", None, 1).await));
    assert_eq!(layout(&c, "g1").await, [b("-", &["c1", "c2", "c4"]), b("k1", &["c3"]), b("k2", &[])]);
    // Past the end is the bottom.
    assert!(ok(move_ch(&c, "alice", "g1", "c1", Some("k2"), 40).await));
    assert_eq!(layout(&c, "g1").await, [b("-", &["c2", "c4"]), b("k1", &["c3"]), b("k2", &["c1"])]);
    assert_positions_contiguous(&c, "g1").await;
}

#[tokio::test]
async fn only_an_admin_moves_a_channel_and_only_within_its_group() {
    let c = conn().await;
    assert!(ok(create_cat(&c, "alice", "g1", "k1", "Campaign").await));
    assert!(ok(create_cat(&c, "carol", "g2", "k9", "Theirs").await));

    assert!(forbidden(move_ch(&c, "bob", "g1", "c1", Some("k1"), 0).await), "a member");
    assert!(forbidden(move_ch(&c, "carol", "g1", "c1", None, 2).await), "an admin of another group");
    assert!(forbidden(move_ch(&c, "alice", "g1", "c1", Some("k9"), 0).await), "into another group's category");
    assert!(forbidden(move_ch(&c, "alice", "g1", "x1", Some("k1"), 0).await), "another group's channel");
    assert!(forbidden(move_ch(&c, "alice", "g2", "c1", None, 0).await), "a channel claimed under the wrong group");
    assert!(forbidden(move_ch(&c, "alice", "g1", "nope", None, 0).await), "no such channel");
    assert_eq!(count(&c, "SELECT COUNT(*) FROM channels WHERE category_id IS NOT NULL").await, 0);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM channels WHERE position IS NOT NULL").await, 0);

    // The database refuses the cross-group placement too, past the DS.
    let raw = c.execute("UPDATE channels SET category_id = 'k9' WHERE id = 'c1'", ()).await;
    assert!(raw.is_err(), "a channel in another group's category");
    let raw = c
        .execute(
            "INSERT INTO conversation (id, kind) VALUES ('c9', 'channel'); ",
            (),
        )
        .await;
    assert!(raw.is_ok());
    let raw = c
        .execute(
            "INSERT INTO channels (id, group_id, name, category_id) VALUES ('c9', 'g1', 'c9', 'k9')",
            (),
        )
        .await;
    assert!(raw.is_err(), "a new channel in another group's category");
}

#[tokio::test]
async fn a_duplicate_position_is_unrepresentable() {
    let c = conn().await;
    assert!(ok(move_ch(&c, "alice", "g1", "c2", None, 0).await));
    let dup = c.execute("UPDATE channels SET position = 0 WHERE id = 'c3'", ()).await;
    assert!(dup.is_err(), "two channels at position 0 of one block");
    assert!(ok(create_cat(&c, "alice", "g1", "k1", "One").await));
    assert!(ok(create_cat(&c, "alice", "g1", "k2", "Two").await));
    let dup = c.execute("UPDATE channel_category SET position = 0 WHERE id = 'k2'", ()).await;
    assert!(dup.is_err(), "two categories at position 0");
    // The same position in different blocks is fine.
    assert!(ok(move_ch(&c, "alice", "g1", "c4", Some("k1"), 0).await));
    assert_positions_contiguous(&c, "g1").await;
}

// ── New channels ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_new_channel_lands_at_the_bottom_of_its_block() {
    let c = conn().await;
    assert!(ok(create_cat(&c, "alice", "g1", "k1", "Campaign").await));
    assert!(ok(move_ch(&c, "alice", "g1", "c1", Some("k1"), 0).await));
    // Rearranged top block: c4 first.
    assert!(ok(move_ch(&c, "alice", "g1", "c4", None, 0).await));

    assert!(ok(create_ch(&c, "alice", "g1", "n1", Some("k1")).await));
    assert!(ok(create_ch(&c, "alice", "g1", "n2", None).await));
    // A member may still create a channel (as before), uncategorised.
    assert!(ok(create_ch(&c, "bob", "g1", "n3", None).await));
    assert_eq!(layout(&c, "g1").await, [b("-", &["c4", "c2", "c3", "n2", "n3"]), b("k1", &["c1", "n1"])]);
    assert_positions_contiguous(&c, "g1").await;

    // Placing one in a category is an arrangement: admins only, own group only.
    assert!(forbidden(create_ch(&c, "bob", "g1", "n4", Some("k1")).await));
    assert!(ok(create_cat(&c, "carol", "g2", "k9", "Theirs").await));
    assert!(forbidden(create_ch(&c, "alice", "g1", "n5", Some("k9")).await));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM channels WHERE id IN ('n4', 'n5')").await, 0);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM conversation WHERE id IN ('n4', 'n5')").await, 0);
}

// ── Deleting a category ─────────────────────────────────────────────────────

#[tokio::test]
async fn deleting_a_category_moves_its_channels_to_the_top_block_and_deletes_none() {
    let c = conn().await;
    assert!(ok(create_cat(&c, "alice", "g1", "k1", "Campaign").await));
    assert!(ok(create_cat(&c, "alice", "g1", "k2", "Other").await));
    assert!(ok(create_cat(&c, "alice", "g1", "k3", "Third").await));
    assert!(ok(move_ch(&c, "alice", "g1", "c1", Some("k1"), 0).await));
    assert!(ok(move_ch(&c, "alice", "g1", "c3", Some("k1"), 0).await));
    assert!(ok(move_ch(&c, "alice", "g1", "c2", Some("k2"), 0).await));
    assert_eq!(layout(&c, "g1").await, [b("-", &["c4"]), b("k1", &["c3", "c1"]), b("k2", &["c2"]), b("k3", &[])]);

    assert!(ok(delete_cat(&c, "alice", "g1", "k1").await));
    // After the channels already there, in their order.
    assert_eq!(layout(&c, "g1").await, [b("-", &["c4", "c3", "c1"]), b("k2", &["c2"]), b("k3", &[])]);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM channels WHERE group_id = 'g1'").await, 4);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM conversation WHERE kind = 'channel'").await, 5);
    // Remaining categories renumbered, so the next one lands at the bottom.
    assert!(ok(create_cat(&c, "alice", "g1", "k4", "Fourth").await));
    let cats = categories_of(&c, "g1").await.unwrap();
    assert_eq!(cats.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["k2", "k3", "k4"]);
    assert_positions_contiguous(&c, "g1").await;
}

/// The schema itself never cascades a category delete to a channel, whatever a
/// writer does and whether or not foreign keys are enforced.
#[tokio::test]
async fn a_raw_category_delete_detaches_channels_and_deletes_none() {
    for fk in ["OFF", "ON"] {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let c = db.connect().unwrap();
        c.execute_batch(&format!("PRAGMA foreign_keys={fk};")).await.unwrap();
        seed(&c).await;
        assert!(ok(create_cat(&c, "alice", "g1", "k1", "Campaign").await));
        assert!(ok(move_ch(&c, "alice", "g1", "c1", Some("k1"), 0).await));
        assert!(ok(move_ch(&c, "alice", "g1", "c2", Some("k1"), 1).await));

        c.execute("DELETE FROM channel_category WHERE id = 'k1'", ()).await.unwrap();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM channels WHERE group_id = 'g1'").await, 4, "fk={fk}");
        assert_eq!(count(&c, "SELECT COUNT(*) FROM channels WHERE category_id IS NOT NULL").await, 0, "fk={fk}");
        let g = group(&c, "bob", "g1").await;
        assert_eq!(g.channels.len(), 4, "fk={fk}");
    }
}

#[tokio::test]
async fn deleting_the_group_deletes_its_categories() {
    let c = conn().await;
    assert!(ok(create_cat(&c, "alice", "g1", "k1", "Campaign").await));
    assert!(ok(create_cat(&c, "carol", "g2", "k9", "Theirs").await));
    let body = DeleteGroupBody {
        group_id: "g1".into(),
        requester_id: None,
    };
    let (outcome, _) = apply_delete_group(&c, Some("alice"), &body).await.unwrap();
    assert!(ok(outcome));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM channel_category WHERE group_id = 'g1'").await, 0);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM channel_category WHERE group_id = 'g2'").await, 1);
}

// ── Two admins at once ──────────────────────────────────────────────────────

/// Two admins dragging the same group at once, many times over, on separate
/// connections to one file: every move serializes, so each block ends numbered
/// 0..n with no duplicate and no channel lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_moves_never_duplicate_or_drop_a_position() {
    let db = common::TempDb::open("categories.db").await;
    {
        let c = db.conn().await.unwrap();
        seed(&c).await;
        assert!(ok(create_cat(&c, "alice", "g1", "k1", "One").await));
        assert!(ok(create_cat(&c, "alice", "g1", "k2", "Two").await));
    }
    let moves: Vec<(&str, &str, Option<&str>, u32)> = vec![
        ("alice", "c1", Some("k1"), 0),
        ("dave", "c2", Some("k1"), 0),
        ("alice", "c3", Some("k2"), 0),
        ("dave", "c4", Some("k1"), 1),
        ("alice", "c2", None, 0),
        ("dave", "c1", Some("k2"), 1),
        ("alice", "c4", None, 5),
        ("dave", "c3", Some("k1"), 0),
    ];
    for round in 0..6 {
        let mut tasks = Vec::new();
        for (i, (actor, ch, cat, pos)) in moves.iter().enumerate() {
            let db = db.arc();
            let (actor, ch, cat) = (actor.to_string(), ch.to_string(), cat.map(str::to_string));
            let pos = (*pos + round + i as u32) % 4;
            tasks.push(tokio::spawn(async move {
                let c = db.conn().await.unwrap();
                let body = move_body("g1", &ch, cat.as_deref(), pos);
                apply_move_channel(&c, Some(&actor), &body).await.unwrap()
            }));
        }
        for t in tasks {
            assert!(ok(t.await.unwrap()));
        }
        let c = db.conn().await.unwrap();
        assert_positions_contiguous(&c, "g1").await;
        let g = group(&c, "bob", "g1").await;
        let mut all = ids(&g.channels).into_iter().map(str::to_string).collect::<Vec<_>>();
        all.sort();
        assert_eq!(all, ["c1", "c2", "c3", "c4"], "round {round}: a channel went missing");
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM channels WHERE group_id = 'g1' AND position IS NULL").await,
            0,
            "round {round}"
        );
    }
}
