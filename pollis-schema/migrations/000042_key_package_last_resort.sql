-- #1571: a last-resort KeyPackage per device per ciphersuite (RFC 9420 §16.8).
--
-- WHY. A device's pool is five one-shot packages. Every add spends one, a claim
-- whose commit loses its epoch race spends one for nothing, and the pool only
-- refills when that device itself runs. A device whose pool is empty is not
-- even LISTED as claimable, so reconcile never sees it: it is left out of every
-- new conversation with no error anywhere, and its user finds the conversation
-- empty on it. A last-resort package is the one handed out when the pool is
-- empty and never consumed, so a registered device always has something a
-- committer can add it with. openmls keeps a last-resort package's private key
-- after a join (`keys_for_welcome` skips the delete), so the same package can
-- admit the device to any number of groups.
--
-- Additive and backward-compatible (CLAUDE.md migration rule):
--
-- * One ADD COLUMN, NOT NULL with a DEFAULT, so every existing row is a regular
--   package the instant this runs, and a client that predates #1571 (every
--   shipped mobile and desktop build) writes regular packages exactly as before
--   without naming the column.
-- * One partial UNIQUE index: at most one last-resort package per device per
--   suite. Existing rows all have `last_resort = 0`, so it is already satisfied.
-- * One trigger that makes "a claimed last-resort package" unrepresentable.
--   It RESETS the flag rather than aborting the statement, on purpose: a
--   Delivery Service that predates #1571 (a rollback) knows nothing of the
--   column and may pick the last-resort row in its claim `UPDATE … RETURNING`.
--   Aborting would turn that into a 500 for every add of the device; resetting
--   hands the old DS the bytes (RETURNING reports the claimed row) and leaves
--   the package claimable for the next add, which is what last-resort means.
--   SQLite's `recursive_triggers` is off by default, so the inner UPDATE does
--   not re-fire it, and its WHEN clause would not match it anyway.
ALTER TABLE mls_key_package ADD COLUMN last_resort INTEGER NOT NULL DEFAULT 0;

CREATE UNIQUE INDEX IF NOT EXISTS idx_mls_kp_last_resort
    ON mls_key_package (user_id, device_id, ciphersuite)
    WHERE last_resort = 1;

CREATE TRIGGER IF NOT EXISTS mls_key_package_last_resort_never_claimed
AFTER UPDATE OF claimed ON mls_key_package
WHEN NEW.last_resort = 1 AND NEW.claimed = 1
BEGIN
    UPDATE mls_key_package SET claimed = 0 WHERE ref_hash = NEW.ref_hash;
END;
