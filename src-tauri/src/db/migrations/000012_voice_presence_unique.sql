-- Enforce single-channel-per-user per group on voice_presence.
--
-- This file originally did `DROP TABLE IF EXISTS voice_presence;
-- CREATE TABLE voice_presence (... UNIQUE (user_id, group_id))` to
-- retrofit the UNIQUE constraint. The DROP wipes any live voice
-- presence rows on every re-run — catastrophic if someone re-applies
-- migrations while a call is in progress.
--
-- Rewritten to use a UNIQUE INDEX instead, which gives the exact same
-- constraint without touching the table and which SQLite's
-- `IF NOT EXISTS` handles idempotently.

CREATE UNIQUE INDEX IF NOT EXISTS idx_voice_presence_user_group
    ON voice_presence(user_id, group_id);

INSERT OR IGNORE INTO schema_migrations (version, description) VALUES
    (12, 'voice_presence unique constraint on (user_id, group_id)');
