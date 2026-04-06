CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS identity_key (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    public_key BLOB NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- NOTE: signed_prekey, one_time_prekey, signal_session, group_sender_key were
-- Signal Protocol tables removed in favour of MLS. They are not created for new
-- local databases. Existing databases may still have these tables but nothing
-- reads from or writes to them.

CREATE TABLE IF NOT EXISTS message (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    ciphertext BLOB NOT NULL,
    content TEXT,
    reply_to_id TEXT,
    sent_at TEXT NOT NULL,
    received_at TEXT NOT NULL DEFAULT (datetime('now')),
    delivered INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_message_conversation ON message(conversation_id, sent_at);

CREATE TABLE IF NOT EXISTS dm_conversation (
    id TEXT PRIMARY KEY,
    peer_user_id TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- User preferences (local-first mirror of remote user_preferences).
-- Single-row table — the DB file is already scoped to one user.
CREATE TABLE IF NOT EXISTS preferences (
    preferences TEXT NOT NULL DEFAULT '{}',
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT OR IGNORE INTO preferences (preferences) VALUES ('{}');

-- UI state (window geometry, etc.)
CREATE TABLE IF NOT EXISTS ui_state (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT OR IGNORE INTO ui_state (key, value) VALUES ('window_state', '{"width":1024,"height":768,"x":0,"y":0}');

-- MLS StorageProvider backend.
-- All openmls state is stored here via MlsStore (src/signal/mls_storage.rs).
-- scope = entity-type discriminator string (see MLSProgress.md for convention).
-- key   = serde_json-serialised lookup key (hash_ref, group_id, public_key, …).
-- value = serde_json-serialised entity value.
CREATE TABLE IF NOT EXISTS mls_kv (
    scope TEXT    NOT NULL,
    key   BLOB    NOT NULL,
    value BLOB    NOT NULL,
    PRIMARY KEY (scope, key)
);

