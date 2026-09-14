-- #1087 / invariant I3: a DS-assigned monotone delivery sequence replaces the
-- client timestamp as the delivery cursor, the fetch order and the GC floor.
--
-- What was wrong with the timestamp. `message_envelope.sent_at` is written by
-- the CLIENT and compared LEXICALLY, and three unrelated things read it: the
-- fetch predicate (`sent_at > last_fetched_at`), the per-device read cursor, and
-- the retention floor. So delivery depended on client clocks AND on every
-- producer agreeing byte-for-byte on a format. The hardening in #1091 bounded it
-- (canonical UTC, no further ahead than the signature window, membership-gated
-- advance), which closed the blackout attack — but the design still could not
-- state "this envelope comes after that one" without trusting a clock.
--
-- It also produced real bugs that were pure format accidents: a whole-second DS
-- stamp sorts BELOW a sub-second client stamp in the same second (`+` 0x2B < `.`
-- 0x2E), which silently buried admin tombstones, and a `datetime('now')` seed
-- sorted below every RFC 3339 stamp sharing its day because a space sorts below
-- a `T`. Both were fixed by making every writer agree on a format. An integer
-- has no format to agree on.
--
--   seq         — assigned by the DS, strictly increasing per conversation.
--   last_seq    — the per-(conversation, user, device) cursor, monotone.
--
-- `sent_at` stays, as DISPLAY metadata only. Nothing routes on it after this.

ALTER TABLE message_envelope ADD COLUMN seq INTEGER;
ALTER TABLE conversation_watermark ADD COLUMN last_seq INTEGER;

-- The counter lives in its OWN table, and that is load-bearing rather than
-- tidy. The obvious implementation — `MAX(seq)+1` over `message_envelope` — is
-- WRONG, because envelope GC deletes rows: once every member device has read
-- past everything, the conversation is emptied, `MAX(seq)` goes NULL, and the
-- next envelope is assigned 1 again. Every device's cursor is already at or
-- above 1, so `seq > last_seq` never selects it and the message is delivered to
-- nobody. That is exactly the #692 shape the timestamp design suffered, and it
-- reappears in the sequence design unless the high-water mark outlives the rows.
--
-- `conversation_seq` is never pruned by GC. It is deleted only when the
-- conversation itself is torn down, at which point the cursors go with it.
CREATE TABLE IF NOT EXISTS conversation_seq (
    conversation_id TEXT PRIMARY KEY,
    next_seq        INTEGER NOT NULL DEFAULT 0
);

-- Backfill in the order the old cursor would have delivered them, so a
-- conversation that already holds envelopes keeps its history in one order:
-- `sent_at` then `id`, exactly the ORDER BY the fetch used.
-- A window function, not a correlated COUNT over the same table: the latter is
-- quadratic per conversation, which is fine on an empty database and a deploy
-- timeout on a full one. `ROW_NUMBER()` is one sort.
WITH ranked AS (
    SELECT id,
           ROW_NUMBER() OVER (
               PARTITION BY conversation_id
               ORDER BY sent_at, id
           ) AS rn
      FROM message_envelope
)
UPDATE message_envelope
   SET seq = (SELECT rn FROM ranked WHERE ranked.id = message_envelope.id)
 WHERE seq IS NULL;

-- And map every existing cursor onto the new space: the highest seq at or below
-- where that device had read to. A device that had read nothing maps to 0, which
-- is below every assigned seq (they start at 1).
UPDATE conversation_watermark
   SET last_seq = COALESCE(
       (SELECT MAX(e.seq)
          FROM message_envelope e
         WHERE e.conversation_id = conversation_watermark.conversation_id
           AND e.sent_at <= conversation_watermark.last_fetched_at),
       0)
 WHERE last_seq IS NULL;

-- Seed the counter above whatever the backfill assigned, so a conversation that
-- already holds envelopes continues rather than restarts.
INSERT INTO conversation_seq (conversation_id, next_seq)
SELECT conversation_id, MAX(seq) FROM message_envelope
 WHERE seq IS NOT NULL
 GROUP BY conversation_id
    ON CONFLICT(conversation_id) DO UPDATE SET
       next_seq = MAX(next_seq, excluded.next_seq);

-- Uniqueness is still a SCHEMA rule, as defence in depth: the counter hands out
-- each value once, and this index is what makes a second writer that somehow
-- reused one fail loudly instead of silently giving two envelopes the same
-- cursor position. Partial, so rows that predate the column do not collide on
-- NULL.
CREATE UNIQUE INDEX IF NOT EXISTS idx_envelope_conv_seq
    ON message_envelope(conversation_id, seq)
    WHERE seq IS NOT NULL;
