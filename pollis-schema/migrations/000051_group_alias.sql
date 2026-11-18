-- #2003: per-group aliases — a display name and avatar a member shows in one
-- group instead of their main profile.
--
-- One row per (group, user). Each field is independently nullable: NULL means
-- "use the main profile's `preferred_name` / `avatar_url`" for that field, and
-- a missing row means both. A row with both fields NULL says nothing, so the
-- CHECK refuses it: clearing the last field deletes the row instead.
--
-- Visibility: current members of the group read every row of that group,
-- including rows of members who have since left (their old messages keep
-- rendering the alias). Only the owner lists all of their own rows.
--
-- Lifecycle: the row survives leave and removal (a rejoin restores it). It is
-- deleted by the owner, by an admin of that group, with the group, and with
-- the account (`teardown.rs`) — no `ON DELETE CASCADE` is relied on
-- (production runs with foreign_keys=OFF).
--
-- Avatars are the same public, content-addressed `avatars/<user_id>/<sha>.<ext>`
-- objects the main profile uses, and only ever the owner's own. When a write
-- or a teardown drops a reference to one, its key goes into `avatar_release`;
-- the DS deletes the R2 object once neither `users.avatar_url` nor any
-- `group_alias.avatar_url` names it (`profile::reclaim_released_avatars`).
--
-- Additive and backward-compatible: two new tables. A shipped client neither
-- reads nor writes them and keeps rendering the main profile everywhere.
CREATE TABLE IF NOT EXISTS group_alias (
    group_id     TEXT NOT NULL CHECK (length(group_id) BETWEEN 1 AND 128),
    user_id      TEXT NOT NULL CHECK (length(user_id) BETWEEN 1 AND 128),
    display_name TEXT CHECK (display_name IS NULL OR length(display_name) BETWEEN 1 AND 100),
    avatar_url   TEXT CHECK (avatar_url IS NULL OR length(avatar_url) BETWEEN 1 AND 512),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now')),
    CHECK (display_name IS NOT NULL OR avatar_url IS NOT NULL),
    PRIMARY KEY (group_id, user_id)
);
-- The owner's own list and account deletion read by user.
CREATE INDEX IF NOT EXISTS idx_group_alias_user ON group_alias(user_id);

-- Avatar objects a write or teardown stopped referencing, waiting for the
-- reclaim pass. The key is the row: releasing twice is one row. A key that is
-- referenced again by the time the pass runs loses its row and keeps its object.
CREATE TABLE IF NOT EXISTS avatar_release (
    r2_key      TEXT PRIMARY KEY CHECK (length(r2_key) BETWEEN 1 AND 512),
    released_at TEXT NOT NULL DEFAULT (datetime('now'))
);
