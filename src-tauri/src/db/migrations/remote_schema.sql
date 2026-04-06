-- Single source of truth for the remote Turso schema.
-- Edit this file when the schema changes, then run: pnpm db:push
-- The migrate binary drops all existing tables/indexes before applying this.

-- Core tables
CREATE TABLE users (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    username TEXT,
    phone TEXT,
    identity_key TEXT,
    avatar_url TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    icon_url TEXT,
    owner_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE group_member (
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'member',
    joined_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (group_id, user_id)
);

CREATE TABLE channels (
    id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    channel_type TEXT NOT NULL DEFAULT 'text',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- signed_prekey and one_time_prekey were Signal Protocol tables removed in
-- migration 000009. Not present in live databases.

CREATE TABLE message_envelope (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    ciphertext TEXT NOT NULL,
    reply_to_id TEXT,
    sent_at TEXT NOT NULL,
    delivered INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_envelope_channel_time
    ON message_envelope(conversation_id, sent_at DESC, id);

-- DM channels
CREATE TABLE dm_channel (
    id TEXT PRIMARY KEY,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE dm_channel_member (
    dm_channel_id TEXT NOT NULL REFERENCES dm_channel(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    added_by TEXT NOT NULL,
    added_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (dm_channel_id, user_id)
);

-- sender_key_dist and x3dh_init were Signal Protocol tables removed in
-- migration 000009. Not present in live databases.

CREATE INDEX idx_dm_member_user ON dm_channel_member(user_id);

-- Group invites (a member invites an outside user to join).
-- Rows are deleted on accept or decline — all rows in this table are implicitly pending.
CREATE TABLE group_invite (
    id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    inviter_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    invitee_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_invite_invitee ON group_invite(invitee_id);
CREATE INDEX idx_invite_group   ON group_invite(group_id);

-- Join requests (a user requests to join a group)
CREATE TABLE group_join_request (
    id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    requester_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    reviewed_by TEXT REFERENCES users(id),
    reviewed_at TEXT,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected'))
);

CREATE INDEX        idx_join_request_group     ON group_join_request(group_id, status);
CREATE INDEX        idx_join_request_requester ON group_join_request(requester_id, status);
-- One row per (group, requester) — re-applications upsert rather than insert new rows.
CREATE UNIQUE INDEX idx_join_request_unique    ON group_join_request(group_id, requester_id);

-- User preferences (stored as JSON: accent_color, font_size, etc.)
CREATE TABLE user_preferences (
    user_id    TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    preferences TEXT NOT NULL DEFAULT '{}',
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Emoji reactions on messages.
-- NOTE: This table must be created in Turso manually (or via pnpm db:push).
-- The UNIQUE constraint prevents duplicate (message, user, emoji) combos;
-- add_reaction uses INSERT OR IGNORE to silently skip duplicates.
CREATE TABLE message_reaction (
    id         TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    emoji      TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(message_id, user_id, emoji)
);

CREATE INDEX idx_reaction_message ON message_reaction(message_id, created_at);

-- Cross-user attachment deduplication registry.
-- Uses convergent encryption: SHA-256(plaintext) → deterministic key → identical ciphertext.
-- Same file uploaded by any user maps to the same R2 object; no per-user or per-message rows here.
-- Access control is enforced by MLS: the content_hash is inside the encrypted message_envelope,
-- so only members who can decrypt the message can derive the decryption key.
CREATE TABLE attachment_object (
    content_hash  TEXT PRIMARY KEY,
    r2_key        TEXT NOT NULL,
    mime_type     TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
