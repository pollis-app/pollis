-- #2005: App approvals. An installation runs exactly the App an admin approved,
-- with exactly the grants the admin chose; the runtime proves both to itself
-- from these signed records, without trusting the DS.
--
--   * `bot_app_approval` holds each approval as an opaque record signed by the
--     approving admin's device (ML-DSA-44). The DS holds no admin key, so it
--     cannot forge one; the runtime verifies the signature, the signer's
--     certificate and admin role from its own MLS view before every load.
--     `manifest` is the manifest text the record's `manifest_hash` names.
--   * `bot_app_report` holds what a bot's runtime last found (running, awaiting
--     approval of the artifact it has, or refused and why), one row per
--     installation, shown to the group's admins.
--
-- WHAT THE DATABASE REFUSES, with no pollis-delivery code in the path:
--   * an approval or report for an installation that is not a bot;
--   * an approval whose `seq` is not exactly one more than the latest stored
--     for its installation (so `seq` increases by one with every approval);
--   * changing or deleting an approval while its bot exists (records are
--     append-only; they go with the bot);
--   * a `kind` other than production or development, a report `state` other
--     than running, awaiting_approval or refused, and oversized fields.
--
-- Deleted with the bot (`bots::purge_bot_rows_of`). Additive: two new tables.

CREATE TABLE IF NOT EXISTS bot_app_approval (
    installation_id    TEXT NOT NULL CHECK (length(installation_id) BETWEEN 1 AND 128),
    seq                INTEGER NOT NULL CHECK (seq BETWEEN 1 AND 1000000),
    kind               TEXT NOT NULL CHECK (kind IN ('production', 'development')),
    record             TEXT NOT NULL CHECK (length(record) BETWEEN 1 AND 32768),
    manifest           TEXT NOT NULL CHECK (length(manifest) BETWEEN 2 AND 65536),
    approver_user_id   TEXT NOT NULL CHECK (length(approver_user_id) BETWEEN 1 AND 64),
    approver_device_id TEXT NOT NULL CHECK (length(approver_device_id) BETWEEN 1 AND 64),
    created_at         TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (installation_id, seq)
);

CREATE TRIGGER IF NOT EXISTS bot_app_approval_insert_guard
BEFORE INSERT ON bot_app_approval
FOR EACH ROW
WHEN NOT EXISTS (SELECT 1 FROM users WHERE id = NEW.installation_id AND kind = 'bot')
  OR NEW.seq != (SELECT COALESCE(MAX(seq), 0) + 1 FROM bot_app_approval
                 WHERE installation_id = NEW.installation_id)
BEGIN
    SELECT RAISE(ABORT, 'bot app approval: not a bot, or seq is not the next one');
END;

CREATE TRIGGER IF NOT EXISTS bot_app_approval_immutable
BEFORE UPDATE ON bot_app_approval
FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'bot app approvals are append-only');
END;

CREATE TRIGGER IF NOT EXISTS bot_app_approval_kept_while_the_bot_exists
BEFORE DELETE ON bot_app_approval
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM users WHERE id = OLD.installation_id)
BEGIN
    SELECT RAISE(ABORT, 'bot app approvals go only with their bot');
END;

CREATE TABLE IF NOT EXISTS bot_app_report (
    installation_id TEXT PRIMARY KEY CHECK (length(installation_id) BETWEEN 1 AND 128),
    state           TEXT NOT NULL CHECK (state IN ('running', 'awaiting_approval', 'refused')),
    reason          TEXT NOT NULL CHECK (length(reason) <= 64),
    detail          TEXT NOT NULL CHECK (length(detail) <= 500),
    app_hash        TEXT CHECK (app_hash IS NULL OR length(app_hash) = 64),
    manifest        TEXT CHECK (manifest IS NULL OR length(manifest) BETWEEN 2 AND 65536),
    source          TEXT CHECK (source IS NULL OR length(source) BETWEEN 1 AND 2048),
    developer_key   TEXT CHECK (developer_key IS NULL OR length(developer_key) BETWEEN 1 AND 4096),
    device_id       TEXT NOT NULL CHECK (length(device_id) BETWEEN 1 AND 64),
    updated_at      TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TRIGGER IF NOT EXISTS bot_app_report_bot_only
BEFORE INSERT ON bot_app_report
FOR EACH ROW
WHEN NOT EXISTS (SELECT 1 FROM users WHERE id = NEW.installation_id AND kind = 'bot')
BEGIN
    SELECT RAISE(ABORT, 'bot app report: not a bot');
END;
