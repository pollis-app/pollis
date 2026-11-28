-- #2180: the database assigns every envelope its delivery sequence (#1087).
--
-- Until now the DS allocated `seq` itself, inside a `BEGIN IMMEDIATE`
-- transaction: bump `conversation_seq`, read the new value back, insert the row
-- with it, commit. Correct, but it held Turso's database-wide write lock across
-- four or five round trips per send, which capped total send capacity no matter
-- how many conversations were active. "Allocation and insert land together" was
-- code discipline (a transaction the caller had to open).
--
-- Here it moves down to the schema, the lowest layer:
--
--   * `message_envelope_assign_seq` (AFTER INSERT, only when the row came
--     without a seq): bumps the conversation's counter and stamps the row with
--     the new value. A SQLite statement is atomic, so the INSERT, the bump and
--     the stamp commit or roll back together: an envelope can no longer exist
--     without its seq, and a failed insert consumes none.
--
--   * `message_envelope_seq_from_counter` (BEFORE INSERT, only when the row
--     names a seq): refuses any seq that is not the counter's current value.
--     A writer that bypassed the counter (a `MAX(seq)+1`, a copy with old seqs)
--     would reissue a seq below every device's cursor, and that envelope would
--     be delivered to nobody (#692). The DS deployed before this migration
--     upserts the counter and inserts exactly the value it got back, inside one
--     transaction, so it passes this check unchanged: old and new DS can write
--     the same database during a rolling deploy or a rollback.
--
-- The counter is `conversation_seq`, never `MAX(seq)`: envelope GC empties a
-- conversation and MAX would restart at 1 (see 000028). `WHERE id = NEW.id`
-- stamps the inserted row by its primary key.
--
-- The DS refuses to start without both triggers, and every envelope INSERT is
-- itself conditioned on both existing (`messages::insert_envelope`), so a
-- database this migration never reached fails a send loudly instead of storing
-- an envelope with no seq. Apply it BEFORE deploying the DS that relies on it.
--
-- Additive (CLAUDE.md migration rule): CREATE TRIGGER only; `IF NOT EXISTS`
-- makes a re-apply a no-op. No row that exists today violates either trigger,
-- and neither fires on UPDATE or DELETE.

CREATE TRIGGER IF NOT EXISTS message_envelope_seq_from_counter
BEFORE INSERT ON message_envelope
FOR EACH ROW
WHEN NEW.seq IS NOT NULL
 AND NEW.seq IS NOT (SELECT next_seq FROM conversation_seq
                      WHERE conversation_id = NEW.conversation_id)
BEGIN
    SELECT RAISE(ABORT, 'message_envelope.seq must be the conversation_seq counter (#1087)');
END;

CREATE TRIGGER IF NOT EXISTS message_envelope_assign_seq
AFTER INSERT ON message_envelope
FOR EACH ROW
WHEN NEW.seq IS NULL
BEGIN
    INSERT INTO conversation_seq (conversation_id, next_seq)
    VALUES (NEW.conversation_id, 1)
    ON CONFLICT(conversation_id) DO UPDATE SET next_seq = next_seq + 1;
    UPDATE message_envelope
       SET seq = (SELECT next_seq FROM conversation_seq
                   WHERE conversation_id = NEW.conversation_id)
     WHERE id = NEW.id;
END;
