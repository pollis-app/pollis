-- #2056: pushes follow one set of fire and clear rules across a user's devices.
--
-- 1. Dismiss by message, not by wall clock. A push handle now records the
--    message it was sent for (`message_id`, `message_sent_at`: the envelope's
--    own id and delivery stamp, which the DS already stores). A read reports
--    the position it read up to, and the DS clears every handle at or before
--    it, so a read a second after arrival clears that push too. A handle
--    without them (minted before this migration) still clears by time.
--
-- 2. Held pushes. While one of a user's desktops reports itself active
--    (`POST /v1/push/activity`), a message push for that user is held here
--    instead of sent: one row per (user, conversation), the newest message.
--    A read of the conversation deletes the row; the user going inactive, or
--    the active report lapsing, sends one push per row and deletes it. The
--    rows outlive a DS restart: boot sends whatever is still held.
--
-- WHAT THE SERVER LEARNS: nothing it did not hold already. The message id and
-- stamp are the envelope's; that a user read up to a message is what the
-- read-receipt envelope (#892) already tells it; whether a desktop is in use
-- is a few minutes of memory, never written down.
--
-- Additive and backward-compatible: nullable ADD COLUMNs and a new table. An
-- older DS never reads or writes them.
ALTER TABLE push_handle ADD COLUMN message_id TEXT;
ALTER TABLE push_handle ADD COLUMN message_sent_at TEXT;

CREATE TABLE IF NOT EXISTS push_held (
    user_id         TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    kind            TEXT NOT NULL,
    message_id      TEXT NOT NULL,
    message_sent_at TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, conversation_id)
);
