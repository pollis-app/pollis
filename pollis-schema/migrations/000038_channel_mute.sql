-- #1431: per-account, per-channel mute.
--
-- Every group-channel message now wakes every other member with a content-free
-- push. A user who finds a channel too noisy mutes it, and the mute has to live
-- HERE, on the DS, rather than on a device: the push fan-out runs server-side
-- (#987), so only a row the DS can read can stop the push before it is sent,
-- and only a server row reaches every device of the account.
--
-- Slack semantics: a muted channel still pushes on a direct `@username`
-- mention of the muting user, and stays silent for `@all` and ordinary
-- messages (`pollis_delivery::push::notify_new_message`).
--
-- WHAT THE SERVER LEARNS, stated plainly: that this user muted this channel,
-- and when. Unlike the read cursor (000018) this cannot be a blob the server is
-- unable to read, because the server is what acts on it. It is a membership-
-- level fact the DS already holds the shape of (it knows who is in which
-- channel), and it says nothing about content or reading habits.
--
-- One row per (user, channel); the row's existence IS the mute, so "muted
-- twice" and "unmuted but still present" are unrepresentable. The DS writes a
-- row only for a channel the user is currently a member of, deletes the user's
-- rows for a group's channels when they leave or are removed, and deletes them
-- with the account and with the channel (`teardown.rs`) — no `ON DELETE
-- CASCADE` is relied on (production runs with foreign_keys=OFF).
--
-- Additive + backward-compatible: one new table. A shipped client neither
-- reads nor writes it; a DS running before this migration lands treats a
-- failed lookup as "nobody muted" and keeps pushing.
CREATE TABLE IF NOT EXISTS channel_mute (
    user_id    TEXT NOT NULL CHECK (length(user_id) BETWEEN 1 AND 128),
    channel_id TEXT NOT NULL CHECK (length(channel_id) BETWEEN 1 AND 128),
    muted_at   TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, channel_id)
);
-- The fan-out reads by channel ("who muted this one?").
CREATE INDEX IF NOT EXISTS idx_channel_mute_channel ON channel_mute(channel_id);
