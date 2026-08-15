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
    delivered INTEGER NOT NULL DEFAULT 0,
    edited_at TEXT,
    deleted_at TEXT,
    -- ULID of the thread root this message replies into, NULL for an ordinary
    -- channel message (#825). Only ever populated from the decrypted MLS
    -- payload — it is never sent to, or read from, the DS.
    --
    -- This column is ALSO added to pre-existing databases by
    -- `add_missing_columns` in `local.rs`; CREATE TABLE IF NOT EXISTS is a
    -- no-op once the table exists, so this line only serves fresh DBs.
    thread_id TEXT
);

CREATE INDEX IF NOT EXISTS idx_message_conversation ON message(conversation_id, sent_at);

-- Thread lookup: fetching one thread's replies, and counting replies per root.
-- Created on every open like the eviction index above, so existing DBs gain it
-- without a schema-version bump (which would wipe history).
CREATE INDEX IF NOT EXISTS idx_message_thread ON message(thread_id, sent_at);

-- Eviction scan index: lookback/retention sweeps delete by received_at. Run on
-- every open (this schema is re-applied each open) so existing DBs gain it
-- without a schema-version bump (which would wipe history).
CREATE INDEX IF NOT EXISTS idx_message_received_at ON message(received_at);

CREATE TABLE IF NOT EXISTS dm_conversation (
    id TEXT PRIMARY KEY,
    peer_user_id TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- User preferences (local-first mirror of remote user_preferences).
-- Single-row table — the DB file is already scoped to one user.
-- No seed row: a missing (or literal '{}') row tells get_preferences to pull
-- from remote and cache here. save_preferences upserts into this table.
CREATE TABLE IF NOT EXISTS preferences (
    preferences TEXT NOT NULL DEFAULT '{}',
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

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

-- When this device last rotated its OWN leaf in a group (issue #666).
-- openmls keeps `unmerged_leaves` crate-private, so a device cannot ask the
-- ratchet tree "is my leaf still unmerged?"; it tracks its own rotations here
-- instead. Absence of a row means "never rotated" — which is exactly the
-- post-join state whose unmerged leaf inflates every other member's commits.
CREATE TABLE IF NOT EXISTS mls_self_update (
    conversation_id TEXT PRIMARY KEY,
    last_epoch      INTEGER NOT NULL,
    last_at         TEXT NOT NULL
);

-- Which suite GENERATION of a conversation's MLS group this device is on
-- (#454 P4). MLS binds the ciphersuite into the group, so upgrading a live
-- conversation to the post-quantum hybrid suite means standing up a SUCCESSOR
-- group and moving the roster into it — a second lineage, restarting at epoch 0.
--
-- Absence of a row means generation 0, which is every group that exists today,
-- so this table is inert until a conversation actually migrates. The value is
-- the lineage this device currently READS AND WRITES; a successor group that has
-- been created locally but not yet adopted (its predecessor is still being
-- drained) is stored under its own openmls GroupId and is invisible until this
-- row advances. See `commands/mls/generation.rs`.
CREATE TABLE IF NOT EXISTS mls_generation (
    conversation_id TEXT PRIMARY KEY,
    generation      INTEGER NOT NULL,
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS user_cache (
    id         TEXT PRIMARY KEY,
    username   TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Safety-number / contact verification pins (Signal-style).
-- TOFU: the first account_id_pub seen for a peer is stored here. A later
-- mismatch is surfaced as a "changed" status and clears `verified`. Lives
-- in the local (secrets) DB so a malicious Turso write to
-- users.account_id_pub is detectable client-side.
CREATE TABLE IF NOT EXISTS contact_verification (
    peer_user_id     TEXT PRIMARY KEY,
    account_id_pub   BLOB NOT NULL,
    identity_version INTEGER NOT NULL,
    verified         INTEGER NOT NULL DEFAULT 0,
    first_seen_at    TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now'))
);


-- Saved / bookmarked messages (#854).
--
-- DEVICE-LOCAL BY DESIGN. Bookmarks are never sent to the DS or Turso and there
-- is no sync endpoint for them. Which messages a user chose to save is exactly
-- the kind of per-message interest metadata the threat model says the untrusted
-- server must never learn — syncing it would hand the server a ranked list of
-- the conversations and moments a user cares most about, which is strictly more
-- than it learns from envelope delivery alone. The cost is that bookmarks do not
-- follow you to a new device, which is consistent with accepted loss (2) in
-- CLAUDE.md: a new device starts empty and has no history to bookmark anyway.
--
-- No foreign key to `message`: a bookmark is allowed to outlive local retention
-- eviction. The saved-items list resolves each row against `message` at read
-- time and renders an honest "you do not have this message" placeholder when the
-- join misses, rather than making the row silently disappear.
--
-- Additive CREATE TABLE IF NOT EXISTS — this file is re-applied on every open,
-- so existing databases gain the table with no LOCAL_SCHEMA_VERSION bump (a bump
-- would DELETE the user's message history; see the comment in local.rs).
CREATE TABLE IF NOT EXISTS bookmark (
    message_id      TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The saved-items surface lists newest-first across all conversations.
CREATE INDEX IF NOT EXISTS idx_bookmark_created_at ON bookmark(created_at DESC);
