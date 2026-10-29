//! Account and group teardown actually removes everything.
//! Production Turso runs `foreign_keys=OFF`, so the schema's `ON DELETE CASCADE` never fires.

use libsql::Connection;
use pollis_delivery::account::{apply_delete_account, apply_reset_recover};
use pollis_delivery::groups::apply_delete_group;
use pollis_delivery::teardown::{
    purge_user_rows, EXEMPT_FROM_CONVERSATION_PURGE, EXEMPT_FROM_USER_PURGE,
    USER_PURGED_TABLES,
};
use pollis_delivery::writes::WriteOutcome;
use pollis_api::account::DeleteAccountBody;
use pollis_api::groups::DeleteGroupBody;

/// Every user-naming (table, column), written out for review; the coverage test proves it complete.
const USER_COLUMNS: &[(&str, &str)] = &[
    ("account_recovery", "user_id"),
    ("channel_mute", "user_id"),
    ("conversation_watermark", "user_id"),
    ("device_enrollment_request", "user_id"),
    ("dm_channel", "created_by"),
    ("dm_channel_member", "user_id"),
    ("dm_channel_member", "added_by"),
    ("group_emoji", "created_by"),
    ("group_join_request", "reviewed_by"),
    ("groups", "owner_id"),
    ("group_invite", "inviter_id"),
    ("group_invite", "invitee_id"),
    ("group_invite_link", "created_by"),
    ("group_invite_link_redemption", "user_id"),
    ("group_join_request", "requester_id"),
    ("group_member", "user_id"),
    ("message_envelope", "sender_id"),
    ("message_reaction", "user_id"),
    ("mls_commit_since", "user_id"),
    ("mls_key_package", "user_id"),
    ("mls_welcome", "recipient_id"),
    ("push_token", "user_id"),
    ("read_cursor_sync", "user_id"),
    ("security_event", "user_id"),
    ("user_block", "blocker_id"),
    ("user_block", "blocked_id"),
    ("user_report", "reporter_id"),
    ("user_report", "reported_id"),
    ("account_suspension", "user_id"),
    ("user_device", "user_id"),
    ("user_preferences", "user_id"),
    ("users", "id"),
];

const GROUP_COLUMNS: &[(&str, &str)] = &[
    ("channels", "group_id"),
    ("group_emoji", "group_id"),
    ("group_invite", "group_id"),
    ("group_invite_link", "group_id"),
    ("group_join_request", "group_id"),
    ("group_member", "group_id"),
    ("groups", "id"),
];

async fn conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    // Production parity.
    c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
    pollis_schema::apply::single_db(&c).await.unwrap();
    c
}

async fn exec(c: &Connection, sql: &str) {
    c.execute(sql, ()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn count_where(c: &Connection, table: &str, col: &str, val: &str) -> i64 {
    let mut rows = c
        .query(
            &format!("SELECT COUNT(*) FROM {table} WHERE {col} = ?1"),
            libsql::params![val.to_string()],
        )
        .await
        .unwrap_or_else(|e| panic!("count {table}.{col}: {e}"));
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

async fn count(c: &Connection, table: &str) -> i64 {
    let mut rows = c.query(&format!("SELECT COUNT(*) FROM {table}"), ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// Seed every user-scoped table for `u` (ids namespaced by `u`); `solo` decides sharing with `peer`.
async fn seed_user(c: &Connection, u: &str, peer: &str, solo: bool) {
    exec(c, &format!(
        "INSERT INTO users (id, email, username, account_id_pub) \
         VALUES ('{u}', '{u}@x.com', '{u}', X'01')"
    )).await;
    exec(c, &format!(
        "INSERT INTO account_key_log (user_id, account_id_pub, identity_version) \
         VALUES ('{u}', X'01', 1)"
    )).await;
    exec(c, &format!(
        "INSERT INTO user_device (device_id, user_id) VALUES ('{u}_dev', '{u}')"
    )).await;
    exec(c, &format!(
        "INSERT INTO account_recovery (user_id, identity_version, salt, nonce, wrapped_key) \
         VALUES ('{u}', 1, X'01', X'02', X'03')"
    )).await;
    exec(c, &format!(
        "INSERT INTO device_enrollment_request \
         (id, user_id, new_device_id, new_device_ephemeral_pub, verification_code, status, expires_at) \
         VALUES ('{u}_enr', '{u}', '{u}_d2', X'01', '123456', 'pending', '2099-01-01')"
    )).await;
    exec(c, &format!(
        "INSERT INTO security_event (id, user_id, kind) VALUES ('{u}_sec', '{u}', 'device_added')"
    )).await;
    exec(c, &format!(
        "INSERT INTO user_preferences (user_id, preferences) VALUES ('{u}', '{{}}')"
    )).await;
    exec(c, &format!(
        "INSERT INTO read_cursor_sync (user_id, nonce, blob) VALUES ('{u}', X'01', X'02')"
    )).await;
    exec(c, &format!(
        "INSERT INTO push_token (token, user_id, platform, updated_at) \
         VALUES ('{u}_tok', '{u}', 'ios', datetime('now'))"
    )).await;
    exec(c, &format!(
        "INSERT INTO mls_key_package (ref_hash, user_id, key_package) \
         VALUES ('{u}_kp', '{u}', X'01')"
    )).await;
    exec(c, &format!(
        "INSERT INTO user_block (blocker_id, blocked_id) VALUES ('{u}', '{u}_stranger')"
    )).await;
    exec(c, &format!(
        "INSERT INTO user_block (blocker_id, blocked_id) VALUES ('{u}_stranger2', '{u}')"
    )).await;
    exec(c, &format!(
        "INSERT INTO user_report (id, reporter_id, reported_id, reason) \
         VALUES ('{u}_r1', '{u}', '{u}_stranger', 'spam')"
    )).await;
    exec(c, &format!(
        "INSERT INTO user_report (id, reporter_id, reported_id, reason) \
         VALUES ('{u}_r2', '{u}_stranger2', '{u}', 'harassment')"
    )).await;
    exec(c, &format!(
        "INSERT INTO account_suspension (user_id, reason) VALUES ('{u}', 'test')"
    )).await;

    let gid = format!("{u}_g");
    let cid = format!("{u}_c");
    // Claim before the row: 000017's triggers refuse an unclaimed insert (#948).
    exec(c, &format!("INSERT INTO conversation (id, kind) VALUES ('{gid}', 'group')")).await;
    exec(c, &format!(
        "INSERT INTO groups (id, name, owner_id) VALUES ('{gid}', 'G', '{u}')"
    )).await;
    exec(c, &format!(
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('{gid}', '{u}', 'admin')"
    )).await;
    exec(c, &format!("INSERT INTO conversation (id, kind) VALUES ('{cid}', 'channel')")).await;
    exec(c, &format!(
        "INSERT INTO channels (id, group_id, name) VALUES ('{cid}', '{gid}', 'general')"
    )).await;
    exec(c, &format!(
        "INSERT INTO channel_mute (user_id, channel_id) VALUES ('{u}', '{cid}')"
    )).await;
    exec(c, &format!(
        "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) \
         VALUES ('{u}_msg', '{cid}', '{u}', 'ct', '2026-01-01')"
    )).await;
    exec(c, &format!(
        "INSERT INTO message_reaction (id, message_id, user_id, emoji) \
         VALUES ('{u}_rx', '{u}_msg', '{u}', ':+1:')"
    )).await;
    exec(c, &format!(
        "INSERT INTO attachment_ref (content_hash, message_id) VALUES ('{u}_hash', '{u}_msg')"
    )).await;
    exec(c, &format!(
        "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at) \
         VALUES ('{cid}', '{u}', '{u}_dev', '2026-01-01')"
    )).await;
    exec(c, &format!(
        "INSERT INTO mls_group_info (conversation_id, epoch, group_info, updated_by_device_id) \
         VALUES ('{cid}', 1, X'01', '{u}_dev')"
    )).await;
    exec(c, &format!(
        "INSERT INTO mls_commit_log (conversation_id, epoch, sender_id, commit_data) \
         VALUES ('{cid}', 0, '{u}', X'01')"
    )).await;
    exec(c, &format!(
        "INSERT INTO mls_welcome (id, conversation_id, recipient_id, welcome_data) \
         VALUES ('{u}_w', '{cid}', '{u}', X'01')"
    )).await;
    exec(c, &format!(
        "INSERT INTO mls_commit_since (conversation_id, user_id, device_id, since_epoch) \
         VALUES ('{cid}', '{u}', '{u}_dev', 0)"
    )).await;
    exec(c, &format!(
        "INSERT INTO group_emoji (group_id, shortcode, content_hash, created_by) \
         VALUES ('{gid}', 'parrot', '{u}_ehash', '{u}')"
    )).await;
    exec(c, &format!(
        "INSERT INTO custom_emoji_object (content_hash, r2_key, content_type, size_bytes) \
         VALUES ('{u}_ehash', 'emoji/x.webp', 'image/webp', 10)"
    )).await;

    exec(c, &format!(
        "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) \
         VALUES ('{u}_inv_out', '{gid}', '{u}', '{u}_stranger')"
    )).await;
    exec(c, &format!(
        "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) \
         VALUES ('{u}_inv_in', '{gid}', '{u}_stranger', '{u}')"
    )).await;
    exec(c, &format!(
        "INSERT INTO group_join_request (id, group_id, requester_id, reviewed_by) \
         VALUES ('{u}_jr', '{gid}', '{u}', NULL)"
    )).await;
    exec(c, &format!(
        "INSERT INTO group_invite_link (id, group_id, created_by, selector, secret_hash) \
         VALUES ('{u}_link', '{gid}', '{u}', '{u}_sel', 'deadbeef')"
    )).await;
    exec(c, &format!(
        "INSERT INTO group_invite_link_redemption (id, link_id, user_id, succeeded) \
         VALUES ('{u}_red', '{u}_link', '{u}', 1)"
    )).await;

    let dm = format!("{u}_dm");
    exec(c, &format!("INSERT INTO conversation (id, kind) VALUES ('{dm}', 'dm')")).await;
    exec(c, &format!("INSERT INTO dm_channel (id, created_by) VALUES ('{dm}', '{u}')")).await;
    exec(c, &format!(
        "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) \
         VALUES ('{dm}', '{u}', '{u}')"
    )).await;

    if !solo {
        // A second member keeps the group and DM alive through the teardown.
        exec(c, &format!(
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('{gid}', '{peer}', 'member')"
        )).await;
        exec(c, &format!(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) \
             VALUES ('{dm}', '{peer}', '{u}')"
        )).await;
    }
}

/// After deleting an account, no table names that user.
#[tokio::test]
async fn account_deletion_leaves_no_row_naming_the_user() {
    let c = conn().await;
    seed_user(&c, "victim", "bystander", true).await;

    let (outcome, dead) = apply_delete_account(
        &c,
        Some("victim"),
        &DeleteAccountBody { user_id: Some("victim".into()) },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    // One DB stands in for the log DB here, so run its purge too.
    pollis_delivery::writes::purge_welcomes(&c, "victim").await.unwrap();
    pollis_delivery::teardown::purge_conversation_log(&c, &dead).await.unwrap();

    for (table, col) in USER_COLUMNS {
        let n = count_where(&c, table, col, "victim").await;
        assert_eq!(n, 0, "{table}.{col} still names the deleted user ({n} rows)");
    }
}

/// Surviving conversations need their `NOT NULL` pointers to the leaver handed over.
#[tokio::test]
async fn account_deletion_leaves_no_row_naming_the_user_in_surviving_conversations() {
    let c = conn().await;
    seed_user(&c, "bystander", "victim", true).await;
    seed_user(&c, "victim", "bystander", false).await;

    let (outcome, dead) = apply_delete_account(
        &c,
        Some("victim"),
        &DeleteAccountBody { user_id: Some("victim".into()) },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    pollis_delivery::writes::purge_welcomes(&c, "victim").await.unwrap();
    pollis_delivery::teardown::purge_user_log_rows(&c, "victim").await.unwrap();
    pollis_delivery::teardown::purge_conversation_log(&c, &dead).await.unwrap();

    assert_eq!(count_where(&c, "groups", "id", "victim_g").await, 1);
    assert_eq!(count_where(&c, "dm_channel", "id", "victim_dm").await, 1);

    for (table, col) in USER_COLUMNS {
        let n = count_where(&c, table, col, "victim").await;
        assert_eq!(n, 0, "{table}.{col} still names the deleted user ({n} rows)");
    }

    let mut rows = c
        .query("SELECT created_by FROM dm_channel WHERE id = 'victim_dm'", ())
        .await
        .unwrap();
    let creator: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(creator, "bystander", "DM creator must pass to a remaining member");
}

/// The account-key transparency log survives: its leaves are published.
#[tokio::test]
async fn account_deletion_keeps_the_append_only_transparency_log() {
    let c = conn().await;
    seed_user(&c, "victim", "bystander", true).await;

    apply_delete_account(
        &c,
        Some("victim"),
        &DeleteAccountBody { user_id: Some("victim".into()) },
    )
    .await
    .unwrap();

    assert_eq!(
        count_where(&c, "account_key_log", "user_id", "victim").await,
        1,
        "account_key_log is append-only and must survive account deletion"
    );
}

/// A commit in a surviving conversation's chain stays, or members hit an epoch gap.
#[tokio::test]
async fn account_deletion_does_not_gap_a_surviving_conversations_commit_chain() {
    let c = conn().await;
    seed_user(&c, "bystander", "victim", false).await;
    seed_user(&c, "victim", "bystander", false).await;
    exec(&c, "INSERT INTO mls_commit_log (conversation_id, epoch, sender_id, commit_data) \
              VALUES ('bystander_c', 1, 'victim', X'02')").await;

    apply_delete_account(
        &c,
        Some("victim"),
        &DeleteAccountBody { user_id: Some("victim".into()) },
    )
    .await
    .unwrap();

    assert_eq!(
        count_where(&c, "mls_commit_log", "conversation_id", "bystander_c").await,
        2,
        "a surviving conversation's commit chain must keep every epoch"
    );
}

/// The #878 defect class: deleting one account must not reach another's rows.
#[tokio::test]
async fn account_deletion_cannot_reach_another_account() {
    let c = conn().await;
    seed_user(&c, "bystander", "victim", true).await;
    seed_user(&c, "victim", "bystander", true).await;

    let before: Vec<i64> = {
        let mut v = Vec::new();
        for (table, col) in USER_COLUMNS {
            v.push(count_where(&c, table, col, "bystander").await);
        }
        v
    };

    let (_, dead) = apply_delete_account(
        &c,
        Some("victim"),
        &DeleteAccountBody { user_id: Some("victim".into()) },
    )
    .await
    .unwrap();
    pollis_delivery::writes::purge_welcomes(&c, "victim").await.unwrap();
    pollis_delivery::teardown::purge_conversation_log(&c, &dead).await.unwrap();

    for (i, (table, col)) in USER_COLUMNS.iter().enumerate() {
        let after = count_where(&c, table, col, "bystander").await;
        assert_eq!(
            after, before[i],
            "deleting 'victim' changed {table}.{col} for 'bystander' ({} → {after})",
            before[i]
        );
    }
    assert_eq!(count_where(&c, "groups", "id", "bystander_g").await, 1);
    assert_eq!(count_where(&c, "dm_channel", "id", "bystander_dm").await, 1);
    assert_eq!(count_where(&c, "channels", "id", "bystander_c").await, 1);
}

/// A shared group survives with `owner_id` handed to the promoted admin.
#[tokio::test]
async fn account_deletion_hands_over_a_shared_group_instead_of_destroying_it() {
    let c = conn().await;
    seed_user(&c, "bystander", "victim", true).await;
    seed_user(&c, "victim", "bystander", false).await;

    apply_delete_account(
        &c,
        Some("victim"),
        &DeleteAccountBody { user_id: Some("victim".into()) },
    )
    .await
    .unwrap();

    assert_eq!(count_where(&c, "groups", "id", "victim_g").await, 1, "shared group survives");
    assert_eq!(count_where(&c, "group_member", "group_id", "victim_g").await, 1);
    let mut rows = c
        .query("SELECT owner_id FROM groups WHERE id = 'victim_g'", ())
        .await
        .unwrap();
    let owner: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(owner, "bystander", "owner_id must not name the deleted user");
    let mut rows = c
        .query("SELECT role FROM group_member WHERE group_id = 'victim_g'", ())
        .await
        .unwrap();
    let role: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(role, "admin", "sole remaining member is promoted");
}

#[tokio::test]
async fn account_deletion_tears_down_a_dm_it_empties() {
    let c = conn().await;
    seed_user(&c, "victim", "bystander", true).await;

    apply_delete_account(
        &c,
        Some("victim"),
        &DeleteAccountBody { user_id: Some("victim".into()) },
    )
    .await
    .unwrap();

    assert_eq!(count(&c, "dm_channel").await, 0, "emptied DM must be removed");
    assert_eq!(count(&c, "dm_channel_member").await, 0);
}

/// L3: removing yourself via `/v1/dm/members/remove` must tear down an emptied DM too.
#[tokio::test]
async fn self_removal_tears_down_a_dm_it_empties() {
    let c = conn().await;
    seed_user(&c, "victim", "bystander", true).await;

    let (outcome, torn_down) = pollis_delivery::profile::apply_remove_dm_member(
        &c,
        Some("victim"),
        &pollis_api::profile::RemoveDmMemberBody {
            dm_channel_id: "victim_dm".into(),
            user_id: "victim".into(),
            requester_id: "victim".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    assert!(torn_down, "the last member left, so the DM is gone");

    assert_eq!(
        count_where(&c, "dm_channel", "id", "victim_dm").await,
        0,
        "the emptied DM must not survive a self-removal"
    );
    assert_eq!(
        count_where(&c, "dm_channel_member", "dm_channel_id", "victim_dm").await,
        0
    );
}

#[tokio::test]
async fn self_removal_leaves_a_dm_that_still_has_members() {
    let c = conn().await;
    seed_user(&c, "victim", "bystander", false).await;

    let (outcome, torn_down) = pollis_delivery::profile::apply_remove_dm_member(
        &c,
        Some("victim"),
        &pollis_api::profile::RemoveDmMemberBody {
            dm_channel_id: "victim_dm".into(),
            user_id: "victim".into(),
            requester_id: "victim".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    assert!(!torn_down);

    assert_eq!(count_where(&c, "dm_channel", "id", "victim_dm").await, 1);
    assert_eq!(
        count_where(&c, "dm_channel_member", "user_id", "bystander").await,
        1,
        "the remaining member keeps their seat"
    );
}

#[tokio::test]
async fn identity_reset_keeps_the_account_but_sheds_membership() {
    let c = conn().await;
    seed_user(&c, "victim", "bystander", true).await;

    let (outcome, _) = apply_reset_recover(
        &c,
        Some("victim"),
        &pollis_api::account::ResetRecoverBody {
            user_id: Some("victim".into()),
            current_device_id: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));

    assert_eq!(count_where(&c, "users", "id", "victim").await, 1, "the account survives");
    assert_eq!(count_where(&c, "user_preferences", "user_id", "victim").await, 1);
    assert_eq!(count_where(&c, "group_member", "user_id", "victim").await, 0);
    assert_eq!(count_where(&c, "dm_channel_member", "user_id", "victim").await, 0);
    assert_eq!(count_where(&c, "user_device", "user_id", "victim").await, 0);
    assert_eq!(count_where(&c, "conversation_watermark", "user_id", "victim").await, 0);
}

#[tokio::test]
async fn group_deletion_leaves_no_row_naming_the_group() {
    let c = conn().await;
    seed_user(&c, "owner", "peer", false).await;
    seed_user(&c, "peer", "owner", true).await;

    let (outcome, dead) = apply_delete_group(
        &c,
        Some("owner"),
        &DeleteGroupBody {
            group_id: "owner_g".into(),
            requester_id: Some("owner".into()),
        },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    pollis_delivery::teardown::purge_conversation_log(&c, &dead).await.unwrap();

    for (table, col) in GROUP_COLUMNS {
        let n = count_where(&c, table, col, "owner_g").await;
        assert_eq!(n, 0, "{table}.{col} still names the deleted group ({n} rows)");
    }
    // The channel and everything hanging off it.
    for (table, col) in [
        ("channels", "id"),
        ("channel_mute", "channel_id"),
        ("message_envelope", "conversation_id"),
        ("conversation_watermark", "conversation_id"),
        ("mls_group_info", "conversation_id"),
        ("mls_commit_log", "conversation_id"),
        ("mls_welcome", "conversation_id"),
    ] {
        let n = count_where(&c, table, col, "owner_c").await;
        assert_eq!(n, 0, "{table}.{col} outlived the deleted group's channel ({n} rows)");
    }
    assert_eq!(count_where(&c, "message_reaction", "message_id", "owner_msg").await, 0);
    assert_eq!(count_where(&c, "attachment_ref", "message_id", "owner_msg").await, 0);
    // The redemption row belongs to a user, so it survives with a blanked pointer.
    assert_eq!(count_where(&c, "group_invite_link_redemption", "id", "owner_red").await, 1);
    let mut rows = c
        .query("SELECT link_id FROM group_invite_link_redemption WHERE id = 'owner_red'", ())
        .await
        .unwrap();
    let link: Option<String> = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(link, None, "a redemption's dangling link_id must be nulled");
}

#[tokio::test]
async fn group_deletion_cannot_reach_another_group() {
    let c = conn().await;
    seed_user(&c, "owner", "peer", true).await;
    seed_user(&c, "peer", "owner", true).await;

    let before: Vec<i64> = {
        let mut v = Vec::new();
        for (table, col) in GROUP_COLUMNS {
            v.push(count_where(&c, table, col, "peer_g").await);
        }
        v
    };

    let (_, dead) = apply_delete_group(
        &c,
        Some("owner"),
        &DeleteGroupBody {
            group_id: "owner_g".into(),
            requester_id: Some("owner".into()),
        },
    )
    .await
    .unwrap();
    pollis_delivery::teardown::purge_conversation_log(&c, &dead).await.unwrap();

    for (i, (table, col)) in GROUP_COLUMNS.iter().enumerate() {
        let after = count_where(&c, table, col, "peer_g").await;
        assert_eq!(
            after, before[i],
            "deleting 'owner_g' changed {table}.{col} for 'peer_g' ({} → {after})",
            before[i]
        );
    }
    assert_eq!(count_where(&c, "message_envelope", "conversation_id", "peer_c").await, 1);
    assert_eq!(count_where(&c, "mls_commit_log", "conversation_id", "peer_c").await, 1);
    assert_eq!(count_where(&c, "conversation_watermark", "conversation_id", "peer_c").await, 1);
}

#[tokio::test]
async fn group_deletion_refuses_a_non_admin() {
    let c = conn().await;
    seed_user(&c, "owner", "peer", false).await;

    let (outcome, dead) = apply_delete_group(
        &c,
        Some("peer"),
        &DeleteGroupBody {
            group_id: "owner_g".into(),
            requester_id: Some("peer".into()),
        },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, WriteOutcome::Forbidden));
    assert!(dead.is_empty());
    assert_eq!(count_where(&c, "groups", "id", "owner_g").await, 1);
    assert_eq!(count_where(&c, "channels", "group_id", "owner_g").await, 1);
}

/// Every table naming a user must be purged by account deletion or exempted with a reason.
#[tokio::test]
async fn every_user_scoped_table_is_accounted_for() {
    let c = conn().await;

    const USERISH: &[&str] = &[
        "user_id", "sender_id", "recipient_id", "blocker_id", "blocked_id",
        "inviter_id", "invitee_id", "requester_id", "created_by", "owner_id",
        "reviewed_by", "added_by",
    ];

    let mut tables: Vec<String> = Vec::new();
    {
        let mut rows = c
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
                (),
            )
            .await
            .unwrap();
        while let Some(r) = rows.next().await.unwrap() {
            tables.push(r.get(0).unwrap());
        }
    }
    assert!(tables.len() > 20, "schema probe found only {} tables", tables.len());

    let exempt: Vec<&str> = EXEMPT_FROM_USER_PURGE.iter().map(|(t, _)| *t).collect();
    let mut unaccounted: Vec<String> = Vec::new();

    for table in &tables {
        let mut cols: Vec<String> = Vec::new();
        let mut rows = c.query(&format!("PRAGMA table_info({table})"), ()).await.unwrap();
        while let Some(r) = rows.next().await.unwrap() {
            cols.push(r.get::<String>(1).unwrap());
        }
        // `users.id` is the anchor row, named by its primary key.
        let names_a_user =
            cols.iter().any(|col| USERISH.contains(&col.as_str())) || table == "users";
        if !names_a_user {
            continue;
        }
        if USER_PURGED_TABLES.contains(&table.as_str()) || exempt.contains(&table.as_str()) {
            continue;
        }
        unaccounted.push(table.clone());
    }

    assert!(
        unaccounted.is_empty(),
        "these tables name a user but account deletion neither purges them nor \
         exempts them: {unaccounted:?}. Add each to `teardown::purge_user_rows` \
         (and USER_PURGED_TABLES), or to EXEMPT_FROM_USER_PURGE with a reason."
    );
}

#[tokio::test]
async fn every_conversation_scoped_table_is_accounted_for() {
    let c = conn().await;

    const CONVISH: &[&str] = &["conversation_id", "group_id", "dm_channel_id", "channel_id"];

    let mut tables: Vec<String> = Vec::new();
    {
        let mut rows = c
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
                (),
            )
            .await
            .unwrap();
        while let Some(r) = rows.next().await.unwrap() {
            tables.push(r.get(0).unwrap());
        }
    }

    let exempt: Vec<&str> = EXEMPT_FROM_CONVERSATION_PURGE.iter().map(|(t, _)| *t).collect();
    let purged = pollis_delivery::teardown::CONVERSATION_PURGED_TABLES;
    let mut unaccounted: Vec<String> = Vec::new();

    for table in &tables {
        let mut cols: Vec<String> = Vec::new();
        let mut rows = c.query(&format!("PRAGMA table_info({table})"), ()).await.unwrap();
        while let Some(r) = rows.next().await.unwrap() {
            cols.push(r.get::<String>(1).unwrap());
        }
        let names_a_conversation = cols.iter().any(|col| CONVISH.contains(&col.as_str()))
            || table == "groups"
            || table == "channels"
            || table == "dm_channel";
        if !names_a_conversation {
            continue;
        }
        if purged.contains(&table.as_str()) || exempt.contains(&table.as_str()) {
            continue;
        }
        unaccounted.push(table.clone());
    }

    assert!(
        unaccounted.is_empty(),
        "these tables name a conversation but teardown neither purges them nor \
         exempts them: {unaccounted:?}"
    );
}

/// `purge_user_rows` is idempotent, so a retry after partial failure is safe.
#[tokio::test]
async fn purging_twice_is_a_no_op() {
    let c = conn().await;
    seed_user(&c, "victim", "bystander", true).await;

    purge_user_rows(&c, "victim").await.unwrap();
    let after_first: Vec<i64> = {
        let mut v = Vec::new();
        for (table, _) in USER_COLUMNS {
            v.push(count(&c, table).await);
        }
        v
    };
    purge_user_rows(&c, "victim").await.unwrap();
    for (i, (table, _)) in USER_COLUMNS.iter().enumerate() {
        assert_eq!(count(&c, table).await, after_first[i], "{table} changed on the second purge");
    }
}
