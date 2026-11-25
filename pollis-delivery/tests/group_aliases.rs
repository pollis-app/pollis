//! Per-group aliases (#2003): who may write, reset and read them, how long they
//! last, and when their avatar objects are deleted.

use std::sync::Mutex;

use libsql::Connection;
use pollis_api::groups::{DeleteGroupBody, LeaveGroupBody, RemoveMemberBody};
use pollis_api::profile::{
    GroupAliasWire, ResetGroupAliasBody, SetGroupAliasBody, UpdateProfileBody,
};
use pollis_delivery::aliases::{
    apply_reset_group_alias, apply_set_group_alias, list_group_aliases, list_my_aliases,
    reclaim_released_avatars, AvatarReclaimReport,
};
use pollis_delivery::broker::ObjectStore;
use pollis_delivery::groups::{apply_delete_group, apply_leave_group, apply_remove_member};
use pollis_delivery::profile::{apply_update_profile, ProfileOutcome};
use pollis_delivery::writes::WriteOutcome;

const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn key(owner: &str, hash: &str) -> String {
    format!("avatars/{owner}/{hash}.jpg")
}

async fn conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    // Production parity: Turso runs with foreign keys off.
    c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
    pollis_schema::apply::single_db(&c).await.unwrap();
    for sql in [
        "INSERT INTO users (id, email, username, account_id_pub) VALUES ('alice', 'a@x.com', 'alice', X'01')",
        "INSERT INTO users (id, email, username, account_id_pub) VALUES ('bob', 'b@x.com', 'bob', X'01')",
        "INSERT INTO users (id, email, username, account_id_pub) VALUES ('carol', 'c@x.com', 'carol', X'01')",
        "INSERT INTO conversation (id, kind) VALUES ('g1', 'group')",
        "INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'One', 'alice')",
        "INSERT INTO conversation (id, kind) VALUES ('g2', 'group')",
        "INSERT INTO groups (id, name, owner_id) VALUES ('g2', 'Two', 'alice')",
        "INSERT INTO conversation (id, kind) VALUES ('g3', 'group')",
        "INSERT INTO groups (id, name, owner_id) VALUES ('g3', 'Three', 'carol')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'alice', 'admin')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'bob', 'member')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g3', 'carol', 'admin')",
    ] {
        c.execute(sql, ()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    c
}

fn set(user: &str, group: Option<&str>, name: Option<&str>, avatar: Option<&str>) -> SetGroupAliasBody {
    SetGroupAliasBody {
        user_id: user.into(),
        group_id: group.map(str::to_string),
        display_name: name.map(str::to_string),
        avatar_url: avatar.map(str::to_string),
    }
}

async fn write(c: &Connection, actor: &str, body: SetGroupAliasBody) -> ProfileOutcome {
    apply_set_group_alias(c, Some(actor), &body).await.unwrap().0
}

fn reset(requester: &str, group: &str, user: &str) -> ResetGroupAliasBody {
    ResetGroupAliasBody {
        requester_id: requester.into(),
        group_id: group.into(),
        user_id: user.into(),
    }
}

fn alias(group: &str, user: &str, name: Option<&str>, avatar: Option<&str>) -> GroupAliasWire {
    GroupAliasWire {
        group_id: group.into(),
        user_id: user.into(),
        display_name: name.map(str::to_string),
        avatar_url: avatar.map(str::to_string),
    }
}

async fn read(c: &Connection, group: &str, reader: &str) -> Option<Vec<GroupAliasWire>> {
    let r = list_group_aliases(c, group, reader).await.unwrap();
    r.authorized.then_some(r.aliases)
}

async fn count(c: &Connection, sql: &str) -> i64 {
    let mut r = c.query(sql, ()).await.unwrap();
    r.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// An in-memory bucket that records deletes, standing in for R2.
#[derive(Default)]
struct Bucket {
    deleted: Mutex<Vec<String>>,
    fail: bool,
}

impl ObjectStore for Bucket {
    async fn delete_object(&self, key: &str) -> anyhow::Result<()> {
        if self.fail {
            anyhow::bail!("bucket down");
        }
        self.deleted.lock().unwrap().push(key.to_string());
        Ok(())
    }

    async fn object_exists(&self, key: &str) -> anyhow::Result<bool> {
        Ok(!self.deleted.lock().unwrap().iter().any(|k| k == key))
    }
}

/// Reclaim with no grace, so the test need not wait.
async fn reclaim(c: &Connection, bucket: &Bucket) -> AvatarReclaimReport {
    reclaim_released_avatars(c, bucket, 0).await.unwrap()
}

#[tokio::test]
async fn a_member_sets_and_edits_each_field_independently() {
    let c = conn().await;
    assert!(matches!(write(&c, "bob", set("bob", Some("g1"), Some("Bobby"), None)).await, ProfileOutcome::Ok));
    assert_eq!(read(&c, "g1", "alice").await.unwrap(), vec![alias("g1", "bob", Some("Bobby"), None)]);

    let k = key("bob", H1);
    write(&c, "bob", set("bob", Some("g1"), None, Some(&k))).await;
    assert_eq!(
        read(&c, "g1", "alice").await.unwrap(),
        vec![alias("g1", "bob", Some("Bobby"), Some(&k))],
        "setting the avatar leaves the name"
    );

    write(&c, "bob", set("bob", Some("g1"), Some(""), None)).await;
    assert_eq!(
        read(&c, "g1", "alice").await.unwrap(),
        vec![alias("g1", "bob", None, Some(&k))],
        "clearing the name falls back for the name only"
    );

    write(&c, "bob", set("bob", Some("g1"), None, Some(""))).await;
    assert_eq!(count(&c, "SELECT COUNT(*) FROM group_alias").await, 0, "clearing the last field deletes the row");
    assert!(read(&c, "g1", "alice").await.unwrap().is_empty());
}

/// No backfill: a group with no row reads as nothing, so every client falls back.
#[tokio::test]
async fn absence_of_a_row_is_the_fallback() {
    let c = conn().await;
    assert_eq!(read(&c, "g1", "bob").await, Some(Vec::new()));
    assert!(list_my_aliases(&c, "bob").await.unwrap().aliases.is_empty());
}

#[tokio::test]
async fn the_schema_refuses_a_row_that_says_nothing() {
    let c = conn().await;
    let e = c
        .execute(
            "INSERT INTO group_alias (group_id, user_id, display_name, avatar_url) VALUES ('g1', 'bob', NULL, NULL)",
            (),
        )
        .await;
    assert!(e.is_err(), "a row with neither field must not exist");
}

#[tokio::test]
async fn writes_are_refused_outside_the_rules() {
    let c = conn().await;
    // Signed as bob, claiming alice.
    assert!(matches!(write(&c, "bob", set("alice", Some("g1"), Some("A"), None)).await, ProfileOutcome::Forbidden));
    // Not a member of g3.
    assert!(matches!(write(&c, "bob", set("bob", Some("g3"), Some("B"), None)).await, ProfileOutcome::Forbidden));
    // Nothing to write.
    assert!(matches!(write(&c, "bob", set("bob", Some("g1"), None, None)).await, ProfileOutcome::Invalid(_)));
    // Same rules as preferred_name.
    for bad in ["a\nb", "<b>", "\u{202E}evil", &"x".repeat(101), "   "] {
        assert!(
            matches!(write(&c, "bob", set("bob", Some("g1"), Some(bad), None)).await, ProfileOutcome::Invalid(_)),
            "{bad:?} must be refused"
        );
    }
    // Someone else's picture, a legacy mutable key, a non-avatar object.
    for bad in [key("alice", H1), "avatars/bob".to_string(), format!("emoji/{H1}.png"), "https://example.com/x.jpg".into()] {
        assert!(
            matches!(write(&c, "bob", set("bob", Some("g1"), None, Some(&bad))).await, ProfileOutcome::Invalid(_)),
            "{bad} must be refused"
        );
    }
    assert_eq!(count(&c, "SELECT COUNT(*) FROM group_alias").await, 0);
}

#[tokio::test]
async fn names_need_not_be_unique() {
    let c = conn().await;
    write(&c, "alice", set("alice", Some("g1"), Some("Sam"), None)).await;
    write(&c, "bob", set("bob", Some("g1"), Some("Sam"), None)).await;
    assert_eq!(read(&c, "g1", "bob").await.unwrap().len(), 2);
}

#[tokio::test]
async fn apply_to_all_writes_only_the_chosen_field_to_every_current_group() {
    let c = conn().await;
    let k1 = key("bob", H1);
    write(&c, "bob", set("bob", Some("g1"), Some("One-name"), Some(&k1))).await;
    write(&c, "bob", set("bob", Some("g2"), Some("Two-name"), None)).await;

    let k2 = key("bob", H2);
    let (o, groups) = apply_set_group_alias(&c, Some("bob"), &set("bob", None, None, Some(&k2))).await.unwrap();
    assert!(matches!(o, ProfileOutcome::Ok));
    assert_eq!(groups, vec!["g1".to_string(), "g2".to_string()]);
    assert_eq!(
        list_my_aliases(&c, "bob").await.unwrap().aliases,
        vec![
            alias("g1", "bob", Some("One-name"), Some(&k2)),
            alias("g2", "bob", Some("Two-name"), Some(&k2)),
        ],
        "the avatar is overwritten everywhere; each group keeps its own name"
    );
    assert_eq!(
        count(&c, &format!("SELECT COUNT(*) FROM avatar_release WHERE r2_key = '{k1}'")).await,
        1,
        "the replaced picture is released"
    );

    // A group joined later starts on the main profile.
    c.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g3', 'bob', 'member')", ())
        .await
        .unwrap();
    assert_eq!(read(&c, "g3", "carol").await, Some(Vec::new()));
}

#[tokio::test]
async fn only_current_members_read_a_groups_aliases() {
    let c = conn().await;
    write(&c, "bob", set("bob", Some("g1"), Some("Bobby"), None)).await;
    assert_eq!(read(&c, "g1", "carol").await, None, "a non-member is refused");
    assert_eq!(read(&c, "g1", "nobody").await, None);
    assert!(read(&c, "g1", "alice").await.is_some());
    assert!(read(&c, "g1", "bob").await.is_some());
}

#[tokio::test]
async fn an_alias_survives_leave_and_removal_and_stays_readable() {
    let c = conn().await;
    write(&c, "bob", set("bob", Some("g1"), Some("Bobby"), None)).await;
    write(&c, "bob", set("bob", Some("g2"), Some("Bob Two"), None)).await;

    let (o, _) = apply_leave_group(&c, Some("bob"), &LeaveGroupBody { group_id: "g1".into(), user_id: Some("bob".into()) })
        .await
        .unwrap();
    assert!(matches!(o, WriteOutcome::Ok));
    assert_eq!(
        read(&c, "g1", "alice").await.unwrap(),
        vec![alias("g1", "bob", Some("Bobby"), None)],
        "a former member's alias is still readable by current members"
    );
    assert_eq!(read(&c, "g1", "bob").await, None, "the former member no longer reads the group");
    assert_eq!(list_my_aliases(&c, "bob").await.unwrap().aliases.len(), 2, "the owner still lists it");

    let o = apply_remove_member(
        &c,
        Some("alice"),
        &RemoveMemberBody { group_id: "g2".into(), user_id: "bob".into(), requester_id: Some("alice".into()) },
    )
    .await
    .unwrap();
    assert!(matches!(o, WriteOutcome::Ok));
    assert_eq!(read(&c, "g2", "alice").await.unwrap().len(), 1, "removal keeps it too");

    // A former member cannot write there, but rejoining restores the alias.
    assert!(matches!(write(&c, "bob", set("bob", Some("g1"), Some("X"), None)).await, ProfileOutcome::Forbidden));
    c.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')", ())
        .await
        .unwrap();
    assert_eq!(read(&c, "g1", "bob").await.unwrap(), vec![alias("g1", "bob", Some("Bobby"), None)]);
}

#[tokio::test]
async fn the_owner_resets_even_after_leaving_and_an_admin_resets_anyone() {
    let c = conn().await;
    write(&c, "bob", set("bob", Some("g1"), Some("Bobby"), None)).await;
    write(&c, "bob", set("bob", Some("g2"), Some("Bob Two"), None)).await;
    write(&c, "alice", set("alice", Some("g1"), Some("Al"), None)).await;

    // A plain member cannot reset someone else; an outsider neither.
    assert!(matches!(apply_reset_group_alias(&c, Some("bob"), &reset("bob", "g1", "alice")).await.unwrap(), WriteOutcome::Forbidden));
    assert!(matches!(apply_reset_group_alias(&c, Some("carol"), &reset("carol", "g1", "bob")).await.unwrap(), WriteOutcome::Forbidden));
    // Signed as carol, claiming alice.
    assert!(matches!(apply_reset_group_alias(&c, Some("carol"), &reset("alice", "g1", "bob")).await.unwrap(), WriteOutcome::Forbidden));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM group_alias").await, 3);

    // The admin resets bob in g1 only.
    assert!(matches!(apply_reset_group_alias(&c, Some("alice"), &reset("alice", "g1", "bob")).await.unwrap(), WriteOutcome::Ok));
    assert_eq!(read(&c, "g1", "alice").await.unwrap(), vec![alias("g1", "alice", Some("Al"), None)]);
    assert_eq!(read(&c, "g2", "alice").await.unwrap().len(), 1, "another group's alias is untouched");
    // ...and bob may set it again.
    assert!(matches!(write(&c, "bob", set("bob", Some("g1"), Some("Bobby again"), None)).await, ProfileOutcome::Ok));

    // The owner resets their own after leaving.
    apply_leave_group(&c, Some("bob"), &LeaveGroupBody { group_id: "g2".into(), user_id: Some("bob".into()) })
        .await
        .unwrap();
    assert!(matches!(apply_reset_group_alias(&c, Some("bob"), &reset("bob", "g2", "bob")).await.unwrap(), WriteOutcome::Ok));
    assert!(read(&c, "g2", "alice").await.unwrap().is_empty());
    // Idempotent.
    assert!(matches!(apply_reset_group_alias(&c, Some("bob"), &reset("bob", "g2", "bob")).await.unwrap(), WriteOutcome::Ok));
}

#[tokio::test]
async fn group_deletion_takes_its_aliases_and_releases_their_avatars() {
    let c = conn().await;
    let k = key("bob", H1);
    write(&c, "bob", set("bob", Some("g1"), Some("Bobby"), Some(&k))).await;
    write(&c, "bob", set("bob", Some("g2"), Some("Bob Two"), None)).await;
    let (o, _) = apply_delete_group(&c, Some("alice"), &DeleteGroupBody { group_id: "g1".into(), requester_id: Some("alice".into()) })
        .await
        .unwrap();
    assert!(matches!(o, WriteOutcome::Ok));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM group_alias WHERE group_id = 'g1'").await, 0);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM group_alias WHERE group_id = 'g2'").await, 1);
    let bucket = Bucket::default();
    assert_eq!(reclaim(&c, &bucket).await.deleted, 1);
    assert_eq!(*bucket.deleted.lock().unwrap(), vec![k]);
}

/// An avatar object goes only once no profile and no alias references it.
#[tokio::test]
async fn an_avatar_object_is_deleted_only_when_nothing_references_it() {
    let c = conn().await;
    let shared = key("bob", H1);
    // The main profile and two aliases all use one picture.
    apply_update_profile(
        &c,
        Some("bob"),
        &UpdateProfileBody { user_id: "bob".into(), username: None, preferred_name: None, phone: None, avatar_url: Some(shared.clone()) },
    )
    .await
    .unwrap();
    write(&c, "bob", set("bob", None, None, Some(&shared))).await;

    let bucket = Bucket::default();
    // Reset one alias: still referenced by the other and the profile.
    apply_reset_group_alias(&c, Some("bob"), &reset("bob", "g1", "bob")).await.unwrap();
    let r = reclaim(&c, &bucket).await;
    assert_eq!((r.deleted, r.kept), (0, 1));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM avatar_release").await, 0, "a kept key loses its release row");

    apply_reset_group_alias(&c, Some("bob"), &reset("bob", "g2", "bob")).await.unwrap();
    assert_eq!(reclaim(&c, &bucket).await.deleted, 0, "the profile still uses it");

    // The profile moves on: now nothing references the old picture.
    apply_update_profile(
        &c,
        Some("bob"),
        &UpdateProfileBody { user_id: "bob".into(), username: None, preferred_name: None, phone: None, avatar_url: Some(key("bob", H2)) },
    )
    .await
    .unwrap();
    assert_eq!(reclaim(&c, &bucket).await.deleted, 1);
    assert_eq!(*bucket.deleted.lock().unwrap(), vec![shared]);
}

#[tokio::test]
async fn a_released_avatar_waits_out_the_grace_and_a_failed_delete_is_retried() {
    let c = conn().await;
    let k = key("bob", H1);
    write(&c, "bob", set("bob", Some("g1"), None, Some(&k))).await;
    write(&c, "bob", set("bob", Some("g1"), None, Some(""))).await;

    let bucket = Bucket::default();
    let r = reclaim_released_avatars(&c, &bucket, 600).await.unwrap();
    assert_eq!(r, AvatarReclaimReport::default(), "inside the grace nothing is touched");

    let down = Bucket { fail: true, ..Default::default() };
    assert_eq!(reclaim(&c, &down).await.failed, 1);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM avatar_release").await, 1, "kept for the next pass");
    assert_eq!(reclaim(&c, &bucket).await.deleted, 1);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM avatar_release").await, 0);
}

/// Re-choosing a released picture before the pass runs keeps its bytes.
#[tokio::test]
async fn a_released_avatar_chosen_again_is_kept() {
    let c = conn().await;
    let k = key("bob", H1);
    write(&c, "bob", set("bob", Some("g1"), None, Some(&k))).await;
    write(&c, "bob", set("bob", Some("g1"), None, Some(""))).await;
    write(&c, "bob", set("bob", Some("g2"), None, Some(&k))).await;
    let bucket = Bucket::default();
    let r = reclaim(&c, &bucket).await;
    assert_eq!((r.deleted, r.kept), (0, 1));
    assert!(bucket.deleted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn account_deletion_takes_every_alias_and_releases_every_picture() {
    let c = conn().await;
    apply_update_profile(
        &c,
        Some("bob"),
        &UpdateProfileBody { user_id: "bob".into(), username: None, preferred_name: None, phone: None, avatar_url: Some(key("bob", H2)) },
    )
    .await
    .unwrap();
    write(&c, "bob", set("bob", Some("g1"), Some("Bobby"), Some(&key("bob", H1)))).await;
    write(&c, "alice", set("alice", Some("g1"), Some("Al"), None)).await;

    let (o, _) = pollis_delivery::account::apply_delete_account(
        &c,
        Some("bob"),
        &pollis_api::account::DeleteAccountBody { user_id: Some("bob".into()), withdraw: Vec::new() },
    )
    .await
    .unwrap();
    assert!(matches!(o, WriteOutcome::Ok));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM group_alias WHERE user_id = 'bob'").await, 0);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM group_alias WHERE user_id = 'alice'").await, 1);
    let bucket = Bucket::default();
    assert_eq!(reclaim(&c, &bucket).await.deleted, 2, "the alias picture and the profile picture");
}
