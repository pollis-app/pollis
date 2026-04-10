-- Tighten voice_presence to enforce single-channel-per-user per group.
-- The previous PK (user_id, channel_id) allowed a user to have rows in
-- multiple channels if their leave step failed. Adding UNIQUE(user_id, group_id)
-- makes INSERT OR REPLACE atomically evict the old channel row.

DROP TABLE IF EXISTS voice_presence;

CREATE TABLE voice_presence (
    user_id      TEXT NOT NULL,
    group_id     TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    channel_id   TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    joined_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (user_id, channel_id),
    UNIQUE (user_id, group_id)
);

CREATE INDEX idx_voice_presence_channel ON voice_presence(channel_id);
CREATE INDEX idx_voice_presence_group   ON voice_presence(group_id);

INSERT INTO schema_migrations (version, description) VALUES
    (12, 'voice_presence unique constraint on (user_id, group_id)');
