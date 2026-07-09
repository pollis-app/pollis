//! Directory-index maintenance (issue #261, Phase 2).
//!
//! Keeps the denormalized `user_groups` / `user_dms` tables — the per-user
//! sidebar index in the directory DB — in sync with the authoritative
//! `group_member` / `dm_channel_member` writes. The Delivery Service is the sole
//! writer, so every write path that changes membership, group/DM metadata, or
//! produces a message calls one of these helpers. The index can only drift if a
//! call site is missing one, which the `directory_index_matches_membership`
//! equivalence test guards against.
//!
//! The upsert helpers PROJECT from the authoritative rows
//! (`INSERT … SELECT FROM group_member/groups … ON CONFLICT DO UPDATE`), so the
//! index is literally a projection of membership and re-running a helper is
//! always correct. FK cascade is deliberately NOT relied on — `foreign_keys` is
//! OFF on the DS connection, so every delete here is explicit.
//!
//! All tables live in the main DB (`state.db`), so every fn takes a bare
//! [`Connection`] and composes inside a caller's transaction (`&Transaction`
//! derefs to `&Connection`).

use libsql::Connection;

// ── Groups ───────────────────────────────────────────────────────────────────

/// Upsert one member's `user_groups` row by projecting from the authoritative
/// `group_member` + `groups` rows (which must already be written). Covers member
/// add (create / accept-invite / approve-join-request) and role change. On insert
/// `last_activity_at` seeds from the newest channel message, falling back to the
/// group's creation time; on a name/role update it is PRESERVED.
pub async fn sync_group_member(
    conn: &Connection,
    group_id: &str,
    user_id: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO user_groups (user_id, group_id, group_name, role, joined_at, last_activity_at)
         SELECT gm.user_id, gm.group_id, g.name, gm.role, gm.joined_at,
                COALESCE(
                    (SELECT MAX(me.sent_at) FROM message_envelope me
                     JOIN channels c ON c.id = me.conversation_id
                     WHERE c.group_id = g.id),
                    g.created_at)
         FROM group_member gm
         JOIN groups g ON g.id = gm.group_id
         WHERE gm.group_id = ?1 AND gm.user_id = ?2
         ON CONFLICT(user_id, group_id) DO UPDATE SET
             group_name = excluded.group_name,
             role       = excluded.role",
        libsql::params![group_id.to_string(), user_id.to_string()],
    )
    .await?;
    Ok(())
}

/// Drop one member's `user_groups` row (leave / remove-member).
pub async fn remove_group_member(
    conn: &Connection,
    group_id: &str,
    user_id: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM user_groups WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.to_string(), user_id.to_string()],
    )
    .await?;
    Ok(())
}

/// Drop every `user_groups` row for a deleted group.
pub async fn remove_group(conn: &Connection, group_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM user_groups WHERE group_id = ?1",
        libsql::params![group_id.to_string()],
    )
    .await?;
    Ok(())
}

/// Re-project the group name onto every member's row after a rename.
pub async fn rename_group(conn: &Connection, group_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE user_groups
         SET group_name = (SELECT name FROM groups WHERE id = ?1)
         WHERE group_id = ?1",
        libsql::params![group_id.to_string()],
    )
    .await?;
    Ok(())
}

// ── DMs ──────────────────────────────────────────────────────────────────────

/// Upsert one member's `user_dms` row by projecting from the authoritative
/// `dm_channel_member` + `dm_channel` rows. Covers DM create, member add, and
/// accept (which sets `accepted_at`). `last_activity_at` is preserved on update.
pub async fn sync_dm_member(
    conn: &Connection,
    dm_channel_id: &str,
    user_id: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO user_dms (user_id, dm_channel_id, created_by, added_at, accepted_at, last_activity_at)
         SELECT dcm.user_id, dcm.dm_channel_id, dc.created_by, dcm.added_at, dcm.accepted_at,
                COALESCE(
                    (SELECT MAX(me.sent_at) FROM message_envelope me
                     WHERE me.conversation_id = dc.id),
                    dc.created_at)
         FROM dm_channel_member dcm
         JOIN dm_channel dc ON dc.id = dcm.dm_channel_id
         WHERE dcm.dm_channel_id = ?1 AND dcm.user_id = ?2
         ON CONFLICT(user_id, dm_channel_id) DO UPDATE SET
             accepted_at = excluded.accepted_at",
        libsql::params![dm_channel_id.to_string(), user_id.to_string()],
    )
    .await?;
    Ok(())
}

/// Re-project `accepted_at` for every one of a user's `user_dms` rows from the
/// authoritative `dm_channel_member` table. Used after a bulk accepted-state
/// change that isn't a single-row upsert — blocking a user resets `accepted_at`
/// to NULL on all DMs shared with them.
pub async fn resync_dm_accepted_for_user(
    conn: &Connection,
    user_id: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE user_dms
         SET accepted_at = (
             SELECT dcm.accepted_at FROM dm_channel_member dcm
             WHERE dcm.dm_channel_id = user_dms.dm_channel_id AND dcm.user_id = user_dms.user_id)
         WHERE user_id = ?1",
        libsql::params![user_id.to_string()],
    )
    .await?;
    Ok(())
}

/// Drop one member's `user_dms` row (leave / remove).
pub async fn remove_dm_member(
    conn: &Connection,
    dm_channel_id: &str,
    user_id: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM user_dms WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![dm_channel_id.to_string(), user_id.to_string()],
    )
    .await?;
    Ok(())
}

/// Drop every `user_dms` row for a torn-down DM channel.
pub async fn remove_dm(conn: &Connection, dm_channel_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM user_dms WHERE dm_channel_id = ?1",
        libsql::params![dm_channel_id.to_string()],
    )
    .await?;
    Ok(())
}

// ── Activity + account ───────────────────────────────────────────────────────

/// Bump `last_activity_at` for whichever conversation `conversation_id` names.
/// For a group message it is a channel id (resolved to its group); for a DM it is
/// the dm_channel id. Both statements run — exactly one matches — so the caller
/// doesn't need to know which kind it is.
pub async fn bump_activity(
    conn: &Connection,
    conversation_id: &str,
    sent_at: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE user_groups SET last_activity_at = ?2
         WHERE group_id = (SELECT group_id FROM channels WHERE id = ?1)",
        libsql::params![conversation_id.to_string(), sent_at.to_string()],
    )
    .await?;
    conn.execute(
        "UPDATE user_dms SET last_activity_at = ?2 WHERE dm_channel_id = ?1",
        libsql::params![conversation_id.to_string(), sent_at.to_string()],
    )
    .await?;
    Ok(())
}

/// Remove a user from the whole index (account deletion).
pub async fn remove_user(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM user_groups WHERE user_id = ?1",
        libsql::params![user_id.to_string()],
    )
    .await?;
    conn.execute(
        "DELETE FROM user_dms WHERE user_id = ?1",
        libsql::params![user_id.to_string()],
    )
    .await?;
    Ok(())
}
