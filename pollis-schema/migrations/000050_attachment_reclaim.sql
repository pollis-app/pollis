-- Attachment references that last as long as the message does, and server-side
-- reclaim of an object once every message referencing it is deleted (#1881).
--
-- A message stays in its recipients' history after the Delivery Service
-- collects its envelope (envelope GC, the #720 window, account deletion,
-- conversation teardown), and recipients fetch its attachment from R2 lazily.
-- So a reference is released only by an explicit delete of that message (or of
-- the Vault entry holding the file), never by an envelope going away. The
-- reference count is the `attachment_ref` row itself.
--
-- `attachment_ref.delete_token_hash`: the referencing message's deletion
-- capability hash (#1086), copied from its envelope at registration and
-- backfilled below. It lets the author's delete release the reference after
-- the envelope itself is gone. A hash, not the capability; the envelope
-- carried the same value.
--
-- `attachment_object.reclaim_state`: NULL while referenced or untouched;
-- 'released' once the delete that released the last reference has run;
-- 'deleting' while the DS deletes the R2 object. The row is deleted only after
-- the R2 delete succeeds, so a failed delete is retried on a later pass. A new
-- reference to a 'released' object clears the state.
--
-- `idx_attachment_ref_message`: the release looks references up by message.
--
-- Additive and backward-compatible: nullable columns, two indexes and a
-- backfill of the new column. Shipped clients never read any of it.
ALTER TABLE attachment_ref ADD COLUMN delete_token_hash TEXT;
ALTER TABLE attachment_object ADD COLUMN reclaim_state TEXT
    CHECK (reclaim_state IN ('released', 'deleting'));
CREATE INDEX IF NOT EXISTS idx_attachment_ref_message ON attachment_ref(message_id);
CREATE INDEX IF NOT EXISTS idx_attachment_object_reclaim
    ON attachment_object(reclaim_state) WHERE reclaim_state IS NOT NULL;
UPDATE attachment_ref
   SET delete_token_hash = (SELECT me.delete_token_hash FROM message_envelope me
                             WHERE me.id = attachment_ref.message_id)
 WHERE delete_token_hash IS NULL;
