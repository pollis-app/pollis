-- Voice channel presence tracking.
-- One row per user per channel while they are in a voice call.
-- Records are deleted on graceful leave, and cleaned up on crash via
-- the group room ParticipantDisconnected event observed by other online members.

CREATE TABLE IF NOT EXISTS voice_presence (
    user_id      TEXT NOT NULL,
    group_id     TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    channel_id   TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    joined_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (user_id, channel_id)
);

CREATE INDEX IF NOT EXISTS idx_voice_presence_channel ON voice_presence(channel_id);
CREATE INDEX IF NOT EXISTS idx_voice_presence_group   ON voice_presence(group_id);
