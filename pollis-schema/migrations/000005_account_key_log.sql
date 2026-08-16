-- Append-only history of account identity keys. Every time a user's account
-- identity key is established (signup) or rotated (reset_identity) a new row is
-- written here, never updated or deleted. This is the data source for a future
-- account-key transparency-log tenant: a verifiable, monotonic record of which
-- public key was authoritative for a user at each identity_version.
--
-- Mirrors mls_commit_log: an AUTOINCREMENT seq gives a stable global ordering,
-- and a UNIQUE index enforces the per-subject invariant (here: one row per
-- (user_id, identity_version), the same way mls_commit_log enforces one commit
-- per (conversation_id, epoch)).
CREATE TABLE account_key_log (
    seq              INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id          TEXT NOT NULL,
    account_id_pub   BLOB NOT NULL,
    identity_version INTEGER NOT NULL,
    created_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

-- One row per identity version per user. A duplicate (user_id, identity_version)
-- INSERT conflicts here rather than silently forking the history.
CREATE UNIQUE INDEX IF NOT EXISTS idx_account_key_log_user_version
    ON account_key_log (user_id, identity_version);

-- Backfill: seed the log with the current key of every user that already has an
-- account identity. Users with no identity yet (account_id_pub IS NULL) get
-- their first row written by generate_account_identity when they sign up.
INSERT INTO account_key_log (user_id, account_id_pub, identity_version)
SELECT id, account_id_pub, identity_version
FROM users
WHERE account_id_pub IS NOT NULL;
