//! Explicit teardown. Production Turso runs with `foreign_keys=OFF`, so the
//! schema's `ON DELETE CASCADE` clauses never fire: every row that should go is
//! deleted by name here.
//!
//! A new remote table naming a user, group, channel or DM must be added to the
//! matching function and list below, or to an exemption list with a reason;
//! `tests/teardown.rs` reads the live schema and enforces it.
//!
//! Every statement is bound to the id being torn down, reaching other rows only
//! through subqueries anchored on that id (#878). Children go before parents,
//! anchor row last, so a partial application stays monotone; re-running is a
//! no-op.

use libsql::Connection;

/// Every table account deletion clears ([`purge_user_rows`] plus the caller's
/// `users` delete and log-DB purges). With [`EXEMPT_FROM_USER_PURGE`] it
/// partitions the schema.
pub const USER_PURGED_TABLES: &[&str] = &[
    "account_recovery",
    "directory_lookup_budget",
    "attachment_ref",
    "channel_mute",
    "conversation_watermark",
    "device_enrollment_request",
    "dm_channel",
    "dm_channel_member",
    "group_emoji",
    "group_invite",
    "group_invite_link",
    "group_invite_link_redemption",
    "group_join_request",
    "group_member",
    "groups",
    "message_envelope",
    "message_reaction",
    "mls_key_package",
    // Log-DB tables, purged by `writes::purge_welcomes` and
    // `purge_user_log_rows` after the main transaction commits.
    "mls_commit_since",
    "mls_welcome",
    "pinned_message",
    "push_handle",
    "push_ticket",
    "push_token",
    "read_cursor_sync",
    "security_event",
    "user_block",
    "user_device",
    "user_preferences",
    "users",
    "vault_attachment_ref",
    "vault_message",
    "user_report",
    "account_suspension",
];

/// Every table a conversation teardown clears — [`purge_conversation_rows`],
/// [`purge_group`], [`purge_dm_channel`] and [`purge_conversation_log`] between
/// them.
pub const CONVERSATION_PURGED_TABLES: &[&str] = &[
    "attachment_ref",
    "channel_mute",
    "channels",
    "conversation_seq",
    "conversation_watermark",
    "dm_channel",
    "dm_channel_member",
    "group_emoji",
    "group_invite",
    "group_invite_link",
    "group_invite_link_redemption",
    "group_join_request",
    "group_member",
    "groups",
    "message_envelope",
    "message_reaction",
    "mls_commit_log",
    "mls_commit_since",
    "mls_group_info",
    "mls_welcome",
    "file_keystate",
    "pin_keystate",
    "pinned_message",
    "push_handle",
];

/// Tables that deliberately survive an account deletion, and why. A decision
/// record, read by `tests/teardown.rs`.
pub const EXEMPT_FROM_USER_PURGE: &[(&str, &str)] = &[
    (
        "account_key_log",
        "Append-only account-key transparency log (migration 000005). Its rows are \
         already published in the account tenant's Merkle tree at verify.pollis.com; \
         deleting them would break the client-verifiable proof that an account's key \
         never changed behind a peer's back, for every peer that ever verified it. \
         docs/metadata-retention-policy.md section 8 states this is permanent.",
    ),
    (
        "mls_commit_log",
        "Append-only MLS epoch chain, shared by every member of a conversation and \
         NOT the departing user's to remove. The baseline's `sender_id REFERENCES \
         users(id) ON DELETE CASCADE` is actively dangerous and inert only because \
         Turso has foreign keys off: firing it would erase a departed member's \
         commits from the middle of the chain, leaving permanent epoch gaps that \
         stop every REMAINING member from advancing. Commit rows do go when the \
         whole conversation goes (`purge_conversation_log`), and are pruned by \
         generation in `commit.rs`.",
    ),
    (
        "conversation",
        "The conversation-id registry (migration 000016) is append-only by design: \
         releasing an id re-opens the id-reuse attack that table exists to close. \
         It carries an id and a kind, with no user linkage.",
    ),
    (
        "attachment_object",
        "Content-addressed and shared across users; reference-counted (#690). \
         Collected when the last referencing message goes, which account teardown \
         causes by deleting the envelopes and `attachment_ref` rows.",
    ),
    (
        "custom_emoji_object",
        "Content-addressed and shared across groups (#848); collected by \
         `POST /v1/emoji/gc` once no `group_emoji` row names its hash. Teardown \
         releases the references, the sweep collects the object.",
    ),
    (
        "schema_migrations",
        "Migration bookkeeping. No user data.",
    ),
    (
        "pin_keystate",
        "Conversation-owned, not user-owned (#99): the one wrapped pin key the \
         REMAINING members unwrap to read the conversation's pins. Deleting it on \
         a member's departure would strip everyone else's pin access; it goes with \
         its conversation (CONVERSATION_PURGED_TABLES). Server-unreadable \
         ciphertext; `updated_by_device_id` is diagnostic, and any member's next \
         re-wrap replaces the row.",
    ),    (
        "file_keystate",
        "Conversation-owned, not user-owned (#1382): the wrapped attachment key \
         ring the REMAINING members unwrap to open the conversation's files. Goes \
         with its conversation (CONVERSATION_PURGED_TABLES). Server-unreadable \
         ciphertext; `updated_by_device_id` is diagnostic.",
    ),
];

/// Tables that deliberately survive a conversation (group / channel / DM)
/// teardown, and why.
pub const EXEMPT_FROM_CONVERSATION_PURGE: &[(&str, &str)] = &[
    (
        "conversation",
        "Append-only id registry (migration 000016) — see EXEMPT_FROM_USER_PURGE.",
    ),
    (
        "attachment_object",
        "Shared and reference-counted; released, not deleted, by teardown.",
    ),
    (
        "custom_emoji_object",
        "Shared and reference-counted; released, not deleted, by teardown.",
    ),
    ("schema_migrations", "Migration bookkeeping. No user data."),
    (
        "vault_message",
        "User-owned, not conversation-owned (#107): the vault is a personal \
         space with no conversation id. Purged with its account.",
    ),
    (
        "vault_attachment_ref",
        "User-owned via its vault entry (#107); purged with the account, and \
         reaped by the envelope-GC sweep if ever orphaned.",
    ),
    (
        "user_report",
        "An abuse report names an account, not a conversation (#1213). Its \
         conversation_id is context for the operator and points at no stored \
         content; the report lives until the reporter or reported account is \
         deleted, which purges it.",
    ),
];

/// Delete every main-DB row keyed to `user_id` except the `users` row, which
/// account deletion removes last and identity reset
/// ([`crate::account::apply_reset_recover`]) keeps.
pub async fn purge_user_rows(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    let uid = || libsql::params![user_id.to_string()];
    let uid2 = || libsql::params![user_id.to_string(), user_id.to_string()];

    // Children of this user's envelopes, before the envelopes go (or the
    // subquery matches nothing). Includes other users' reactions on them.
    conn.execute(
        "DELETE FROM attachment_ref WHERE message_id IN \
         (SELECT id FROM message_envelope WHERE sender_id = ?1)",
        uid(),
    )
    .await?;
    conn.execute(
        "DELETE FROM message_reaction WHERE message_id IN \
         (SELECT id FROM message_envelope WHERE sender_id = ?1)",
        uid(),
    )
    .await?;

    // Sealed-sender envelopes (#607) carry a sentinel `sender_id` and are not
    // matched; they go with their conversation or the #720 watermark sweep.
    conn.execute("DELETE FROM message_envelope WHERE sender_id = ?1", uid()).await?;

    conn.execute("DELETE FROM message_reaction WHERE user_id = ?1", uid()).await?;

    conn.execute("DELETE FROM directory_lookup_budget WHERE user_id = ?1", uid()).await?;

    conn.execute("DELETE FROM group_member WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM dm_channel_member WHERE user_id = ?1", uid()).await?;
    conn.execute(
        "DELETE FROM user_block WHERE blocker_id = ?1 OR blocked_id = ?2",
        uid2(),
    )
    .await?;
    conn.execute(
        "DELETE FROM user_report WHERE reporter_id = ?1 OR reported_id = ?2",
        uid2(),
    )
    .await?;
    conn.execute("DELETE FROM account_suspension WHERE user_id = ?1", uid()).await?;

    conn.execute(
        "DELETE FROM group_invite WHERE inviter_id = ?1 OR invitee_id = ?2",
        uid2(),
    )
    .await?;
    conn.execute("DELETE FROM group_join_request WHERE requester_id = ?1", uid()).await?;
    // Audit pointers on other users' rows are nulled, not deleted: the
    // declared `ON DELETE SET NULL`, done by hand, before the links go.
    conn.execute(
        "UPDATE group_join_request SET reviewed_by = NULL WHERE reviewed_by = ?1",
        uid(),
    )
    .await?;
    conn.execute(
        "UPDATE group_invite_link_redemption SET link_id = NULL \
         WHERE link_id IN (SELECT id FROM group_invite_link WHERE created_by = ?1)",
        uid(),
    )
    .await?;
    conn.execute("DELETE FROM group_invite_link_redemption WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM group_invite_link WHERE created_by = ?1", uid()).await?;

    // `created_by` is NOT NULL, so the row goes; the object is released for
    // `POST /v1/emoji/gc`.
    conn.execute("DELETE FROM group_emoji WHERE created_by = ?1", uid()).await?;

    conn.execute("DELETE FROM user_device WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM device_enrollment_request WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM mls_key_package WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM conversation_watermark WHERE user_id = ?1", uid()).await?;
    // Tickets name only a token, so they go via the user's tokens first (#1423).
    conn.execute(
        "DELETE FROM push_ticket WHERE token IN (SELECT token FROM push_token WHERE user_id = ?1)",
        uid(),
    )
    .await?;
    conn.execute("DELETE FROM push_token WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM push_handle WHERE user_id = ?1", uid()).await?;

    conn.execute("DELETE FROM account_recovery WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM user_preferences WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM read_cursor_sync WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM channel_mute WHERE user_id = ?1", uid()).await?;
    // Vault references first, while the subquery can still see the entries.
    conn.execute(
        "DELETE FROM vault_attachment_ref WHERE vault_message_id IN \
         (SELECT id FROM vault_message WHERE user_id = ?1)",
        uid(),
    )
    .await?;
    conn.execute("DELETE FROM vault_message WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM pinned_message WHERE pinned_by = ?1", uid()).await?;
    conn.execute("DELETE FROM security_event WHERE user_id = ?1", uid()).await?;

    hand_over_dangling_pointers(conn, user_id).await?;

    Ok(())
}

/// Hand over the `NOT NULL` columns naming this user on rows that outlive them:
/// `groups.owner_id` (display only) and `dm_channel.created_by` (an authz input
/// for `apply_remove_dm_member`, so it must name someone who can still sign in)
/// go to a remaining member; the audit field `dm_channel_member.added_by`
/// collapses to a self-reference.
async fn hand_over_dangling_pointers(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    let uid = || libsql::params![user_id.to_string()];

    // Prefer an admin. The EXISTS guard leaves a memberless group (being torn
    // down by the caller) unchanged rather than writing NULL.
    conn.execute(
        "UPDATE groups SET owner_id = ( \
             SELECT gm.user_id FROM group_member gm \
             WHERE gm.group_id = groups.id AND gm.user_id != ?1 \
             ORDER BY CASE WHEN gm.role = 'admin' THEN 0 ELSE 1 END, gm.user_id \
             LIMIT 1) \
         WHERE owner_id = ?1 \
           AND EXISTS (SELECT 1 FROM group_member gm2 \
                       WHERE gm2.group_id = groups.id AND gm2.user_id != ?1)",
        uid(),
    )
    .await?;

    conn.execute(
        "UPDATE dm_channel SET created_by = ( \
             SELECT dcm.user_id FROM dm_channel_member dcm \
             WHERE dcm.dm_channel_id = dm_channel.id AND dcm.user_id != ?1 \
             ORDER BY dcm.user_id LIMIT 1) \
         WHERE created_by = ?1 \
           AND EXISTS (SELECT 1 FROM dm_channel_member dcm2 \
                       WHERE dcm2.dm_channel_id = dm_channel.id AND dcm2.user_id != ?1)",
        uid(),
    )
    .await?;

    conn.execute(
        "UPDATE dm_channel_member SET added_by = user_id WHERE added_by = ?1",
        uid(),
    )
    .await?;

    Ok(())
}

/// Delete every main-DB row of one conversation's history. `conversation_id` is
/// a channel or DM channel id (one namespace, migration 000016). The child
/// subqueries run before the envelopes are deleted.
pub async fn purge_conversation_rows(
    conn: &Connection,
    conversation_id: &str,
) -> anyhow::Result<()> {
    let cid = || libsql::params![conversation_id.to_string()];

    conn.execute(
        "DELETE FROM attachment_ref WHERE message_id IN \
         (SELECT id FROM message_envelope WHERE conversation_id = ?1)",
        cid(),
    )
    .await?;
    // By conversation too (#1433), for reactions whose envelope GC collected.
    conn.execute(
        "DELETE FROM message_reaction WHERE conversation_id = ?1 OR message_id IN \
         (SELECT id FROM message_envelope WHERE conversation_id = ?1)",
        cid(),
    )
    .await?;
    conn.execute("DELETE FROM message_envelope WHERE conversation_id = ?1", cid()).await?;
    conn.execute("DELETE FROM conversation_watermark WHERE conversation_id = ?1", cid()).await?;
    // GC must never remove the sequence counter (#1087); teardown must.
    conn.execute("DELETE FROM conversation_seq WHERE conversation_id = ?1", cid()).await?;
    conn.execute("DELETE FROM push_handle WHERE conversation_id = ?1", cid()).await?;
    // Keystate rows are keyed by the MLS conversation id: this id for a DM, the
    // group id for a channel (see `purge_group`).
    conn.execute("DELETE FROM pinned_message WHERE conversation_id = ?1", cid()).await?;
    conn.execute("DELETE FROM pin_keystate WHERE conversation_id = ?1", cid()).await?;
    conn.execute("DELETE FROM file_keystate WHERE conversation_id = ?1", cid()).await?;
    conn.execute("DELETE FROM channel_mute WHERE channel_id = ?1", cid()).await?;
    Ok(())
}

/// Drop `user_id`'s channel mutes (#1431) for every channel of `group_id` on
/// leave or removal, so a mute does not come back on rejoin.
pub async fn purge_member_channel_mutes(
    conn: &Connection,
    group_id: &str,
    user_id: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM channel_mute WHERE user_id = ?1 \
         AND channel_id IN (SELECT id FROM channels WHERE group_id = ?2)",
        libsql::params![user_id.to_string(), group_id.to_string()],
    )
    .await?;
    Ok(())
}

/// Tear a whole group down. Returns the destroyed channel ids for the caller's
/// [`purge_conversation_log`] after the main transaction commits.
pub async fn purge_group(conn: &Connection, group_id: &str) -> anyhow::Result<Vec<String>> {
    let gid = || libsql::params![group_id.to_string()];

    let mut channel_ids: Vec<String> = Vec::new();
    {
        let mut rows = conn
            .query("SELECT id FROM channels WHERE group_id = ?1", gid())
            .await?;
        while let Some(row) = rows.next().await? {
            channel_ids.push(row.get(0)?);
        }
    }
    for channel_id in &channel_ids {
        purge_conversation_rows(conn, channel_id).await?;
    }
    conn.execute("DELETE FROM channels WHERE group_id = ?1", gid()).await?;

    crate::emoji::release_group_emoji(conn, group_id).await?;

    // Redemption rows can belong to other users: null `link_id`, don't delete.
    conn.execute(
        "UPDATE group_invite_link_redemption SET link_id = NULL \
         WHERE link_id IN (SELECT id FROM group_invite_link WHERE group_id = ?1)",
        gid(),
    )
    .await?;
    conn.execute("DELETE FROM group_invite_link WHERE group_id = ?1", gid()).await?;
    conn.execute("DELETE FROM group_invite WHERE group_id = ?1", gid()).await?;
    conn.execute("DELETE FROM group_join_request WHERE group_id = ?1", gid()).await?;
    conn.execute("DELETE FROM group_member WHERE group_id = ?1", gid()).await?;

    conn.execute("DELETE FROM pin_keystate WHERE conversation_id = ?1", gid()).await?;
    conn.execute("DELETE FROM file_keystate WHERE conversation_id = ?1", gid()).await?;

    conn.execute("DELETE FROM groups WHERE id = ?1", gid()).await?;

    Ok(channel_ids)
}

/// Tear a whole DM channel down. The caller has established it has no members
/// left, or is destroying it regardless.
pub async fn purge_dm_channel(conn: &Connection, dm_channel_id: &str) -> anyhow::Result<()> {
    let did = || libsql::params![dm_channel_id.to_string()];

    purge_conversation_rows(conn, dm_channel_id).await?;
    conn.execute("DELETE FROM dm_channel_member WHERE dm_channel_id = ?1", did()).await?;
    conn.execute("DELETE FROM dm_channel WHERE id = ?1", did()).await?;
    Ok(())
}

/// Purge `user_id`'s `mls_commit_since` rows from the commit-log DB.
/// `mls_welcome` is [`crate::writes::purge_welcomes`]'s; `mls_commit_log` is
/// exempt. Run after the main transaction commits, like
/// [`purge_conversation_log`].
pub async fn purge_user_log_rows(log_conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    log_conn
        .execute(
            "DELETE FROM mls_commit_since WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    Ok(())
}

/// Purge one device's commit-log-DB state (device revocation and logout).
pub async fn purge_device_log_rows(
    log_conn: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<()> {
    log_conn
        .execute(
            "DELETE FROM mls_commit_since WHERE user_id = ?1 AND device_id = ?2",
            libsql::params![user_id.to_string(), device_id.to_string()],
        )
        .await?;
    Ok(())
}

/// Purge the commit-log DB's rows for conversations that no longer exist. The
/// log DB cannot share the main transaction, so callers must run this after
/// the main transaction commits: a rollback then leaves only recoverable
/// leftovers, never a live conversation without its history.
pub async fn purge_conversation_log(
    log_conn: &Connection,
    conversation_ids: &[String],
) -> anyhow::Result<()> {
    for id in conversation_ids {
        let cid = || libsql::params![id.to_string()];
        log_conn
            .execute("DELETE FROM mls_group_info WHERE conversation_id = ?1", cid())
            .await?;
        log_conn
            .execute("DELETE FROM mls_welcome WHERE conversation_id = ?1", cid())
            .await?;
        log_conn
            .execute("DELETE FROM mls_commit_since WHERE conversation_id = ?1", cid())
            .await?;
        log_conn
            .execute("DELETE FROM mls_commit_log WHERE conversation_id = ?1", cid())
            .await?;
    }
    Ok(())
}
