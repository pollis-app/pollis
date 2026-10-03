-- #1213: abuse reports and operator account suspension.
--
-- Messages are end-to-end encrypted, so a report can never carry content: it
-- names WHO is reported, WHY (a fixed set of reasons), and optionally WHERE
-- (the conversation and message ids, which are already server-visible
-- metadata). Nothing here can hold message text, and there is no column for
-- it to go in.
--
-- The invalid states are refused by the table itself, not by code:
--   * reporting yourself                 -> CHECK (reporter_id <> reported_id)
--   * a reason outside the set           -> CHECK (reason IN (...))
--   * a message id with no conversation  -> CHECK on the pair
--   * a report outliving either account  -> ON DELETE CASCADE on both ids, so
--     account deletion removes reports BY and ABOUT the account
--     (docs/metadata-retention-policy.md).
CREATE TABLE user_report (
    id              TEXT PRIMARY KEY,
    reporter_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    reported_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    reason          TEXT NOT NULL CHECK (reason IN ('spam', 'harassment', 'illegal', 'other')),
    conversation_id TEXT,
    message_id      TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    CHECK (reporter_id <> reported_id),
    CHECK (message_id IS NULL OR conversation_id IS NOT NULL)
);
CREATE INDEX idx_user_report_reported ON user_report(reported_id, created_at);

-- An account the operator has suspended. While a row exists, the DS refuses
-- every device-signed request from any of the account's devices (the device
-- key lookup in pollis-delivery/src/auth.rs treats it like a revoked device).
-- Written only by the operator script (scripts/suspend-account.sh); no client
-- endpoint can create or remove one. Deleting the row lifts the suspension.
CREATE TABLE account_suspension (
    user_id      TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    reason       TEXT NOT NULL CHECK (length(trim(reason)) > 0),
    suspended_at TEXT NOT NULL DEFAULT (datetime('now'))
);
