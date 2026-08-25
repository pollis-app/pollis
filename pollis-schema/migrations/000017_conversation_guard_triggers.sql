-- #948 — the three conversation tables become UNABLE to hold an id the
-- registry did not grant them. Phase 2 of #880, which created the
-- `conversation` registry and made every DS create path claim its id in the
-- same transaction as the row it names. That refuses a second claim *through
-- the DS*; nothing yet stopped a writer that simply skips the registry. These
-- triggers are that last step: the database itself now refuses the row.
--
-- WHY TRIGGERS AND NOT A FOREIGN KEY. Two findings from #948:
--   * production Turso runs with `foreign_keys=OFF` (`Db::connect_local`
--     primes the same pragma to match), so any FK here is inert exactly where
--     it matters. Triggers fire regardless of the pragma — the commit-log DB
--     already relies on that (`migrations-log/000005`).
--   * a composite `(id, kind)` FK would also require rebuilding all three
--     child tables to add a constant `kind` column — SQLite's 12-step
--     CREATE/copy/DROP/RENAME, the non-additive category CLAUDE.md reserves
--     for a multi-release dance. CREATE TRIGGER is additive.
--
-- VALIDATORS, NOT CLAIMERS. Each trigger raises unless the registry already
-- holds exactly (NEW.id, its table's kind); it never inserts the claim
-- itself. This is deliberate: the DS's `claim_conversation_id` stays the one
-- writer (in the same transaction, so the trigger sees the uncommitted
-- claim), which keeps this migration deployable against the RUNNING DS with
-- no code change and no deploy-order dance. A claiming trigger instead would
-- double-insert against the live DS and fail every creation until the next
-- DS deploy — and a tolerant, guarded claim would quietly re-allow same-kind
-- reuse of a retired id, which the append-only registry exists to refuse.
--
-- What each trigger refuses, with no pollis-delivery code in the path:
--   * an INSERT whose id the registry never granted (a writer that skipped
--     the claim);
--   * an INSERT whose id the registry granted to ANOTHER kind — the #879
--     attack (a dm_channel row naming a victim group's id makes
--     `is_member(victim_group, attacker)` true);
--   * an UPDATE that renames a row onto an id the registry did not grant
--     that table — the same hole through the side door.
--
-- The UPDATE triggers fire on any SET of `id` (SQLite fires UPDATE OF even
-- when the value is unchanged); a same-value SET still satisfies the check,
-- so only an actual rename onto foreign territory is refused.
--
-- Additive and backward-compatible: CREATE TRIGGER only. The shipped client
-- never writes these tables (#987 — all remote writes go through the DS),
-- and the running DS already claims before every insert, so nothing that
-- exists today trips these.

CREATE TRIGGER IF NOT EXISTS conversation_guard_groups_insert
BEFORE INSERT ON groups
FOR EACH ROW
WHEN NOT EXISTS (SELECT 1 FROM conversation WHERE id = NEW.id AND kind = 'group')
BEGIN
    SELECT RAISE(ABORT, 'conversation id not claimed as group');
END;

CREATE TRIGGER IF NOT EXISTS conversation_guard_groups_update
BEFORE UPDATE OF id ON groups
FOR EACH ROW
WHEN NOT EXISTS (SELECT 1 FROM conversation WHERE id = NEW.id AND kind = 'group')
BEGIN
    SELECT RAISE(ABORT, 'conversation id not claimed as group');
END;

CREATE TRIGGER IF NOT EXISTS conversation_guard_channels_insert
BEFORE INSERT ON channels
FOR EACH ROW
WHEN NOT EXISTS (SELECT 1 FROM conversation WHERE id = NEW.id AND kind = 'channel')
BEGIN
    SELECT RAISE(ABORT, 'conversation id not claimed as channel');
END;

CREATE TRIGGER IF NOT EXISTS conversation_guard_channels_update
BEFORE UPDATE OF id ON channels
FOR EACH ROW
WHEN NOT EXISTS (SELECT 1 FROM conversation WHERE id = NEW.id AND kind = 'channel')
BEGIN
    SELECT RAISE(ABORT, 'conversation id not claimed as channel');
END;

CREATE TRIGGER IF NOT EXISTS conversation_guard_dm_channel_insert
BEFORE INSERT ON dm_channel
FOR EACH ROW
WHEN NOT EXISTS (SELECT 1 FROM conversation WHERE id = NEW.id AND kind = 'dm')
BEGIN
    SELECT RAISE(ABORT, 'conversation id not claimed as dm');
END;

CREATE TRIGGER IF NOT EXISTS conversation_guard_dm_channel_update
BEFORE UPDATE OF id ON dm_channel
FOR EACH ROW
WHEN NOT EXISTS (SELECT 1 FROM conversation WHERE id = NEW.id AND kind = 'dm')
BEGIN
    SELECT RAISE(ABORT, 'conversation id not claimed as dm');
END;
