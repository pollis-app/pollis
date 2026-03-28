-- MLS (RFC 9420) remote tables.
-- Run against Turso: pnpm db:migrate (or via the migrate binary).
--
-- These replace the old Signal-protocol tables:
--   mls_key_package  → replaces one_time_prekey (consumed one-at-a-time)
--   mls_commit_log   → AUTOINCREMENT seq linearises concurrent commits
--   mls_welcome      → delivers Welcome to new members

-- Published KeyPackages.  One row per available (unclaimed) package.
-- Claimed atomically via UPDATE ... SET claimed = 1 ... RETURNING.
CREATE TABLE IF NOT EXISTS mls_key_package (
    ref_hash    TEXT PRIMARY KEY,          -- KeyPackageRef hash (hex)
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    key_package BLOB NOT NULL,             -- TLS-serialised KeyPackage
    claimed     INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_mls_kp_user ON mls_key_package(user_id, claimed);

-- Commit log.  AUTOINCREMENT gives the total epoch order needed by MLS.
-- All members of a conversation poll this table and apply commits in seq order.
CREATE TABLE IF NOT EXISTS mls_commit_log (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    epoch           INTEGER NOT NULL,      -- MLS epoch after this commit
    sender_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    commit_data     BLOB NOT NULL,         -- TLS-serialised MlsMessageOut (Commit)
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_mls_commit_conv ON mls_commit_log(conversation_id, seq);

-- Welcome messages for new members.
CREATE TABLE IF NOT EXISTS mls_welcome (
    id              TEXT PRIMARY KEY,      -- ULID
    conversation_id TEXT NOT NULL,
    recipient_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    welcome_data    BLOB NOT NULL,         -- TLS-serialised Welcome
    delivered       INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_mls_welcome_recip ON mls_welcome(recipient_id, delivered);

INSERT INTO schema_migrations (version, description) VALUES
    (3, 'mls implementation: key packages, commit log, welcome messages');
