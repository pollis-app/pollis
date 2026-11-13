//! Explicit teardown — the deletes that `ON DELETE CASCADE` was supposed to do
//! and never did on any deployment.
//!
//! Deletes never rely on foreign-key cascades ([`crate::db::LOCAL_PRAGMAS`]).
//!
//! So: no implicit deletes anywhere. Every row that should go is named here.
//!
//! # The rule for adding a table
//!
//! A new table naming a user, group, channel or DM goes below or in an `EXEMPT_*` list (`tests/teardown.rs`).
//!
//! # Scoping — the #878 defect class
//!
//! #878: every statement is bound to the torn-down id, reaching other tables only via anchored subqueries.
//!
//! # Ordering
//!
//! Children before parents, anchor row last, so a partial run is monotone; re-running is a no-op.

use libsql::Connection;

/// Every table [`purge_user_rows`] clears (plus `users`); with [`EXEMPT_FROM_USER_PURGE`], the whole schema.
pub const USER_PURGED_TABLES: &[&str] = &[
    "history_transfer",
    "history_transfer_budget",
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
    "mls_key_package_claim",
    // Commit-log-DB tables, purged by the delete handler after the main transaction.
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
    "conversation_membership_version",
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

/// Tables that deliberately survive an account deletion, and why (a decision record).
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
         stop every REMAINING member from advancing. The rows stay, but every \
         field naming the account (`sender_id`, `added_user_id`, \
         `added_device_ids`) is blanked by `scrub_commit_log`. Commit rows go when \
         the whole conversation goes (`purge_conversation_log`), and are pruned by \
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
        "account_log_purge",
        "Account deletion's own record of the commit-log half it still owes \
         (#1883), written in the deletion transaction and deleted by \
         `finish_account_log_purge` once that half has run.",
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

/// Delete every main-DB row keyed to `user_id`, **except** the `users` row
/// itself, which the caller deletes last.
///
/// Identity reset ([`crate::account::apply_reset_recover`]) keeps the anchor row; the rest is shared.
pub async fn purge_user_rows(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    let uid = || libsql::params![user_id.to_string()];
    let uid2 = || libsql::params![user_id.to_string(), user_id.to_string()];

    // Rows hanging off this user's envelopes go BEFORE those envelopes, or they're orphaned (#690).
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

    // Unsealed (legacy) envelopes. Sealed ones (#607) carry no sender id; account
    // deletion withdraws them by capability first (`messages::withdraw_envelopes`).
    conn.execute("DELETE FROM message_envelope WHERE sender_id = ?1", uid()).await?;

    conn.execute("DELETE FROM message_reaction WHERE user_id = ?1", uid()).await?;

    // The lookup budget (#1089): a recreated account gets a new id, so nothing to keep.
    conn.execute("DELETE FROM directory_lookup_budget WHERE user_id = ?1", uid()).await?;

    // Membership and the social graph.
    conn.execute("DELETE FROM group_member WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM dm_channel_member WHERE user_id = ?1", uid()).await?;
    conn.execute(
        "DELETE FROM user_block WHERE blocker_id = ?1 OR blocked_id = ?2",
        uid2(),
    )
    .await?;
    // Abuse reports filed BY or ABOUT this user, and any suspension (#1213).
    conn.execute(
        "DELETE FROM user_report WHERE reporter_id = ?1 OR reported_id = ?2",
        uid2(),
    )
    .await?;
    conn.execute("DELETE FROM account_suspension WHERE user_id = ?1", uid()).await?;

    // Invites and links; others' redemption rows are nulled (`ON DELETE SET NULL`), not deleted.
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

    // Custom emoji this user contributed (migration 000015); releases the objects for gc.
    conn.execute("DELETE FROM group_emoji WHERE created_by = ?1", uid()).await?;

    // Devices and per-device state. The conversation-owned key rows (#99, #1382)
    // survive, so their diagnostic device pointer is blanked while `user_device`
    // can still say which devices were this user's.
    for table in ["pin_keystate", "file_keystate"] {
        conn.execute(
            &format!(
                "UPDATE {table} SET updated_by_device_id = '' WHERE updated_by_device_id IN \
                 (SELECT device_id FROM user_device WHERE user_id = ?1)"
            ),
            uid(),
        )
        .await?;
    }
    conn.execute("DELETE FROM user_device WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM device_enrollment_request WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM mls_key_package WHERE user_id = ?1", uid()).await?;
    // The claim budget (#847), as claimer and as the account claimed from.
    conn.execute(
        "DELETE FROM mls_key_package_claim WHERE claimer_id = ?1 OR target_user_id = ?2",
        uid2(),
    )
    .await?;
    conn.execute("DELETE FROM conversation_watermark WHERE user_id = ?1", uid()).await?;
    // #1423: tickets awaiting a receipt name only a token, so they go via the
    // user's tokens, before those are deleted.
    conn.execute(
        "DELETE FROM push_ticket WHERE token IN (SELECT token FROM push_token WHERE user_id = ?1)",
        uid(),
    )
    .await?;
    conn.execute("DELETE FROM push_token WHERE user_id = ?1", uid()).await?;
    // #1122: the only durable record that a notification for some conversation
    // went to this user. A deleted account must not leave it behind.
    conn.execute("DELETE FROM push_handle WHERE user_id = ?1", uid()).await?;

    // Account-scoped records.
    conn.execute("DELETE FROM account_recovery WHERE user_id = ?1", uid()).await?;
    conn.execute("DELETE FROM user_preferences WHERE user_id = ?1", uid()).await?;
    // The encrypted read-cursor blob (#844).
    conn.execute("DELETE FROM read_cursor_sync WHERE user_id = ?1", uid()).await?;
    // Channel mutes (#1431).
    conn.execute("DELETE FROM channel_mute WHERE user_id = ?1", uid()).await?;
    // The vault (#107): references first, while the subquery can still see the entries.
    conn.execute(
        "DELETE FROM vault_attachment_ref WHERE vault_message_id IN \
         (SELECT id FROM vault_message WHERE user_id = ?1)",
        uid(),
    )
    .await?;
    conn.execute("DELETE FROM vault_message WHERE user_id = ?1", uid()).await?;
    // Pins this user placed (#99); the row names the pinner in the clear.
    conn.execute("DELETE FROM pinned_message WHERE pinned_by = ?1", uid()).await?;
    conn.execute("DELETE FROM security_event WHERE user_id = ?1", uid()).await?;
    // #1735: transfers (and their sealed chunks) and the request budget.
    crate::history_transfer::purge_user(conn, user_id).await?;

    // Rows that SURVIVE but still point at this user by id.
    hand_over_dangling_pointers(conn, user_id).await?;

    Ok(())
}

/// Deal with the columns that name a user on a row belonging to something that
/// outlives them: `groups.owner_id`, `dm_channel.created_by` and
/// `dm_channel_member.added_by`.
///
/// Those rows outlive the user and the columns are `NOT NULL`, so each is handed on:
/// * **`dm_channel.created_by` is an authorization input** (`apply_remove_dm_member`).
/// * **`groups.owner_id`** is display-only, handed over for every group they owned.
///
/// Every statement is anchored on `= ?1` and picks from that conversation's survivors.
async fn hand_over_dangling_pointers(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    let uid = || libsql::params![user_id.to_string()];

    // Prefer an admin, else any member; a NULL subquery leaves the row unchanged.
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

    // Pure audit field: collapse to the member's own id.
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
/// `conversation_id` is a channel or DM id (one namespace, migration 000016).
///
/// The subqueries run BEFORE the envelopes go, or the child rows would be orphaned.
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
    // By the envelope while it survives, and by the conversation the reaction
    // was gated against (#1433) once GC has collected it.
    conn.execute(
        "DELETE FROM message_reaction WHERE conversation_id = ?1 OR message_id IN \
         (SELECT id FROM message_envelope WHERE conversation_id = ?1)",
        cid(),
    )
    .await?;
    conn.execute("DELETE FROM message_envelope WHERE conversation_id = ?1", cid()).await?;
    conn.execute("DELETE FROM conversation_watermark WHERE conversation_id = ?1", cid()).await?;
    // The delivery-sequence counter (#1087): GC never removes it; a teardown must.
    conn.execute("DELETE FROM conversation_seq WHERE conversation_id = ?1", cid()).await?;
    // #1122: handles naming this conversation.
    conn.execute("DELETE FROM push_handle WHERE conversation_id = ?1", cid()).await?;
    // Pins live on the channel/DM id (#99), the keystate on the MLS id; mismatches are no-ops.
    conn.execute("DELETE FROM pinned_message WHERE conversation_id = ?1", cid()).await?;
    conn.execute("DELETE FROM pin_keystate WHERE conversation_id = ?1", cid()).await?;
    conn.execute("DELETE FROM file_keystate WHERE conversation_id = ?1", cid()).await?;
    // Mutes of this channel (#1431). A no-op for a DM id, which nobody can mute.
    conn.execute("DELETE FROM channel_mute WHERE channel_id = ?1", cid()).await?;
    Ok(())
}

/// Drop `user_id`'s channel mutes (#1431) in `group_id`, so a rejoin doesn't revive them.
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

/// Tear a whole group down: every channel's history, then the group's own rows.
///
/// Returns the gone channel ids, whose commit-log rows the caller purges after commit.
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

    // Invites and links; redemption rows' `link_id` is nulled, not deleted.
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

    // The group's pin key (#99), keyed by the group id.
    conn.execute("DELETE FROM pin_keystate WHERE conversation_id = ?1", gid()).await?;
    conn.execute("DELETE FROM file_keystate WHERE conversation_id = ?1", gid()).await?;

    // The roster-departure counter (#1874), keyed by the group id.
    conn.execute("DELETE FROM conversation_membership_version WHERE conversation_id = ?1", gid()).await?;

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
    conn.execute("DELETE FROM conversation_membership_version WHERE conversation_id = ?1", did()).await?;
    conn.execute("DELETE FROM dm_channel WHERE id = ?1", did()).await?;
    Ok(())
}

/// Purge or blank every commit-log-DB field that names `user_id` or one of
/// `device_ids` (from [`crate::broker::user_device_ids`], read BEFORE
/// [`purge_user_rows`] deletes `user_device`).
///
/// * `mls_commit_since` rows go.
/// * `mls_welcome.submitted_by` is nulled on Welcomes addressed to others. NULL
///   is "unattributed", which only drops a resubmit defence the deleted account
///   can no longer exercise. The account's own Welcomes go via
///   [`crate::writes::purge_welcomes`].
/// * `mls_commit_log` and `mls_group_info` rows stay; see [`scrub_commit_log`].
///
/// Callers run this AFTER the main transaction commits, for the same reason as
/// [`purge_conversation_log`].
pub async fn purge_user_log_rows(
    log_conn: &Connection,
    user_id: &str,
    device_ids: &[String],
) -> anyhow::Result<()> {
    let uid = || libsql::params![user_id.to_string()];
    log_conn
        .execute("DELETE FROM mls_commit_since WHERE user_id = ?1", uid())
        .await?;
    log_conn
        .execute(
            "UPDATE mls_welcome SET submitted_by = NULL WHERE submitted_by = ?1",
            uid(),
        )
        .await?;
    scrub_commit_log(log_conn, user_id, device_ids).await
}

/// Blank `user_id` and `device_ids` out of the commit chain and the GroupInfo
/// rows of conversations that outlive the account.
///
/// The rows stay: the chain is shared by every member and a commit removed from
/// its middle is an epoch gap for all of them (see `EXEMPT_FROM_USER_PURGE`).
///
/// What is blanked, and why nothing breaks:
/// * `sender_id` → `''`. Read back only by `writes::head_commit_sender`, which
///   compares it to an authenticated (never empty) actor, and echoed in
///   `CommitWire`, which clients do not consult. The immutability trigger
///   (log migration 000005) guards the chain identity and payload, not this.
/// * `added_user_id` / `added_device_ids` → the account and its devices are
///   dropped from the lists (both NULL once no other user is named). They are a
///   prefetch hint (`reads::added_identities`) and a re-add hint for the named
///   device (`readded_epoch` in pollis-core); a deleted account has no device
///   left to use either.
/// * `mls_group_info.updated_by_device_id` → `''`; diagnostic only.
///
/// Idempotent. Runs on the commit-log DB, via [`purge_user_log_rows`].
pub async fn scrub_commit_log(
    conn: &Connection,
    user_id: &str,
    device_ids: &[String],
) -> anyhow::Result<()> {
    scrub_commit_rows(conn, user_id, device_ids).await?;
    scrub_group_info(conn, user_id, device_ids).await
}

/// The `mls_commit_log` half of [`scrub_commit_log`].
async fn scrub_commit_rows(
    conn: &Connection,
    user_id: &str,
    device_ids: &[String],
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE mls_commit_log SET sender_id = '' WHERE sender_id = ?1",
        libsql::params![user_id.to_string()],
    )
    .await?;

    // `added_user_id` is a CSV: match a whole element, never a substring.
    let mut hits: Vec<(i64, Option<String>, Option<String>)> = Vec::new();
    {
        let mut rows = conn
            .query(
                "SELECT seq, added_user_id, added_device_ids FROM mls_commit_log \
                 WHERE instr(',' || replace(added_user_id, ' ', '') || ',', ',' || ?1 || ',') > 0",
                libsql::params![user_id.to_string()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            hits.push((row.get(0)?, row.get(1)?, row.get(2)?));
        }
    }
    for (seq, users, devices) in hits {
        let users = csv_without(users.as_deref(), &[user_id.to_string()]);
        let devices = match users {
            Some(_) => csv_without(devices.as_deref(), device_ids),
            None => None,
        };
        conn.execute(
            "UPDATE mls_commit_log SET added_user_id = ?2, added_device_ids = ?3 WHERE seq = ?1",
            libsql::params![seq, users, devices],
        )
        .await?;
    }
    Ok(())
}

/// The `mls_group_info` half of [`scrub_commit_log`].
async fn scrub_group_info(
    conn: &Connection,
    user_id: &str,
    device_ids: &[String],
) -> anyhow::Result<()> {
    let mut named: Vec<String> = Vec::with_capacity(device_ids.len() + 1);
    named.push(user_id.to_string());
    named.extend(device_ids.iter().cloned());
    for id in named {
        conn.execute(
            "UPDATE mls_group_info SET updated_by_device_id = '' WHERE updated_by_device_id = ?1",
            libsql::params![id],
        )
        .await?;
    }
    Ok(())
}

/// The main DB's copies of the control-plane tables, from before the commit log
/// moved to its own DB (#420). That move started the log DB empty and never
/// dropped these, so a deployment that predates it still holds rows in them
/// that name accounts. Each statement runs only where its table and column
/// exist: a main DB created after the split, or a test DB that drops them, has
/// none. Where the log shares the main DB this repeats
/// [`purge_user_log_rows`], which is idempotent.
///
/// Callers run this AFTER the main transaction commits, like the log purge.
pub async fn purge_legacy_log_copies(
    conn: &Connection,
    user_id: &str,
    device_ids: &[String],
) -> anyhow::Result<()> {
    let uid = || libsql::params![user_id.to_string()];
    if has_column(conn, "mls_welcome", "recipient_id").await? {
        conn.execute("DELETE FROM mls_welcome WHERE recipient_id = ?1", uid()).await?;
    }
    if has_column(conn, "mls_welcome", "submitted_by").await? {
        conn.execute(
            "UPDATE mls_welcome SET submitted_by = NULL WHERE submitted_by = ?1",
            uid(),
        )
        .await?;
    }
    if has_column(conn, "mls_commit_since", "user_id").await? {
        conn.execute("DELETE FROM mls_commit_since WHERE user_id = ?1", uid()).await?;
    }
    if has_column(conn, "mls_commit_log", "added_device_ids").await? {
        scrub_commit_rows(conn, user_id, device_ids).await?;
    }
    if has_column(conn, "mls_group_info", "updated_by_device_id").await? {
        scrub_group_info(conn, user_id, device_ids).await?;
    }
    Ok(())
}

/// Does `table` exist with a column named `column`?
async fn has_column(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
            libsql::params![table.to_string(), column.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// `csv` with every element in `drop` removed; `None` when nothing is left.
fn csv_without(csv: Option<&str>, drop: &[String]) -> Option<String> {
    let kept: Vec<&str> = csv
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty() && !drop.iter().any(|d| d == e))
        .collect();
    if kept.is_empty() {
        None
    } else {
        Some(kept.join(","))
    }
}

/// Purge one device's commit-log-DB state (revocation, logout).
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
/// These tables live on the commit-log DB, which cannot share the main transaction.
///
/// **Callers must run this AFTER the main transaction commits:** leftovers beat deleting live data.
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

/// Record account deletion's commit-log half in the deletion's own main-DB
/// transaction (migration 000049), so the half that cannot share that
/// transaction is owed durably rather than by the request alone. `device_ids`
/// must be read before [`purge_user_rows`] deletes `user_device`.
pub async fn record_account_log_purge(
    tx: &Connection,
    user_id: &str,
    device_ids: &[String],
    dead_conversations: &[String],
) -> anyhow::Result<()> {
    tx.execute(
        "INSERT INTO account_log_purge (user_id, device_ids, dead_conversations) \
         VALUES (?1, ?2, ?3) \
         ON CONFLICT(user_id) DO UPDATE SET \
             device_ids = excluded.device_ids, \
             dead_conversations = excluded.dead_conversations",
        libsql::params![
            user_id.to_string(),
            serde_json::to_string(device_ids)?,
            serde_json::to_string(dead_conversations)?
        ],
    )
    .await?;
    Ok(())
}

/// Run the commit-log half recorded for `user_id` by [`record_account_log_purge`],
/// then drop the record. Runs only after the deletion transaction has committed
/// (the record exists only then), as [`purge_conversation_log`] requires.
/// Idempotent; `false` when nothing was owed.
///
/// `conn` is the main DB (the record and the pre-#420 log copies), `log_conn`
/// the commit-log DB; they are the same database in a single-DB deploy.
pub async fn finish_account_log_purge(
    conn: &Connection,
    log_conn: &Connection,
    user_id: &str,
) -> anyhow::Result<bool> {
    let owed: Option<(String, String)> = {
        let mut rows = conn
            .query(
                "SELECT device_ids, dead_conversations FROM account_log_purge WHERE user_id = ?1",
                libsql::params![user_id.to_string()],
            )
            .await?;
        match rows.next().await? {
            Some(row) => Some((row.get(0)?, row.get(1)?)),
            None => None,
        }
    };
    let Some((device_ids, dead_conversations)) = owed else {
        return Ok(false);
    };
    let device_ids: Vec<String> = serde_json::from_str(&device_ids)?;
    let dead_conversations: Vec<String> = serde_json::from_str(&dead_conversations)?;

    crate::writes::purge_welcomes(log_conn, user_id).await?;
    purge_user_log_rows(log_conn, user_id, &device_ids).await?;
    purge_conversation_log(log_conn, &dead_conversations).await?;
    purge_legacy_log_copies(conn, user_id, &device_ids).await?;

    conn.execute(
        "DELETE FROM account_log_purge WHERE user_id = ?1",
        libsql::params![user_id.to_string()],
    )
    .await?;
    Ok(true)
}

/// Run every commit-log half still owed, oldest first. One that fails stays
/// recorded for the next run and does not stop the rest. Returns how many ran.
pub async fn drain_account_log_purges(
    conn: &Connection,
    log_conn: &Connection,
) -> anyhow::Result<usize> {
    let mut owed: Vec<String> = Vec::new();
    {
        let mut rows = conn
            .query("SELECT user_id FROM account_log_purge ORDER BY created_at, user_id", ())
            .await?;
        while let Some(row) = rows.next().await? {
            owed.push(row.get(0)?);
        }
    }
    let mut finished = 0;
    for user_id in owed {
        match finish_account_log_purge(conn, log_conn, &user_id).await {
            Ok(true) => finished += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!("account deletion: commit-log purge still owed: {e:#}"),
        }
    }
    Ok(finished)
}
