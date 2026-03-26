-- Speed up poll_pending_messages by indexing only undelivered envelopes.
-- Partial index stays small and shrinks as messages are delivered.
CREATE INDEX IF NOT EXISTS idx_envelope_undelivered
    ON message_envelope(conversation_id, delivered)
    WHERE delivered = 0;

INSERT INTO schema_migrations (version, description) VALUES
    (2, 'partial index on message_envelope for undelivered polling');
