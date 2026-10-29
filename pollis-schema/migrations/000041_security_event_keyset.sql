-- #1548: the Security events page reads the log one keyset page at a time,
-- optionally filtered by kind.
--
-- The page query is
--   WHERE user_id = ? [AND kind IN (...)] [AND (created_at, id) < (?, ?)]
--   ORDER BY created_at DESC, id DESC LIMIT ?
-- and `id` is the tiebreak that makes the cursor stable when two rows share a
-- `created_at` (the column is second-granular). The baseline's
-- `idx_security_event_user (user_id, created_at DESC)` has no `id`, so the
-- unfiltered path needs a temp sort for the tiebreak; the filtered path had no
-- index on `kind` at all.
--
-- Additive and backward-compatible: two CREATE INDEX statements. A DS that
-- predates #1548 runs the same query it always ran and is served by either
-- index. The old index is now redundant but is left in place; dropping it is not
-- additive and buys nothing a later cleanup cannot.
CREATE INDEX IF NOT EXISTS idx_security_event_user_created_id
    ON security_event(user_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_security_event_user_kind
    ON security_event(user_id, kind, created_at DESC, id DESC);
