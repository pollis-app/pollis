-- Device push-notification tokens. Refs issue #344.
--
-- Mobile clients register an Expo push token (one per device install) so a
-- sender — possibly a different user on a different device — can deliver a
-- content-free wake/notify to the recipient's backgrounded or closed app.
-- Foreground delivery flows over the LiveKit realtime path instead; this
-- table is only consulted by `send_message`'s background push fanout
-- (`commands::push::notify_new_message`).
--
-- Content note: the token routes a notification that carries ONLY
-- {conversationId, kind} — never message plaintext, sender, or any content.
-- That is consistent with the security model (Turso already sees message
-- metadata; APNs/FCM see no more than the conversation id).
--
-- `token` is the primary key: an Expo push token is unique per device
-- install, so a re-register from the same device (e.g. after switching
-- accounts) upserts the owning user_id/platform rather than duplicating.
-- Desktop never registers a token, so desktop-only users simply have no
-- rows here and the fanout is a no-op for them.
CREATE TABLE IF NOT EXISTS push_token (
    token      TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL,
    platform   TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- The fanout looks tokens up by recipient user_id.
CREATE INDEX IF NOT EXISTS idx_push_token_user ON push_token(user_id);
