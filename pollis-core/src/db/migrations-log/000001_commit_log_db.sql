-- Commit-log database bootstrap. Refs issue #420 (Goal A — commit-log sole-writer).
--
-- IMPORTANT: this migration is applied to the SEPARATE commit-log Turso DB
-- (LOG_DB_URL / LOG_DB_ADMIN_TOKEN), NOT the main DB. It (re)creates the three
-- MLS control-plane tables so they can live behind a database-level token split:
-- the Delivery Service holds a read-write token and is the sole writer, while
-- clients hold a read-only token. Turso tokens are database-level, not
-- table-level, which is the entire reason these tables must move to their own
-- DB — a client then *physically cannot* write the commit log around the DS, so
-- epoch/commit slips become structurally impossible.
--
-- FK clauses are intentionally OMITTED versus the main-DB baseline:
--   * mls_commit_log.sender_id    -> users(id) ON DELETE CASCADE
--   * mls_welcome.recipient_id    -> users(id) ON DELETE CASCADE
-- `users` does not exist in the log DB, so these would be cross-database
-- references. The DS validates sender_id / membership server-side before any
-- write, so the invariant is preserved without the FK.
--
-- The main-DB copies of these tables are NOT dropped here. Old shipped clients
-- still read/write them (CLAUDE.md backward-compat rule); the main-DB drop is a
-- later phase, after old-version uptake ages out.

-- Append-only MLS commit log. seq is the global monotonic order; the unique
-- (conversation_id, epoch) index (added in main-DB migration 000003) enforces
-- one commit per epoch per conversation, so a racing second INSERT conflicts
-- instead of forking the group.
CREATE TABLE IF NOT EXISTS mls_commit_log (
    seq              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id  TEXT NOT NULL,
    epoch            INTEGER NOT NULL,      -- MLS epoch after this commit
    sender_id        TEXT NOT NULL,         -- FK to users(id) dropped (cross-DB)
    commit_data      BLOB NOT NULL,         -- TLS-serialised MlsMessageOut (Commit)
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    added_user_id    TEXT,
    added_device_ids TEXT
);

-- Latest GroupInfo per conversation (UPSERTed at each resulting epoch).
CREATE TABLE IF NOT EXISTS mls_group_info (
    conversation_id      TEXT PRIMARY KEY,
    epoch                INTEGER NOT NULL,
    group_info           BLOB NOT NULL,
    updated_at           TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by_device_id TEXT NOT NULL
);

-- Per-recipient MLS Welcome messages awaiting delivery.
CREATE TABLE IF NOT EXISTS mls_welcome (
    id                  TEXT PRIMARY KEY,   -- ULID
    conversation_id     TEXT NOT NULL,
    recipient_id        TEXT NOT NULL,      -- FK to users(id) dropped (cross-DB)
    welcome_data        BLOB NOT NULL,      -- TLS-serialised Welcome
    delivered           INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL DEFAULT (datetime('now')),
    recipient_device_id TEXT
);

-- Commit log is read per conversation in (seq) order during catch-up.
CREATE INDEX IF NOT EXISTS idx_mls_commit_conv ON mls_commit_log(conversation_id, seq);

-- One commit per epoch per conversation (from main-DB migration 000003). A
-- duplicate (conversation_id, epoch) INSERT conflicts rather than forking.
CREATE UNIQUE INDEX IF NOT EXISTS idx_mls_commit_conv_epoch
    ON mls_commit_log (conversation_id, epoch);

-- Welcome fanout looks rows up by recipient + delivered flag.
CREATE INDEX IF NOT EXISTS idx_mls_welcome_recip ON mls_welcome(recipient_id, delivered);
