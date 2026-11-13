-- Account deletion's commit-log half, recorded in the deletion's own main-DB
-- transaction (#1883).
--
-- The commit-log DB cannot join the main transaction, and its half (the
-- account's pending Welcomes, its `mls_commit_since` rows, its name in the
-- commit chain, the MLS state of the conversations the deletion tore down) may
-- only run once that transaction has committed. A row here says that half is
-- still owed. The DS runs it right after the commit and deletes the row; a row
-- left by a failure is run by the next account deletion or the next DS start.
-- So no deleted account's commit-log rows depend on a request that can no
-- longer authenticate.
--
-- Holds the deleted account's id and its former device ids (JSON arrays) only
-- until that half has run. No other user data.
--
-- Additive and backward-compatible: one CREATE TABLE. Clients never touch it.
CREATE TABLE IF NOT EXISTS account_log_purge (
    user_id            TEXT PRIMARY KEY,
    device_ids         TEXT NOT NULL,
    dead_conversations TEXT NOT NULL,
    created_at         TEXT NOT NULL DEFAULT (datetime('now'))
);
