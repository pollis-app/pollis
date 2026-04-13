-- DM requests + user blocklist.
--
-- Requests: a DM channel is a pending "request" for a given member as
-- long as that member's dm_channel_member.accepted_at is NULL. The
-- initiator's own row is marked accepted on creation; every other
-- member's row starts NULL and flips to a timestamp when they accept.
-- Existing membership rows are backfilled as accepted so prior DMs
-- stay visible.
--
-- Blocks: user_block stores one row per (blocker, blocked) pair. When
-- a user blocks another user, the blocker's accepted_at is reset to
-- NULL for any DM channel they share, so if the block is later
-- released the conversation resurfaces in Requests rather than in the
-- regular DM list.
--
-- Run against Turso manually.

ALTER TABLE dm_channel_member ADD COLUMN accepted_at TEXT;

UPDATE dm_channel_member SET accepted_at = added_at WHERE accepted_at IS NULL;

CREATE TABLE user_block (
    blocker_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    blocked_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (blocker_id, blocked_id)
);

CREATE INDEX idx_block_blocked ON user_block(blocked_id);

INSERT INTO schema_migrations (version, description) VALUES
    (15, 'dm_channel_member.accepted_at + user_block table');
