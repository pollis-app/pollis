-- Enforce unique usernames and stand up the migration tracking table.
--
-- This file originally did a SQLite "ALTER COLUMN" dance —
--   CREATE TABLE users_new (...7 cols...);
--   INSERT INTO users_new SELECT * FROM users;
--   DROP TABLE users;
--   ALTER TABLE users_new RENAME TO users;
-- to retrofit NOT NULL onto `username`. That dance is destructive on
-- re-run: once later migrations have added columns to `users`, the
-- positional `INSERT … SELECT *` mismatches, the INSERT fails, and
-- the `DROP TABLE users` that follows silently wipes real data.
--
-- Rewritten to be idempotent: create the tracking table if missing,
-- backfill NULL usernames deterministically, and enforce uniqueness
-- via a UNIQUE INDEX (which SQLite's `IF NOT EXISTS` handles
-- natively). NOT NULL is enforced at the application layer (see
-- `UserProfile` in the Rust auth commands) rather than via a table
-- rebuild, which is an acceptable tradeoff pre-production.

CREATE TABLE IF NOT EXISTS schema_migrations (
    version     INTEGER PRIMARY KEY,
    description TEXT NOT NULL,
    applied_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Backfill any NULL usernames with email prefix + last 6 chars of the
-- ULID so the default is unique even when multiple users share the
-- same email prefix (e.g. john@foo.com vs john@bar.com). Idempotent:
-- no-op once every row has a username.
UPDATE users
SET username = LOWER(SUBSTR(email, 1, INSTR(email, '@') - 1))
            || '_'
            || LOWER(SUBSTR(id, -6))
WHERE username IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_username ON users(username);

INSERT OR IGNORE INTO schema_migrations (version, description) VALUES
    (1, 'unique index on users.username, schema_migrations table');
