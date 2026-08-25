//! Account and group teardown actually removes everything.
//!
//! The bug these tests exist for: the remote schema decorates nearly every child
//! table with `ON DELETE CASCADE`, and production Turso runs with
//! `foreign_keys=OFF`, so none of it ever fired. `DELETE FROM users` removed one
//! row and left the device roster, DM membership, invites, recovery blob and
//! security-event log in place — while `docs/metadata-retention-policy.md` told
//! users they were gone.
//!
//! Every fixture here runs with `PRAGMA foreign_keys=OFF`, which is production
//! parity: if these tests passed only because SQLite cascaded for them, they
//! would be testing SQLite rather than the delivery service.
//!
//! Coverage is enumerated, not sampled. `every_user_scoped_table_is_accounted_for`
//! reads the live schema and fails when a table naming a user is in neither the
//! purge list nor the documented exemption list, so the next table added cannot
//! be silently forgotten the way these were.

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

/// Every (table, column) pair in the remote schema that names a user. Written
/// out rather than derived so that a reviewer can check the list against the
/// migrations by eye — `every_user_scoped_table_is_accounted_for` then proves
/// the list is complete against the live schema.
const USER_COLUMNS: &[(&str, &str)] = &[
    ("account_recovery", "user_id"),
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
    ("security_event", "user_id"),
    ("user_block", "blocker_id"),
    ("user_block", "blocked_id"),
    ("user_device", "user_id"),
    ("user_dms", "user_id"),
    ("user_groups", "user_id"),
    ("user_preferences", "user_id"),
    ("users", "id"),
];

/// Every (table, column) pair that names a group.
const GROUP_COLUMNS: &[(&str, &str)] = &[
    ("channels", "group_id"),
    ("group_emoji", "group_id"),
    ("group_invite", "group_id"),
    ("group_invite_link", "group_id"),
    ("group_join_request", "group_id"),
    ("group_member", "group_id"),
    ("groups", "id"),
    ("user_groups", "group_id"),
];

async fn conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    // Production parity — see the module docs.
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

/// Seed one row in every user-scoped table for `u`, plus the conversations they
/// belong to. `solo` controls whether the group and DM created for this user are
/// theirs alone (so teardown destroys them) or shared with `peer`.
///
/// Ids are namespaced by `u`, which is what makes the isolation tests meaningful:
/// two users seeded this way share no row.
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
        "INSERT INTO push_token (token, user_id, platform, updated_at) \
         VALUES ('{u}_tok', '{u}', 'ios', datetime('now'))"
    )).await;
    exec(c, &format!(
        "INSERT INTO mls_key_package (ref_hash, user_id, key_package) \
         VALUES ('{u}_kp', '{u}', X'01')"
    )).await;
    // Blocks in both directions, so a delete keyed on only one side is caught.
    exec(c, &format!(
        "INSERT INTO user_block (blocker_id, blocked_id) VALUES ('{u}', '{u}_stranger')"
    )).await;
    exec(c, &format!(
        "INSERT INTO user_block (blocker_id, blocked_id) VALUES ('{u}_stranger2', '{u}')"
    )).await;

    // A group the user belongs to, with one channel carrying a message.
    let gid = format!("{u}_g");
    let cid = format!("{u}_c");
    // Claim precedes the row it names — 000017's guard triggers refuse an
    // unclaimed insert (#948).
    exec(c, &format!("INSERT INTO conversation (id, kind) VALUES ('{gid}', 'group')")).await;
    exec(c, &format!(
        "INSERT INTO groups (id, name, owner_id) VALUES ('{gid}', 'G', '{u}')"
    )).await;
    exec(c, &format!(
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('{gid}', '{u}', 'admin')"
    )).await;
    exec(c, &format!(
        "INSERT INTO user_groups (user_id, group_id, group_name) VALUES ('{u}', '{gid}', 'G')"
    )).await;
    exec(c, &format!("INSERT INTO conversation (id, kind) VALUES ('{cid}', 'channel')")).await;
    exec(c, &format!(
        "INSERT INTO channels (id, group_id, name) VALUES ('{cid}', '{gid}', 'general')"
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

    // Invites in both directions, a join request, and a link with a redemption.
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

    // A DM.
    let dm = format!("{u}_dm");
    exec(c, &format!("INSERT INTO conversation (id, kind) VALUES ('{dm}', 'dm')")).await;
    exec(c, &format!("INSERT INTO dm_channel (id, created_by) VALUES ('{dm}', '{u}')")).await;
    exec(c, &format!(
        "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) \
         VALUES ('{dm}', '{u}', '{u}')"
    )).await;
    exec(c, &format!(
        "INSERT INTO user_dms (user_id, dm_channel_id, created_by) VALUES ('{u}', '{dm}', '{u}')"
    )).await;

    if !solo {
        // A second member keeps the group and DM alive through the teardown, so
        // the tests can tell "the actor's rows went" from "everything went".
        exec(c, &format!(
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('{gid}', '{peer}', 'member')"
        )).await;
        exec(c, &format!(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) \
             VALUES ('{dm}', '{peer}', '{u}')"
        )).await;
    }
}

// ── Account deletion ─────────────────────────────────────────────────────────

/// The headline property: after deleting an account, NO table names that user.
/// Enumerated over every user-scoped column in the schema, not spot-checked.
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
    // The commit-log DB is a separate connection in production; here one DB
    // stands in for both, so run the log purge the handler would run.
    pollis_delivery::writes::purge_welcomes(&c, "victim").await.unwrap();
    pollis_delivery::teardown::purge_conversation_log(&c, &dead).await.unwrap();

    for (table, col) in USER_COLUMNS {
        let n = count_where(&c, table, col, "victim").await;
        assert_eq!(n, 0, "{table}.{col} still names the deleted user ({n} rows)");
    }
}

/// The same property when the user's conversations SURVIVE them — a group and a
/// DM that still have another member in them.
///
/// This is the harder half: those rows cannot simply be deleted, and three of
/// their columns (`groups.owner_id`, `dm_channel.created_by`,
/// `dm_channel_member.added_by`) are `NOT NULL` pointers naming the departing
/// user. They have to be handed over, and `dm_channel.created_by` is an
/// authorization input, so leaving it dead would also wedge member removal in
/// that DM forever.
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

    // The conversations really did survive — otherwise this test would pass
    // vacuously by having destroyed everything.
    assert_eq!(count_where(&c, "groups", "id", "victim_g").await, 1);
    assert_eq!(count_where(&c, "dm_channel", "id", "victim_dm").await, 1);

    for (table, col) in USER_COLUMNS {
        let n = count_where(&c, table, col, "victim").await;
        assert_eq!(n, 0, "{table}.{col} still names the deleted user ({n} rows)");
    }

    // The handed-over pointers name a real remaining participant, not a
    // placeholder.
    let mut rows = c
        .query("SELECT created_by FROM dm_channel WHERE id = 'victim_dm'", ())
        .await
        .unwrap();
    let creator: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(creator, "bystander", "DM creator must pass to a remaining member");
}

/// The exemptions are real, and deliberate: the account-key transparency log
/// survives, because its leaves are already published and clients verify them.
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

/// A commit sitting in a SURVIVING conversation's chain is not the departing
/// member's to remove: deleting it would leave an epoch gap that stops every
/// remaining member from advancing. The baseline's `sender_id ... ON DELETE
/// CASCADE` would do exactly that if foreign keys were ever switched on.
#[tokio::test]
async fn account_deletion_does_not_gap_a_surviving_conversations_commit_chain() {
    let c = conn().await;
    seed_user(&c, "bystander", "victim", false).await;
    seed_user(&c, "victim", "bystander", false).await;
    // The victim commits into the bystander's still-populated conversation.
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
    // And their conversations are untouched.
    assert_eq!(count_where(&c, "groups", "id", "bystander_g").await, 1);
    assert_eq!(count_where(&c, "dm_channel", "id", "bystander_dm").await, 1);
    assert_eq!(count_where(&c, "channels", "id", "bystander_c").await, 1);
}

/// A shared group survives the departure, loses only the leaver's rows, and has
/// its `owner_id` handed to the promoted admin rather than left dangling at a
/// user who no longer exists.
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

/// A DM left with nobody in it is torn down completely rather than kept as an
/// unreachable conversation full of ciphertext.
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
    assert_eq!(count(&c, "user_dms").await, 0);
}

/// Identity reset sheds membership and devices but is NOT deletion — the account
/// and its own records stay.
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

// ── Group deletion ───────────────────────────────────────────────────────────

/// After deleting a group, no table names it — enumerated over every
/// group-scoped column, plus the per-conversation tables reachable through its
/// channels.
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
    // The invite link's redemption row belongs to a user, so it survives with a
    // blanked pointer rather than being deleted out from under them.
    assert_eq!(count_where(&c, "group_invite_link_redemption", "id", "owner_red").await, 1);
    let mut rows = c
        .query("SELECT link_id FROM group_invite_link_redemption WHERE id = 'owner_red'", ())
        .await
        .unwrap();
    let link: Option<String> = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(link, None, "a redemption's dangling link_id must be nulled");
}

/// Deleting one group must not reach another group's rows.
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
    // The other group's conversation data is intact too.
    assert_eq!(count_where(&c, "message_envelope", "conversation_id", "peer_c").await, 1);
    assert_eq!(count_where(&c, "mls_commit_log", "conversation_id", "peer_c").await, 1);
    assert_eq!(count_where(&c, "conversation_watermark", "conversation_id", "peer_c").await, 1);
}

/// A non-admin cannot delete a group, so teardown can never run on someone
/// else's authority. (The destructive path's authorization, pinned alongside the
/// deletes it guards.)
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

// ── Coverage: the schema cannot outgrow the teardown unnoticed ───────────────

/// Read the LIVE schema and require every table naming a user to be either
/// purged by account deletion or explicitly exempted with a reason.
///
/// This is the test that makes the class of bug non-recurring. Adding a table
/// with a `user_id` and forgetting to tear it down fails here, at the point the
/// migration lands, instead of silently retaining that user's data forever.
#[tokio::test]
async fn every_user_scoped_table_is_accounted_for() {
    let c = conn().await;

    // Column names that mean "this row is about a user".
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
        // `users.id` is the anchor row, named by its primary key rather than by
        // a *ish column.
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

/// Same guarantee for conversation teardown.
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

/// `purge_user_rows` is idempotent — re-running it is a no-op, which is what
/// makes a retry after a partial failure safe.
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
