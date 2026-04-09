-- Add type discriminator and target reference to message_envelope so edits
-- can be delivered through the same queue as regular messages.
--
-- type: 'message' (default) or 'edit'. Kept as a plaintext column so Turso
-- can enforce the one-edit-per-message constraint without decrypting.
--
-- target_message_id: the id of the message being edited (NULL for type='message').
--
-- The partial unique index ensures at most one pending edit envelope exists per
-- message per conversation — senders upsert by deleting any prior edit then
-- inserting, so recipients always see only the latest edit on next fetch.

ALTER TABLE message_envelope ADD COLUMN type TEXT NOT NULL DEFAULT 'message';
ALTER TABLE message_envelope ADD COLUMN target_message_id TEXT;

CREATE UNIQUE INDEX idx_envelope_one_edit_per_message
    ON message_envelope(conversation_id, target_message_id)
    WHERE type = 'edit';

INSERT INTO schema_migrations (version, description) VALUES
    (10, 'message envelope type discriminator and edit support');
