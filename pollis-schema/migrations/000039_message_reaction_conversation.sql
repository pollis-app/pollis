-- #1433: a reaction records the conversation it was membership-gated against.
--
-- Reading a message's reactions (`/v1/messages/lookup`, `reactions_for`) was
-- gated on the message's `message_envelope` row alone, and envelope GC (#689)
-- deletes that row as soon as every member device has read past it. From then
-- on the message had no reactions for anyone: existing ones vanished, and a new
-- one was written (the add gate falls back to the client's declared
-- conversation since #1161) but never read back.
--
-- The add path now stamps the conversation it gated on here, and the read gate
-- shows a row to a member of that conversation whether or not the envelope
-- survives. Rows from before this migration carry NULL and stay envelope-gated;
-- the UPDATE below stamps every one whose envelope still exists, so only
-- reactions on already-collected messages stay hidden, and re-adding one
-- stamps it.
--
-- Additive and backward-compatible: a nullable ADD COLUMN, an index, and a
-- backfill of that new column. A DS that predates it neither reads nor writes
-- the column.
ALTER TABLE message_reaction ADD COLUMN conversation_id TEXT;

CREATE INDEX IF NOT EXISTS idx_reaction_conversation ON message_reaction(conversation_id);

UPDATE message_reaction
   SET conversation_id = (
       SELECT e.conversation_id FROM message_envelope e
        WHERE e.id = message_reaction.message_id
        LIMIT 1
   )
 WHERE conversation_id IS NULL;
