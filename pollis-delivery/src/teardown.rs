//! Explicit teardown — the deletes that `ON DELETE CASCADE` was supposed to do
//! and never did on any deployment.
//!
//! # Why this module exists
//!
//! The baseline schema decorates almost every child table with `REFERENCES
//! users(id) ON DELETE CASCADE` (and the same for `groups`, `channels`,
//! `dm_channel`). **Production Turso runs with `foreign_keys=OFF`**, so not one
//! of those clauses has ever fired on a real deploy — see
//! [`crate::db::LOCAL_PRAGMAS`], which mirrors the production setting in this
//! crate's own fixtures, and migration `000016`, which keeps the conversation
//! registry append-only for precisely this reason.
//!
//! The consequence was a data-retention bug, not a cosmetic one. "Delete my
//! account" ran `DELETE FROM users` and trusted the cascade for the device
//! roster, DM membership, invites, recovery blob, security-event log and a dozen
//! other tables. Every one of those rows survived. For a messenger whose product
//! claim is that the server holds metadata you can actually remove, that claim
//! was false.
//!
//! So: no implicit deletes anywhere. Every row that should go is named here.
//!
//! # The rule for adding a table
//!
//! Any new remote table carrying a user, group, channel or DM identifier MUST be
//! added to the matching function below, or added to [`EXEMPT_FROM_USER_PURGE`] /
//! [`EXEMPT_FROM_CONVERSATION_PURGE`] with a reason. `tests/teardown.rs`
//! reads the live schema and fails if a table is in neither list, so a table
//! cannot be forgotten the way these were. That test is the actual guarantee;
//! this doc comment is only its explanation.
//!
//! # Scoping — the #878 defect class
//!
//! #878 was "authorised on one field, deleted by another". Every statement here
//! is bound to the id of the thing being torn down, and where a delete has to
//! reach rows via another table it does so through a subquery anchored on that
//! same id (never a join that could widen). Nothing here re-derives a target
//! from client input; callers pass an id they have already authorised.
//!
//! # Ordering
//!
//! Children before parents, anchor row last. Callers run these inside one
//! transaction, so ordering is not what makes them atomic — it is what makes a
//! *partial* application (a crash mid-teardown on a deploy where the transaction
//! is degraded, a manual replay) monotone toward the goal instead of leaving an
//! anchor row whose children are unreachable. Re-running any of these functions
//! is a no-op, by design.

use libsql::Connection;

/// Every table [`purge_user_rows`] clears (plus `users`, which its caller
/// deletes). Paired with [`EXEMPT_FROM_USER_PURGE`] this is the complete
/// partition of the schema with respect to account deletion, and
/// `tests/teardown.rs` asserts exactly that against the live schema.
pub const USER_PURGED_TABLES: &[&str] = &[
    "account_recovery",
    "attachment_ref",
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
    // The two commit-log-DB tables in this list. `mls_welcome` is purged by
    // `writes::purge_welcomes` and `mls_commit_since` by `purge_user_log_rows`,
    // both called from the `/v1/account/delete` handler once the main
    // transaction has committed.
    "mls_commit_since",
    "mls_welcome",
    "push_token",
    "security_event",
    "user_block",
    "user_device",
    "user_dms",
    "user_groups",
    "user_preferences",
    "users",
];

/// Every table a conversation teardown clears — [`purge_conversation_rows`],
/// [`purge_group`], [`purge_dm_channel`] and [`purge_conversation_log`] between
/// them.
pub const CONVERSATION_PURGED_TABLES: &[&str] = &[
    "attachment_ref",
    "channels",
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
    "user_dms",
    "user_groups",
];

/// Tables that deliberately survive an account deletion, and why. Read by
/// `tests/teardown.rs`, which fails on any table that is neither
/// purged nor listed here — the list is a decision record, not a suppression.
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
];

/// Delete every main-DB row keyed to `user_id`, **except** the `users` row
/// itself, which the caller deletes last.
///
/// Split that way because account deletion removes the anchor row while identity
/// reset ([`crate::account::apply_reset_recover`]) keeps it — the set of
/// dependent rows is otherwise identical, and having one list is the point.
///
/// Every statement is `WHERE <user column> = ?1`, so nothing here can reach a
/// row that does not name this user. The two `OR`-ed predicates (`user_block`,
/// `group_invite`) name the same user on both sides of a pair, which is the
/// whole row's subject in each case.
pub async fn purge_user_rows(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    let uid = || libsql::params![user_id.to_string()];
    let uid2 = || libsql::params![user_id.to_string(), user_id.to_string()];

    // Rows hanging off this user's own envelopes, BEFORE those envelopes go —
    // afterwards the subquery matches nothing and they are orphaned, which is
    // the whole failure mode. The reactions removed here can belong to other
    // users; that is correct, because the message being reacted to ceases to
    // exist. Deleting the attachment references also releases the shared
    // objects for collection (#690).
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

    // Envelopes this user sent. Sealed-sender envelopes (#607) carry a fixed
    // non-identifying sentinel in `sender_id` rather than this id, so they are
    // deliberately NOT matched here — they are unattributable by construction.
    // They go with their conversation, or via the #720 watermark sweep.
    conn.execute("DELETE FROM message_envelope WHERE sender_id = ?1", uid()).await?;

    // Reactions this user made on anyone's messages. They carry the reacting
    // user in the clear, so they are removable and must be removed.
    conn.execute("DELETE FROM message_reaction WHERE user_id = ?1", uid()).await?;

    // Membership and the social graph.
    conn.execute("DELETE FROM group_member WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM dm_channel_member WHERE user_id = ?1", uid()).await?;
    conn.execute(
        "DELETE FROM user_block WHERE blocker_id = ?1 OR blocked_id = ?2",
        uid2(),
    )
    .await?;

    // Invites, in both directions, plus the links this user minted. Honour the
    // declared `ON DELETE SET NULL` on the redemption audit trail BEFORE the
    // links go: those rows can belong to OTHER users, so they are nulled, never
    // deleted — only this user's own redemptions are removed.
    conn.execute(
        "DELETE FROM group_invite WHERE inviter_id = ?1 OR invitee_id = ?2",
        uid2(),
    )
    .await?;
    conn.execute("DELETE FROM group_join_request WHERE requester_id = ?1", uid()).await?;
    // `reviewed_by` is a nullable audit pointer on ANOTHER user's request row.
    // Blank the reference rather than deleting someone else's request.
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

    // Custom emoji this user contributed. Migration 000015 states the intent
    // ("account teardown deletes by creator") and adds `idx_group_emoji_created_by`
    // for it. `created_by` is NOT NULL, so the alternative to deleting the row is
    // retaining a deleted user's id in it forever. Deleting also releases the
    // shared object for `POST /v1/emoji/gc`.
    conn.execute("DELETE FROM group_emoji WHERE created_by = ?1", uid()).await?;

    // Devices and per-device state.
    conn.execute("DELETE FROM user_device WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM device_enrollment_request WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM mls_key_package WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM conversation_watermark WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM push_token WHERE user_id = ?1", uid()).await?;

    // Account-scoped records.
    conn.execute("DELETE FROM account_recovery WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM user_preferences WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM security_event WHERE user_id = ?1", uid()).await?;

    // Derived directory index (migration 000009). Nothing reads it today, which
    // is exactly why it would have gone on holding a deleted user's membership
    // graph indefinitely.
    conn.execute("DELETE FROM user_groups WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM user_dms WHERE user_id = ?1", uid()).await?;

    // Rows that SURVIVE but still point at this user by id.
    hand_over_dangling_pointers(conn, user_id).await?;

    Ok(())
}

/// Deal with the columns that name a user on a row belonging to something that
/// outlives them: `groups.owner_id`, `dm_channel.created_by` and
/// `dm_channel_member.added_by`.
///
/// Deleting those rows is not an option — they belong to a conversation other
/// people are still using — and all three columns are `NOT NULL`, so they cannot
/// simply be blanked. Each is therefore handed to a remaining participant, or
/// (for the pure audit field) collapsed to a self-reference.
///
/// Two of the three are more than tidiness:
///
/// * **`dm_channel.created_by` is an authorization input.** `apply_remove_dm_member`
///   lets the channel creator remove someone else. Left naming a deleted user,
///   that power belongs to an account that can never authenticate again, so the
///   DM can never have a member removed. Handing it to a remaining member
///   restores the capability to somebody who actually holds it — the same shape
///   as the group sole-admin promotion.
/// * **`groups.owner_id` is a display field, not an authz input**
///   (`pollis-core::commands::groups::membership`), so reassigning it grants
///   nothing. It is handed over because otherwise a deleted user's id is
///   displayed as the owner of a live group forever. Note this runs for EVERY
///   group they owned, not just the ones where a sole-admin promotion happened —
///   a group with two admins promotes nobody, and used to keep the dead
///   `owner_id` on that path.
///
/// Every statement is anchored on `= ?1` for this user and picks its replacement
/// from the surviving membership of that same conversation, so nothing here can
/// touch a conversation the user had no part in.
async fn hand_over_dangling_pointers(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    let uid = || libsql::params![user_id.to_string()];

    // Prefer an admin, fall back to any member. A group with no members left is
    // being torn down by the caller, so the subquery yielding NULL is fine: the
    // guard keeps the row unchanged rather than writing NULL into a NOT NULL
    // column.
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

    // Pure audit field, never read for authorization. Collapsing it to the
    // member's own id says "who added them is no longer recorded" without
    // inventing a sentinel or dropping somebody else's membership row.
    conn.execute(
        "UPDATE dm_channel_member SET added_by = user_id WHERE added_by = ?1",
        uid(),
    )
    .await?;

    Ok(())
}

/// Delete every main-DB row belonging to one conversation's message history:
/// envelopes, the reactions and attachment references hanging off them, and the
/// per-device cursors.
///
/// `conversation_id` is a channel id or a DM channel id — `message_envelope`,
/// `conversation_watermark` and the registry all key on the same namespace
/// (migration 000016), which is why one function serves both.
///
/// The two subqueries are anchored on `conversation_id`, so they can only reach
/// messages of this conversation. They run BEFORE the envelopes are deleted —
/// afterwards the subquery would return nothing and the child rows would be
/// orphaned, which is the bug this whole module is about.
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
    conn.execute(
        "DELETE FROM message_reaction WHERE message_id IN \
         (SELECT id FROM message_envelope WHERE conversation_id = ?1)",
        cid(),
    )
    .await?;
    conn.execute("DELETE FROM message_envelope WHERE conversation_id = ?1", cid()).await?;
    conn.execute("DELETE FROM conversation_watermark WHERE conversation_id = ?1", cid()).await?;
    Ok(())
}

/// Tear a whole group down: every channel's history, then the group's own rows.
///
/// Returns the conversation ids that no longer exist (the group's channels), so
/// the caller can purge their commit-log-DB rows after the main transaction
/// commits — those live on a different database and cannot join this one's
/// transaction.
pub async fn purge_group(conn: &Connection, group_id: &str) -> anyhow::Result<Vec<String>> {
    let gid = || libsql::params![group_id.to_string()];

    // Channels first: their history is only reachable through them.
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

    // Emoji references the group held (#848) — released here, collected by
    // `POST /v1/emoji/gc`.
    crate::emoji::release_group_emoji(conn, group_id).await?;

    // Invites and links. Redemption rows can belong to other users, so their
    // dangling `link_id` is nulled (the declared `ON DELETE SET NULL`) rather
    // than the row being deleted.
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
    conn.execute("DELETE FROM user_groups WHERE group_id = ?1", gid()).await?;

    // The anchor row last.
    conn.execute("DELETE FROM groups WHERE id = ?1", gid()).await?;

    Ok(channel_ids)
}

/// Tear a whole DM channel down. Caller must already have established that the
/// channel has no members left (or that it is being destroyed regardless).
pub async fn purge_dm_channel(conn: &Connection, dm_channel_id: &str) -> anyhow::Result<()> {
    let did = || libsql::params![dm_channel_id.to_string()];

    purge_conversation_rows(conn, dm_channel_id).await?;
    conn.execute("DELETE FROM dm_channel_member WHERE dm_channel_id = ?1", did()).await?;
    conn.execute("DELETE FROM user_dms WHERE dm_channel_id = ?1", did()).await?;
    conn.execute("DELETE FROM dm_channel WHERE id = ?1", did()).await?;
    Ok(())
}

/// Purge the commit-log DB's rows that name `user_id` and are not part of an
/// append-only guarantee.
///
/// Today that is `mls_commit_since`, the per-device commit catch-up high-water
/// (`migrations-log/000003`). Nothing has ever deleted from it: it is upserted on
/// every catch-up and was never cleaned on account deletion, so it retained a
/// `(conversation_id, user_id, device_id)` triple for every conversation a
/// deleted user was ever in, indefinitely. It is not a storage-pinning bug —
/// `commit.rs`'s retention floor is taken over CURRENT member devices, so a
/// departed user's row does not hold commits back — but it is exactly the
/// metadata the retention policy promises to remove.
///
/// `mls_welcome` is handled by [`crate::writes::purge_welcomes`], which already
/// existed for identity reset. `mls_commit_log` is deliberately untouched — see
/// [`EXEMPT_FROM_USER_PURGE`].
///
/// Callers run this AFTER the main transaction commits, for the same reason as
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

/// Purge one device's commit-log-DB state. Used by device revocation and logout,
/// which already drop that device's key packages and fetch cursors on the main
/// DB but left its catch-up high-water behind.
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

/// Purge the commit-log DB's rows for conversations that no longer exist.
///
/// Separate from the main-DB functions because these three tables live on the
/// commit-log database (`AppState::log_db`), which in a split deploy is a
/// different Turso and therefore cannot share the main transaction.
///
/// **Callers must run this AFTER the main transaction commits.** If the main
/// transaction rolls back, the conversation still exists, and having already
/// deleted its GroupInfo and commit history would be destructive to live
/// members. Running it after means the worst case is leftover rows for a dead
/// conversation — retention debt, recoverable by re-running — instead of data
/// loss for a living one. Deleting too little beats deleting too much.
///
/// Only ever called with ids of conversations this same operation destroyed, so
/// the append-only guarantee of `mls_commit_log` is not weakened: there is no
/// member left to read the chain.
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
