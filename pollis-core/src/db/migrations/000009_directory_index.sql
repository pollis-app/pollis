-- Directory index (issue #261, Phase 2 — precursor to the per-conversation DB
-- split). Denormalized per-user membership tables in the shared directory DB, so
-- the sidebar's "which groups / DMs am I in, most-recently-active first" read is
-- ONE index query instead of a JOIN across group_member / groups / channels —
-- the JOIN that becomes a cross-shard fan-out once each conversation gets its own
-- DB. Channels themselves are NOT indexed here (they stay enumerated from the
-- shared `channels` table pre-split, and come from the per-group replica after).
--
-- Additive + backward-compatible (CLAUDE.md migration rule): new tables + indexes
-- only, no DROP / column change. `group_member` and `dm_channel_member` remain
-- AUTHORITATIVE; these are a DS-maintained derived cache until the split reaches
-- its `primary` phase. A previously-shipped app never reads them.
--
-- The tables are backfilled from current membership at the bottom of this
-- migration so they are correct the instant the DS begins maintaining them.
-- `last_activity_at` seeds from the newest envelope in the conversation, falling
-- back to the group / DM creation time when there are no messages yet.

CREATE TABLE IF NOT EXISTS user_groups (
    user_id          TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_id         TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    group_name       TEXT NOT NULL,
    role             TEXT NOT NULL DEFAULT 'member',
    joined_at        TEXT NOT NULL DEFAULT (datetime('now')),
    last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, group_id)
);

CREATE INDEX IF NOT EXISTS idx_user_groups_user_activity
    ON user_groups (user_id, last_activity_at DESC);

CREATE TABLE IF NOT EXISTS user_dms (
    user_id          TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    dm_channel_id    TEXT NOT NULL REFERENCES dm_channel(id) ON DELETE CASCADE,
    created_by       TEXT NOT NULL,
    added_at         TEXT NOT NULL DEFAULT (datetime('now')),
    accepted_at      TEXT,
    last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, dm_channel_id)
);

CREATE INDEX IF NOT EXISTS idx_user_dms_user_activity
    ON user_dms (user_id, last_activity_at DESC);

-- Backfill group membership. For group messages, message_envelope.conversation_id
-- is the CHANNEL id, so last-activity resolves through channels → group_id.
INSERT OR IGNORE INTO user_groups (user_id, group_id, group_name, role, joined_at, last_activity_at)
SELECT gm.user_id, gm.group_id, g.name, gm.role, gm.joined_at,
       COALESCE(
           (SELECT MAX(me.sent_at)
            FROM message_envelope me
            JOIN channels c ON c.id = me.conversation_id
            WHERE c.group_id = gm.group_id),
           g.created_at)
FROM group_member gm
JOIN groups g ON g.id = gm.group_id;

-- Backfill DM membership. For DMs, message_envelope.conversation_id IS the
-- dm_channel id, so last-activity resolves directly.
INSERT OR IGNORE INTO user_dms (user_id, dm_channel_id, created_by, added_at, accepted_at, last_activity_at)
SELECT dcm.user_id, dcm.dm_channel_id, dc.created_by, dcm.added_at, dcm.accepted_at,
       COALESCE(
           (SELECT MAX(me.sent_at)
            FROM message_envelope me
            WHERE me.conversation_id = dcm.dm_channel_id),
           dc.created_at)
FROM dm_channel_member dcm
JOIN dm_channel dc ON dc.id = dcm.dm_channel_id;
