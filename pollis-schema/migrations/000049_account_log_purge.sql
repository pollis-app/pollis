-- The commit-log half of an identity reset or an account deletion, recorded in
-- that operation's own main-DB transaction (#1883).
--
-- The commit-log DB cannot join the main transaction, and its half may only
-- run once that transaction has committed. A row here says that half is still
-- owed. The DS runs it right after the commit and deletes the row; a row left
-- by a failure is run by the next account deletion or the next DS start. So
-- no commit-log rows depend on a request that cannot be repeated: a deleted
-- account's devices can no longer sign, and a repeated reset no longer knows
-- which conversations the first one tore down.
--
-- `scrub_account` = 1 for an account deletion: the account's pending
-- Welcomes, its `mls_commit_since` rows and its name in the commit chain go
-- too. An identity reset owes only the torn-down conversations (0); its
-- request deletes the account's Welcomes itself, every time it runs.
--
-- `device_ids` and `dead_conversations` are JSON arrays of ids. Rows hold the
-- account's id and its former device ids only until the half has run.
--
-- Additive and backward-compatible: one CREATE TABLE. Clients never touch it.
CREATE TABLE IF NOT EXISTS account_log_purge (
    user_id            TEXT PRIMARY KEY,
    scrub_account      INTEGER NOT NULL CHECK (scrub_account IN (0, 1)),
    device_ids         TEXT NOT NULL,
    dead_conversations TEXT NOT NULL,
    created_at         TEXT NOT NULL DEFAULT (datetime('now'))
);
