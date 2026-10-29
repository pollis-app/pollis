//! Per-channel mute (#1431); the push half is pinned in `push::tests`.

use libsql::Connection;
use pollis_api::groups::{LeaveGroupBody, RemoveMemberBody};
use pollis_api::profile::{ChannelMutesResponse, SetChannelMuteBody};
use pollis_delivery::groups::{apply_leave_group, apply_remove_member};
use pollis_delivery::profile::{apply_set_channel_mute, list_channel_mutes};
use pollis_delivery::writes::WriteOutcome;

async fn conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    // Production parity: Turso runs with foreign keys off.
    c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
    pollis_schema::apply::single_db(&c).await.unwrap();
    for sql in [
        "INSERT INTO conversation (id, kind) VALUES ('g1', 'group')",
        "INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'alice')",
        "INSERT INTO conversation (id, kind) VALUES ('ch1', 'channel')",
        "INSERT INTO channels (id, group_id, name) VALUES ('ch1', 'g1', 'general')",
        "INSERT INTO conversation (id, kind) VALUES ('ch2', 'channel')",
        "INSERT INTO channels (id, group_id, name) VALUES ('ch2', 'g1', 'random')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')",
        "INSERT INTO conversation (id, kind) VALUES ('dm1', 'dm')",
        "INSERT INTO dm_channel (id, created_by) VALUES ('dm1', 'alice')",
        "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm1', 'alice', 'alice')",
    ] {
        c.execute(sql, ()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    c
}

fn body(user: &str, channel: &str, muted: bool) -> SetChannelMuteBody {
    SetChannelMuteBody {
        user_id: user.into(),
        channel_id: channel.into(),
        muted,
    }
}

async fn mutes(c: &Connection, user: &str) -> Vec<String> {
    list_channel_mutes(c, user).await.unwrap().channel_ids
}

async fn rows(c: &Connection) -> i64 {
    let mut r = c.query("SELECT COUNT(*) FROM channel_mute", ()).await.unwrap();
    r.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

#[tokio::test]
async fn mute_and_unmute_are_idempotent_and_read_back() {
    let c = conn().await;
    for _ in 0..2 {
        let o = apply_set_channel_mute(&c, Some("bob"), &body("bob", "ch1", true)).await.unwrap();
        assert!(matches!(o, WriteOutcome::Ok));
    }
    assert_eq!(rows(&c).await, 1, "muting twice is one row");
    assert_eq!(mutes(&c, "bob").await, vec!["ch1".to_string()]);
    assert_eq!(list_channel_mutes(&c, "alice").await.unwrap(), ChannelMutesResponse::default());

    for _ in 0..2 {
        let o = apply_set_channel_mute(&c, Some("bob"), &body("bob", "ch1", false)).await.unwrap();
        assert!(matches!(o, WriteOutcome::Ok));
    }
    assert_eq!(rows(&c).await, 0);
    assert!(mutes(&c, "bob").await.is_empty());
}

/// A mute row can only ever name a channel its owner is in.
#[tokio::test]
async fn a_mute_cannot_be_written_for_anything_but_your_own_channel() {
    let c = conn().await;
    // Signed as bob, claiming to be alice.
    let o = apply_set_channel_mute(&c, Some("bob"), &body("alice", "ch1", true)).await.unwrap();
    assert!(matches!(o, WriteOutcome::Forbidden));
    let o = apply_set_channel_mute(&c, Some("carol"), &body("carol", "ch1", true)).await.unwrap();
    assert!(matches!(o, WriteOutcome::Forbidden));
    for id in ["dm1", "g1", "nope"] {
        let o = apply_set_channel_mute(&c, Some("alice"), &body("alice", id, true)).await.unwrap();
        assert!(matches!(o, WriteOutcome::Forbidden), "{id} must not be mutable");
    }
    assert_eq!(rows(&c).await, 0);
}

#[tokio::test]
async fn leaving_or_being_removed_drops_the_groups_mutes() {
    let c = conn().await;
    for ch in ["ch1", "ch2"] {
        apply_set_channel_mute(&c, Some("bob"), &body("bob", ch, true)).await.unwrap();
        apply_set_channel_mute(&c, Some("alice"), &body("alice", ch, true)).await.unwrap();
    }
    assert_eq!(rows(&c).await, 4);

    let (o, _) = apply_leave_group(
        &c,
        Some("bob"),
        &LeaveGroupBody { group_id: "g1".into(), user_id: Some("bob".into()) },
    )
    .await
    .unwrap();
    assert!(matches!(o, WriteOutcome::Ok));
    assert!(mutes(&c, "bob").await.is_empty(), "the leaver's mutes go");
    assert_eq!(mutes(&c, "alice").await.len(), 2, "another member's mutes stay");

    c.execute(
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')",
        (),
    )
    .await
    .unwrap();
    apply_set_channel_mute(&c, Some("bob"), &body("bob", "ch1", true)).await.unwrap();
    let o = apply_remove_member(
        &c,
        Some("alice"),
        &RemoveMemberBody {
            group_id: "g1".into(),
            user_id: "bob".into(),
            requester_id: Some("alice".into()),
        },
    )
    .await
    .unwrap();
    assert!(matches!(o, WriteOutcome::Ok));
    assert!(mutes(&c, "bob").await.is_empty(), "a removed member's mutes go");
    assert_eq!(rows(&c).await, 2);
}

/// A row stranded by a path that did not clean up is never served as a mute.
#[tokio::test]
async fn the_read_ignores_a_mute_left_behind_by_a_lost_membership() {
    let c = conn().await;
    apply_set_channel_mute(&c, Some("bob"), &body("bob", "ch1", true)).await.unwrap();
    c.execute("DELETE FROM group_member WHERE user_id = 'bob'", ()).await.unwrap();
    assert!(mutes(&c, "bob").await.is_empty());
}
