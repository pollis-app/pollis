-- Multi-device support: per-device identity and MLS key material.
-- Run against Turso manually.

-- Track all devices registered to a user.
CREATE TABLE IF NOT EXISTS user_device (
    device_id   TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_name TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_user_device_user ON user_device(user_id);

-- Scope key packages to individual devices.
ALTER TABLE mls_key_package ADD COLUMN device_id TEXT;

-- Scope welcome delivery to individual devices.
ALTER TABLE mls_welcome ADD COLUMN recipient_device_id TEXT;

INSERT INTO schema_migrations (version, description) VALUES
    (11, 'multi-device support: user_device table, device-scoped key packages and welcomes');
