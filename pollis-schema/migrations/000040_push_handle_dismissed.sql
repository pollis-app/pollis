-- #1508: a notification the user has read stops being shown on every device.
--
-- A push stayed in the phone's tray after its message was read, whether the
-- read happened on the same phone (opened from the conversation list rather
-- than the notification) or on another client. When a client's read cursor
-- moves, it now tells the DS which conversations it read and how long ago
-- (`POST /v1/push/dismiss`); the DS stamps `dismissed_at` on that user's
-- handles for those conversations minted before the read, and sends the
-- user's devices one silent, data-only push listing the handles to clear.
-- The handles are the same random values the original notifications already
-- carried, so the providers learn no new identifier — only that those
-- notifications were read.
--
-- A stamped row is kept until the existing TTL sweep, rather than deleted, so
-- a phone that missed the silent push (iOS after a force-quit, Android in
-- Doze) can still ask which of its tray notifications are stale when it comes
-- to the foreground (`POST /v1/push/dismissed`); and so the next read of the
-- same conversation sends nothing for handles already cleared.
--
-- WHAT THE SERVER LEARNS, stated plainly: that this user read this
-- conversation at about this time — the same thing the read-receipt envelope
-- (#892, on by default) already tells it. The read-cursor blob (000018) stays
-- opaque; this column is not derived from it.
--
-- Additive and backward-compatible: a nullable ADD COLUMN and an index. A DS
-- that predates it neither reads nor writes the column, and a DS that has it
-- but runs before the migration answers the new endpoints as "nothing to
-- dismiss".
ALTER TABLE push_handle ADD COLUMN dismissed_at TEXT;

-- The dismiss lookup: one user's undismissed handles for a few conversations.
CREATE INDEX IF NOT EXISTS idx_push_handle_user_conversation
    ON push_handle(user_id, conversation_id);
