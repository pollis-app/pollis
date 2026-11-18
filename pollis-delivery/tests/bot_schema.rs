//! #1182 — the schema refuses every invalid bot state, by direct SQL with no
//! pollis-delivery code in the path (migration `000052_bots.sql`).
//!
//! Each test builds the state through the documented insert order and then
//! attempts one invalid write, asserting the database itself refuses it.

use libsql::Connection;

async fn conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    // Production parity: Turso runs with foreign keys off.
    c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
    pollis_schema::apply::single_db(&c).await.unwrap();
    for sql in [
        "INSERT INTO users (id, email, username) VALUES ('alice', 'a@x.com', 'alice')",
        "INSERT INTO users (id, email, username) VALUES ('bob', 'b@x.com', 'bob')",
        "INSERT INTO conversation (id, kind) VALUES ('g1', 'group')",
        "INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'One', 'alice')",
        "INSERT INTO conversation (id, kind) VALUES ('g2', 'group')",
        "INSERT INTO groups (id, name, owner_id) VALUES ('g2', 'Two', 'alice')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'alice', 'admin')",
    ] {
        ok(&c, sql).await;
    }
    c
}

async fn ok(c: &Connection, sql: &str) {
    c.execute(sql, ()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// Run `sql` and return the database's refusal; panics if it was accepted.
async fn refused(c: &Connection, sql: &str) -> String {
    match c.execute(sql, ()).await {
        Ok(_) => panic!("the schema accepted an invalid write: {sql}"),
        Err(e) => e.to_string(),
    }
}

async fn count(c: &Connection, sql: &str) -> i64 {
    let mut r = c.query(sql, ()).await.unwrap();
    r.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// A bot `B1` in `g1`, created in the documented order.
async fn with_bot(c: &Connection) {
    ok(c, "INSERT INTO bot (user_id, group_id, created_by) VALUES ('B1', 'g1', 'alice')").await;
    ok(c, "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'B1', 'member')").await;
    ok(c, "INSERT INTO users (id, email, username, kind) VALUES ('B1', 'bot-b1@bot.invalid', 'bot_b1', 'bot')").await;
}

#[tokio::test]
async fn a_bot_is_created_in_the_documented_order_and_deleted_in_reverse() {
    let c = conn().await;
    with_bot(&c).await;
    assert_eq!(count(&c, "SELECT COUNT(*) FROM users WHERE kind = 'bot'").await, 1);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM users WHERE kind = 'human'").await, 2, "existing rows default to human");

    ok(&c, "DELETE FROM users WHERE id = 'B1'").await;
    ok(&c, "DELETE FROM bot WHERE user_id = 'B1'").await;
    ok(&c, "DELETE FROM group_member WHERE user_id = 'B1'").await;
    assert_eq!(count(&c, "SELECT COUNT(*) FROM bot").await, 0);
}

#[tokio::test]
async fn a_bot_user_cannot_exist_without_its_bot_row() {
    let c = conn().await;
    ok(&c, "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'B1', 'member')").await;
    let e = refused(&c, "INSERT INTO users (id, email, username, kind) VALUES ('B1', 'bot-b1@bot.invalid', 'bot_b1', 'bot')").await;
    assert!(e.contains("bot row"), "{e}");
}

#[tokio::test]
async fn a_bot_user_cannot_exist_outside_its_group() {
    let c = conn().await;
    ok(&c, "INSERT INTO bot (user_id, group_id) VALUES ('B1', 'g1')").await;
    refused(&c, "INSERT INTO users (id, email, username, kind) VALUES ('B1', 'bot-b1@bot.invalid', 'bot_b1', 'bot')").await;
}

#[tokio::test]
async fn a_bot_user_carries_exactly_its_derived_email() {
    let c = conn().await;
    ok(&c, "INSERT INTO bot (user_id, group_id) VALUES ('B1', 'g1')").await;
    ok(&c, "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'B1', 'member')").await;
    for email in ["a@x.com", "bot-B1@bot.invalid", "bot-b2@bot.invalid", "bot-b1@bot.invalid.com", "bot-b1@pollis.com"] {
        refused(
            &c,
            &format!("INSERT INTO users (id, email, username, kind) VALUES ('B1', '{email}', 'bot_b1', 'bot')"),
        )
        .await;
    }
    ok(&c, "INSERT INTO users (id, email, username, kind) VALUES ('B1', 'bot-b1@bot.invalid', 'bot_b1', 'bot')").await;
    refused(&c, "UPDATE users SET email = 'x@y.com' WHERE id = 'B1'").await;
    refused(&c, "UPDATE users SET id = 'B2', email = 'bot-b2@bot.invalid' WHERE id = 'B1'").await;
}

#[tokio::test]
async fn a_human_never_uses_a_bot_invalid_address() {
    let c = conn().await;
    for email in ["c@bot.invalid", "C@BOT.INVALID", "c@sub.bot.invalid"] {
        refused(&c, &format!("INSERT INTO users (id, email, username) VALUES ('carol', '{email}', 'carol')")).await;
    }
    refused(&c, "UPDATE users SET email = 'alice@bot.invalid' WHERE id = 'alice'").await;
    // Not a bot.invalid address: a lookalike domain is an ordinary address.
    ok(&c, "INSERT INTO users (id, email, username) VALUES ('carol', 'c@robot.invalid', 'carol')").await;
}

#[tokio::test]
async fn kind_is_immutable_in_both_directions() {
    let c = conn().await;
    with_bot(&c).await;
    refused(&c, "UPDATE users SET kind = 'bot' WHERE id = 'alice'").await;
    refused(&c, "UPDATE users SET kind = 'human' WHERE id = 'B1'").await;
    refused(&c, "INSERT INTO users (id, email, username, kind) VALUES ('x', 'x@x.com', 'x', 'robot')").await;
}

#[tokio::test]
async fn no_human_becomes_a_bot_through_a_late_bot_row() {
    let c = conn().await;
    refused(&c, "INSERT INTO bot (user_id, group_id) VALUES ('bob', 'g1')").await;
    // A bot row first, then the id inserted as a human, is refused too.
    ok(&c, "INSERT INTO bot (user_id, group_id) VALUES ('B1', 'g1')").await;
    refused(&c, "INSERT INTO users (id, email, username) VALUES ('B1', 'b1@x.com', 'b1')").await;
}

#[tokio::test]
async fn a_bot_row_names_a_real_group() {
    let c = conn().await;
    refused(&c, "INSERT INTO bot (user_id, group_id) VALUES ('B1', 'nope')").await;
}

#[tokio::test]
async fn a_bot_is_scoped_to_one_group_for_life() {
    let c = conn().await;
    with_bot(&c).await;
    refused(&c, "UPDATE bot SET group_id = 'g2' WHERE user_id = 'B1'").await;
    refused(&c, "UPDATE bot SET user_id = 'B2' WHERE user_id = 'B1'").await;
}

#[tokio::test]
async fn a_bot_is_never_a_member_of_another_group() {
    let c = conn().await;
    with_bot(&c).await;
    refused(&c, "INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'B1', 'member')").await;
    refused(&c, "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES ('g2', 'B1', 'member')").await;
    refused(&c, "UPDATE group_member SET group_id = 'g2' WHERE user_id = 'B1'").await;
    // Another member's row re-pointed at the bot is refused the same way.
    ok(&c, "INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'bob', 'member')").await;
    refused(&c, "UPDATE group_member SET user_id = 'B1' WHERE group_id = 'g2' AND user_id = 'bob'").await;
    assert_eq!(count(&c, "SELECT COUNT(*) FROM group_member WHERE user_id = 'B1'").await, 1);
}

#[tokio::test]
async fn a_bot_is_never_an_admin() {
    let c = conn().await;
    ok(&c, "INSERT INTO bot (user_id, group_id) VALUES ('B1', 'g1')").await;
    refused(&c, "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'B1', 'admin')").await;
    ok(&c, "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'B1', 'member')").await;
    ok(&c, "INSERT INTO users (id, email, username, kind) VALUES ('B1', 'bot-b1@bot.invalid', 'bot_b1', 'bot')").await;
    refused(&c, "UPDATE group_member SET role = 'admin' WHERE user_id = 'B1'").await;
}

#[tokio::test]
async fn a_bot_leaves_its_group_only_by_being_deleted() {
    let c = conn().await;
    with_bot(&c).await;
    let e = refused(&c, "DELETE FROM group_member WHERE user_id = 'B1'").await;
    assert!(e.contains("only by being deleted"), "{e}");
    let e = refused(&c, "DELETE FROM bot WHERE user_id = 'B1'").await;
    assert!(e.contains("before its bot row"), "{e}");
}

#[tokio::test]
async fn a_bot_never_owns_a_group() {
    let c = conn().await;
    with_bot(&c).await;
    ok(&c, "INSERT INTO conversation (id, kind) VALUES ('g3', 'group')").await;
    refused(&c, "INSERT INTO groups (id, name, owner_id) VALUES ('g3', 'Three', 'B1')").await;
    refused(&c, "UPDATE groups SET owner_id = 'B1' WHERE id = 'g1'").await;
}

#[tokio::test]
async fn a_group_with_bots_cannot_be_deleted_before_them() {
    let c = conn().await;
    with_bot(&c).await;
    refused(&c, "DELETE FROM groups WHERE id = 'g1'").await;
    ok(&c, "DELETE FROM groups WHERE id = 'g2'").await;
}

#[tokio::test]
async fn a_bot_is_never_in_a_dm() {
    let c = conn().await;
    with_bot(&c).await;
    ok(&c, "INSERT INTO conversation (id, kind) VALUES ('d1', 'dm')").await;
    ok(&c, "INSERT INTO dm_channel (id, created_by) VALUES ('d1', 'alice')").await;
    ok(&c, "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('d1', 'alice', 'alice')").await;
    refused(&c, "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('d1', 'B1', 'alice')").await;
    refused(&c, "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('d1', 'B1', 'alice')").await;
    refused(&c, "UPDATE dm_channel_member SET user_id = 'B1' WHERE user_id = 'alice'").await;
}

#[tokio::test]
async fn a_bot_is_never_invited_and_never_asks_to_join() {
    let c = conn().await;
    with_bot(&c).await;
    refused(&c, "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('i1', 'g2', 'alice', 'B1')").await;
    refused(&c, "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('i2', 'g1', 'B1', 'bob')").await;
    refused(&c, "INSERT INTO group_join_request (id, group_id, requester_id) VALUES ('j1', 'g2', 'B1')").await;
}

#[tokio::test]
async fn a_bot_never_holds_a_push_token() {
    let c = conn().await;
    with_bot(&c).await;
    refused(&c, "INSERT INTO push_token (token, user_id, platform, updated_at) VALUES ('t1', 'B1', 'ios', 'now')").await;
    ok(&c, "INSERT INTO push_token (token, user_id, platform, updated_at) VALUES ('t2', 'alice', 'ios', 'now')").await;
    refused(&c, "UPDATE push_token SET user_id = 'B1' WHERE token = 't2'").await;
}

#[tokio::test]
async fn a_trigger_is_claimed_once_and_only_by_a_bot() {
    let c = conn().await;
    with_bot(&c).await;
    refused(&c, "INSERT INTO bot_trigger_claim (installation_id, trigger_key, device_id, claimed_at) VALUES ('alice', 'k', 'd', 1)").await;
    ok(&c, "INSERT INTO bot_trigger_claim (installation_id, trigger_key, device_id, claimed_at) VALUES ('B1', 'k', 'd1', 1)").await;
    refused(&c, "INSERT INTO bot_trigger_claim (installation_id, trigger_key, device_id, claimed_at) VALUES ('B1', 'k', 'd2', 2)").await;
}

#[tokio::test]
async fn a_pairing_request_has_exactly_its_kinds_shape() {
    let c = conn().await;
    let key = "zeroblob(32)";
    let account = "zeroblob(1312)";
    // 'create' needs the account key; 'add_host' needs the bot and its group, and no key.
    refused(&c, &format!(
        "INSERT INTO bot_pairing_request (id, kind, code_lookup, poll_verifier, ephemeral_pub, device_id, expires_at) \
         VALUES ('r1', 'create', {key}, {key}, {key}, 'd', 1)"
    )).await;
    refused(&c, &format!(
        "INSERT INTO bot_pairing_request (id, kind, code_lookup, poll_verifier, ephemeral_pub, device_id, expires_at) \
         VALUES ('r1', 'add_host', {key}, {key}, {key}, 'd', 1)"
    )).await;
    refused(&c, &format!(
        "INSERT INTO bot_pairing_request (id, kind, code_lookup, poll_verifier, ephemeral_pub, device_id, account_id_pub, user_id, group_id, expires_at) \
         VALUES ('r1', 'add_host', {key}, {key}, {key}, 'd', {account}, 'B1', 'g1', 1)"
    )).await;
    // Approved without who approved it, or an add_host approved without a wrap.
    refused(&c, &format!(
        "INSERT INTO bot_pairing_request (id, kind, code_lookup, poll_verifier, ephemeral_pub, device_id, account_id_pub, user_id, group_id, status, expires_at) \
         VALUES ('r1', 'create', {key}, {key}, {key}, 'd', {account}, 'B1', 'g1', 'approved', 1)"
    )).await;
    refused(&c, &format!(
        "INSERT INTO bot_pairing_request (id, kind, code_lookup, poll_verifier, ephemeral_pub, device_id, user_id, group_id, status, reviewed_by, approver_device_id, expires_at) \
         VALUES ('r1', 'add_host', {key}, {key}, {key}, 'd', 'B1', 'g1', 'approved', 'alice', 'ad', 1)"
    )).await;
    // A wrong-length key.
    refused(&c,
        "INSERT INTO bot_pairing_request (id, kind, code_lookup, poll_verifier, ephemeral_pub, device_id, account_id_pub, expires_at) \
         VALUES ('r1', 'create', zeroblob(32), zeroblob(32), zeroblob(31), 'd', zeroblob(1312), 1)"
    ).await;
    ok(&c, &format!(
        "INSERT INTO bot_pairing_request (id, kind, code_lookup, poll_verifier, ephemeral_pub, device_id, account_id_pub, expires_at) \
         VALUES ('r1', 'create', {key}, {key}, {key}, 'd', {account}, 1)"
    )).await;
}
