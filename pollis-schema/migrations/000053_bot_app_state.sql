-- #2004: App key-value state, sealed by the bot's hosts and shared by all of
-- them. The DS holds ciphertext and sizes only:
--   * `key_id` is HMAC-SHA256 of the App's key under a key only the bot's
--     hosts derive (from the bot's account secret), so key names never leave
--     the hosts;
--   * `sealed` is ChaCha20-Poly1305 over the key and value, under a second key
--     derived the same way, bound to the installation and `key_id`.
--
-- Optimistic versioning: a host writes a key only at the `version` it read
-- (0 = absent), all of a job's writes in one transaction, so two hosts can
-- never clobber each other; the loser re-runs its job on the new state.
--
-- A job's writes are committed only under a trigger claim this device won,
-- once: `bot_trigger_claim.state_committed` flips 0 -> 1 in the same
-- transaction, so a retried job never applies its state twice.
--
-- WHAT THE DATABASE REFUSES, with no pollis-delivery code in the path:
--   * state for an installation that is not a bot;
--   * a row over 70,000 sealed bytes, and an installation over 1 MiB of
--     sealed state in total (`BOT_APP_STATE_MAX_BYTES` in bots.rs);
--   * a version that does not move forward by exactly one;
--   * a claim's `state_committed` going back from 1 to 0.
--
-- Deleted with the bot (teardown.rs, through `purge_bot_rows_of`). Additive:
-- one new table, one defaulted column with an already-satisfied CHECK.

CREATE TABLE IF NOT EXISTS bot_app_state (
    installation_id TEXT NOT NULL CHECK (length(installation_id) BETWEEN 1 AND 128),
    key_id          BLOB NOT NULL CHECK (length(key_id) = 32),
    sealed          BLOB NOT NULL CHECK (length(sealed) BETWEEN 30 AND 70000),
    version         INTEGER NOT NULL CHECK (version >= 1),
    -- Unix seconds.
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (installation_id, key_id)
);

ALTER TABLE bot_trigger_claim ADD COLUMN state_committed INTEGER NOT NULL DEFAULT 0
    CHECK (state_committed IN (0, 1));

CREATE TRIGGER IF NOT EXISTS bot_app_state_insert_guard
BEFORE INSERT ON bot_app_state
FOR EACH ROW
WHEN NOT EXISTS (SELECT 1 FROM users WHERE id = NEW.installation_id AND kind = 'bot')
  OR NEW.version != 1
  OR (SELECT COALESCE(SUM(length(sealed)), 0) FROM bot_app_state
      WHERE installation_id = NEW.installation_id) + length(NEW.sealed) > 1048576
BEGIN
    SELECT RAISE(ABORT, 'bot app state: not a bot, wrong version or over the installation quota');
END;

CREATE TRIGGER IF NOT EXISTS bot_app_state_update_guard
BEFORE UPDATE ON bot_app_state
FOR EACH ROW
WHEN NEW.installation_id != OLD.installation_id
  OR NEW.key_id != OLD.key_id
  OR NEW.version != OLD.version + 1
  OR (SELECT COALESCE(SUM(length(sealed)), 0) FROM bot_app_state
      WHERE installation_id = NEW.installation_id)
     - length(OLD.sealed) + length(NEW.sealed) > 1048576
BEGIN
    SELECT RAISE(ABORT, 'bot app state: version must advance by one and stay within the installation quota');
END;

CREATE TRIGGER IF NOT EXISTS bot_trigger_claim_state_committed_once
BEFORE UPDATE OF state_committed ON bot_trigger_claim
FOR EACH ROW
WHEN OLD.state_committed = 1 AND NEW.state_committed != 1
BEGIN
    SELECT RAISE(ABORT, 'a claim''s state commit cannot be undone');
END;
