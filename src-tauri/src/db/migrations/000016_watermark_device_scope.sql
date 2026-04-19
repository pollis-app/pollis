-- Device-scope the conversation watermark so multi-device users don't miss
-- messages when one device syncs while another is offline. See issue #162.
--
-- SQLite can't alter PRIMARY KEY in place; we rebuild the table. Existing
-- user-scoped rows have no device_id to migrate to, so they're dropped. The
-- worst case is that cleanup stalls per conversation until every active
-- device re-fetches and writes a fresh watermark — strictly safer than the
-- bug this migration fixes.
DROP TABLE IF EXISTS conversation_watermark;

CREATE TABLE conversation_watermark (
    conversation_id TEXT NOT NULL,
    user_id         TEXT NOT NULL,
    device_id       TEXT NOT NULL,
    last_fetched_at TEXT NOT NULL,
    PRIMARY KEY (conversation_id, user_id, device_id)
);

CREATE INDEX IF NOT EXISTS idx_watermark_user_device
    ON conversation_watermark(user_id, device_id);

INSERT INTO schema_migrations (version, description) VALUES
    (16, 'device-scope conversation_watermark (fixes #162)');
