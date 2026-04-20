CREATE TABLE account_recovery (
    user_id          TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    identity_version INTEGER NOT NULL,
    salt             BLOB NOT NULL,
    nonce            BLOB NOT NULL,
    wrapped_key      BLOB NOT NULL,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE attachment_object (
    content_hash  TEXT PRIMARY KEY,
    r2_key        TEXT NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE channels (
    id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    channel_type TEXT NOT NULL DEFAULT 'text',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE conversation_watermark (
    conversation_id TEXT NOT NULL,
    user_id         TEXT NOT NULL,
    device_id       TEXT NOT NULL,
    last_fetched_at TEXT NOT NULL,
    PRIMARY KEY (conversation_id, user_id, device_id)
);
CREATE TABLE device_enrollment_request (
    id                       TEXT PRIMARY KEY,
    user_id                  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    new_device_id            TEXT NOT NULL,
    new_device_ephemeral_pub BLOB NOT NULL,
    verification_code        TEXT NOT NULL,
    wrapped_account_key      BLOB,
    status                   TEXT NOT NULL
        CHECK (status IN ('pending', 'approved', 'rejected', 'expired')),
    created_at               TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at               TEXT NOT NULL,
    approved_by_device_id    TEXT
);
CREATE TABLE dm_channel (
    id TEXT PRIMARY KEY,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE dm_channel_member (
    dm_channel_id TEXT NOT NULL REFERENCES dm_channel(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    added_by TEXT NOT NULL,
    added_at TEXT NOT NULL DEFAULT (datetime('now')), accepted_at TEXT,
    PRIMARY KEY (dm_channel_id, user_id)
);
CREATE TABLE group_invite (
    id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    inviter_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    invitee_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'declined'))
);
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
CREATE TABLE group_member (
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'member',
    joined_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (group_id, user_id)
);
CREATE TABLE groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    icon_url TEXT,
    owner_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE message_envelope (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    ciphertext TEXT NOT NULL,
    reply_to_id TEXT,
    sent_at TEXT NOT NULL,
    delivered INTEGER NOT NULL DEFAULT 0
, type TEXT NOT NULL DEFAULT 'message', target_message_id TEXT);
CREATE TABLE message_reaction (
    id         TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    emoji      TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(message_id, user_id, emoji)
);
CREATE TABLE mls_commit_log (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    epoch           INTEGER NOT NULL,      -- MLS epoch after this commit
    sender_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    commit_data     BLOB NOT NULL,         -- TLS-serialised MlsMessageOut (Commit)
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
, added_user_id TEXT, added_device_ids TEXT);
CREATE TABLE mls_group_info (
    conversation_id      TEXT PRIMARY KEY,
    epoch                INTEGER NOT NULL,
    group_info           BLOB NOT NULL,
    updated_at           TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by_device_id TEXT NOT NULL
);
CREATE TABLE mls_key_package (
    ref_hash    TEXT PRIMARY KEY,          -- KeyPackageRef hash (hex)
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    key_package BLOB NOT NULL,             -- TLS-serialised KeyPackage
    claimed     INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
, device_id TEXT);
CREATE TABLE mls_welcome (
    id              TEXT PRIMARY KEY,      -- ULID
    conversation_id TEXT NOT NULL,
    recipient_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    welcome_data    BLOB NOT NULL,         -- TLS-serialised Welcome
    delivered       INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
, recipient_device_id TEXT);
CREATE TABLE security_event (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    device_id  TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    metadata   TEXT
);
CREATE TABLE user_block (
    blocker_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    blocked_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (blocker_id, blocked_id)
);
CREATE TABLE user_device (
    device_id   TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_name TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen   TEXT NOT NULL DEFAULT (datetime('now'))
, device_cert BLOB, cert_issued_at TEXT, cert_identity_version INTEGER, mls_signature_pub BLOB);
CREATE TABLE user_preferences (
    user_id    TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    preferences TEXT NOT NULL DEFAULT '{}',
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE "users" (
    id           TEXT PRIMARY KEY,
    email        TEXT NOT NULL UNIQUE,
    username     TEXT NOT NULL UNIQUE,
    phone        TEXT,
    avatar_url   TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
, account_id_pub BLOB, identity_version INTEGER NOT NULL DEFAULT 1);

CREATE INDEX idx_block_blocked ON user_block(blocked_id);
CREATE INDEX idx_dm_member_user     ON dm_channel_member(user_id);
CREATE INDEX idx_enrollment_user_pending
    ON device_enrollment_request(user_id, status)
    WHERE status = 'pending';
CREATE INDEX idx_envelope_channel_time
    ON message_envelope(conversation_id, sent_at DESC, id);
CREATE UNIQUE INDEX idx_envelope_one_edit_per_message
    ON message_envelope(conversation_id, target_message_id)
    WHERE type = 'edit';
CREATE INDEX idx_envelope_undelivered
    ON message_envelope(conversation_id, delivered)
    WHERE delivered = 0;
CREATE INDEX idx_invite_group   ON group_invite(group_id, status);
CREATE INDEX idx_invite_invitee ON group_invite(invitee_id, status);
CREATE INDEX idx_join_request_group     ON group_join_request(group_id, status);
CREATE INDEX idx_join_request_requester ON group_join_request(requester_id, status);
CREATE UNIQUE INDEX idx_join_request_unique
    ON group_join_request(group_id, requester_id);
CREATE INDEX idx_mls_commit_conv ON mls_commit_log(conversation_id, seq);
CREATE INDEX idx_mls_kp_user ON mls_key_package(user_id, claimed);
CREATE INDEX idx_mls_welcome_recip ON mls_welcome(recipient_id, delivered);
CREATE INDEX idx_reaction_message ON message_reaction(message_id, created_at);
CREATE INDEX idx_security_event_user
    ON security_event(user_id, created_at DESC);
CREATE INDEX idx_user_device_user ON user_device(user_id);
CREATE INDEX idx_users_username ON users(username);
CREATE INDEX idx_watermark_user_device
    ON conversation_watermark(user_id, device_id);
