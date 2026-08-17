-- #880 — one namespace for conversation ids, owned by the database.
--
-- THE HOLE. `dm_channel`, `groups` and `channels` are three tables with three
-- separate primary keys, so SQLite accepts the SAME id in all three. That is not
-- a ULID collision (ULIDs do not collide by chance) — every creation endpoint
-- takes the id straight from the request body, so an attacker simply *names* an
-- existing conversation. `is_member` then ORs across all three tables on one id,
-- so creating a DM whose id is a victim group's id and adding yourself makes
-- `is_member(victim_group, you)` true: membership of a group you were never in,
-- and with it commit injection, GroupInfo/Welcome overwrite and a LiveKit token
-- for their room. #879 closed it with a `conversation_id_taken` check that every
-- `create_*` path must remember to call — code discipline, which CLAUDE.md ranks
-- LAST (DB constraint > Rust type > protocol chokepoint > code discipline).
--
-- THE REGISTRY. `conversation` owns the id namespace. One row per conversation
-- id, carrying the ONE kind that id names. The primary key is what refuses the
-- second claim, so an id that already names a group cannot be re-registered as a
-- DM or a channel — and because the DS claims the id in the SAME transaction as
-- the row it owns, a refused claim rolls the whole creation back.
--
-- APPEND-ONLY, DELIBERATELY. Rows are never deleted, not even when the
-- conversation is. A claimed id is claimed forever:
--   * releasing an id re-opens the exact window this table closes — the attack
--     is *reuse*, and a deleted conversation is the most attractive id to reuse
--     because members' clients may still hold state keyed to it;
--   * production Turso runs with `foreign_keys=OFF` (see the `Db::connect_local`
--     pragma the DS matches in its tests), so the baseline's `ON DELETE CASCADE`
--     clauses do NOT fire there. Deleting a group leaves its `group_member` and
--     `channels` rows behind, which is precisely what would make a re-registered
--     id answer `is_member` for the dead conversation's members.
-- The cost is one ~60-byte row per conversation ever created, with no user
-- linkage — the id and its kind, both of which the server already stores.
--
-- BACKFILL. Every existing conversation gets its row, oldest first, so if an id
-- is ALREADY shared across two tables (a pre-#879 attack, or a client bug) the
-- ORIGINAL conversation keeps the id and the later squatter is the one left
-- without a registry row. `OR IGNORE` because a migration that fails on data
-- aborts the release for every user; the collision audit is a query, not a
-- deploy-time abort:
--
--   SELECT id, COUNT(*) FROM (SELECT id FROM groups UNION ALL
--                             SELECT id FROM channels UNION ALL
--                             SELECT id FROM dm_channel)
--   GROUP BY id HAVING COUNT(*) > 1;
--
-- That query returning rows is a security incident, not a migration detail, and
-- it is the precondition for the tightening phase (see below).
--
-- WHAT THIS MIGRATION DOES NOT DO. It does not make the three tables REFERENCE
-- `conversation`. Adding a foreign key means rebuilding each table (SQLite's
-- 12-step CREATE/copy/DROP/RENAME), which is not additive and so is not
-- backward-compatible with the shipped app — CLAUDE.md's migration rule. Two
-- further findings shape that follow-up phase rather than this one:
--   * a bare FK per table would NOT be enough. `groups.id REFERENCES
--     conversation(id)` and `dm_channel.id REFERENCES conversation(id)` are both
--     satisfied by the SAME parent row, so the two could still share an id. The
--     tightening needs a constant `kind` column on each child and a COMPOSITE
--     `(id, kind)` foreign key, which is what `idx_conversation_id_kind` below
--     exists to back;
--   * with `foreign_keys=OFF` in production, any FK is inert there. The
--     mechanism that actually enforces this on Turso is a BEFORE INSERT TRIGGER
--     per table claiming the id — triggers fire regardless of the pragma (the
--     commit-log DB already relies on that, `migrations-log/000005`).
--
-- Additive and backward-compatible: CREATE TABLE + CREATE INDEX + an INSERT that
-- cannot fail. A shipped client never reads this table, and never wrote to any
-- of the three (all remote writes go through the DS).
CREATE TABLE IF NOT EXISTS conversation (
    id         TEXT PRIMARY KEY,
    -- The one kind of conversation this id names. Not a hint: it is the column
    -- the tightening phase's composite FK binds each child table to.
    kind       TEXT NOT NULL CHECK (kind IN ('dm', 'group', 'channel')),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Redundant against the primary key today, and deliberately so: SQLite requires
-- the parent side of a composite foreign key to be backed by a UNIQUE index, so
-- `channels (id, kind) REFERENCES conversation (id, kind)` needs this index to
-- exist BEFORE the tightening phase can rebuild the child tables. Creating it
-- now keeps that phase a child-table change only.
CREATE UNIQUE INDEX IF NOT EXISTS idx_conversation_id_kind ON conversation (id, kind);

-- Backfill, oldest first. `ORDER BY created_at` is load-bearing under
-- `OR IGNORE`: SQLite inserts in the SELECT's output order, so on a pre-existing
-- collision the row that gets the id is the one that had it first.
INSERT OR IGNORE INTO conversation (id, kind, created_at)
SELECT id, kind, created_at FROM (
    SELECT id, 'group'   AS kind, created_at FROM groups
    UNION ALL
    SELECT id, 'channel' AS kind, created_at FROM channels
    UNION ALL
    SELECT id, 'dm'      AS kind, created_at FROM dm_channel
)
ORDER BY created_at ASC;
