CREATE TABLE IF NOT EXISTS conversation_watermark (
    conversation_id TEXT NOT NULL,
    user_id         TEXT NOT NULL,
    last_fetched_at TEXT NOT NULL,
    PRIMARY KEY (conversation_id, user_id)
);

INSERT INTO schema_migrations (version, description) VALUES
    (5, 'per-member conversation watermark for message_envelope cleanup');
