-- #H5 — `users.username` becomes UNABLE to contain an `@`.
--
-- Every "find a person by identifier" lookup on the DS (`/v1/invites/create`,
-- `/v1/directory/users`, and through the latter the client's
-- `search_user_by_username`) decides between the `email` and `username`
-- columns by whether the identifier contains `@`. That dispatch is only sound
-- if a username can never contain one. Before this migration the DS wrote any
-- string as a username (only `UNIQUE` applied), and the lookups ran
-- `WHERE username = ?1 OR email = ?1` and took the first row — SQLite's
-- multi-index OR scans the `username` term first, so an account that had set
-- its username to `alice@corp.com` was what an admin's invite to Alice
-- resolved to, and the MLS Add admitted the squatter.
--
-- The DS now refuses such a username with a 400 (`profile::is_valid_username`,
-- which is stricter: `^[a-z0-9_.-]{3,32}$`). This migration is the layer below
-- that: the database itself refuses the `@`, with no pollis-delivery code in
-- the path, so the resolvers' assumption holds against a writer that skips the
-- DS entirely (an operator with the Turso token, a future endpoint that forgets
-- to validate).
--
-- WHY TRIGGERS AND NOT A CHECK CONSTRAINT. SQLite cannot `ALTER TABLE … ADD
-- CHECK`; a CHECK on an existing table means the 12-step CREATE/copy/DROP/
-- RENAME rebuild, the non-additive category CLAUDE.md reserves for a
-- multi-release dance. `CREATE TRIGGER` is additive, and — as `000017` and the
-- commit-log DB's `000005` already rely on — fires regardless of the
-- `foreign_keys` pragma production runs with OFF.
--
-- Only `@` is refused here, not the full character rule, so that the migration
-- is ALREADY SATISFIED by every default username the DS has minted so far
-- (`<email local part>_<upper-case ULID suffix>` — a local part cannot contain
-- `@`, and the suffix is base32). Such names keep working untouched; the
-- stricter rule applies only when a username is actually changed. The UPDATE
-- trigger fires on any SET of `username`, including the COALESCE the profile
-- endpoint always issues, so a legacy row that already holds an `@` cannot
-- save its profile until it picks a real name — which is the intended
-- pressure, and the DS answers it with a 400 that says so rather than letting
-- the trigger's abort surface as a 500.
--
-- Additive and backward-compatible: CREATE TRIGGER only, no column, no index.

CREATE TRIGGER IF NOT EXISTS users_username_no_at_insert
BEFORE INSERT ON users
FOR EACH ROW
WHEN instr(NEW.username, '@') > 0
BEGIN
    SELECT RAISE(ABORT, 'username may not contain @');
END;

CREATE TRIGGER IF NOT EXISTS users_username_no_at_update
BEFORE UPDATE OF username ON users
FOR EACH ROW
WHEN instr(NEW.username, '@') > 0
BEGIN
    SELECT RAISE(ABORT, 'username may not contain @');
END;
