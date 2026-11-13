-- #1879: the per-mailbox sign-in code limits, kept in the database so they
-- hold whatever happens to the DS process.
--
-- One row per mailbox that asked for or guessed a code recently: the resend
-- throttle (`last_sent_at`), the per-window send budget (`sends`,
-- `window_start`), the wrong-guess count (`failed`) and the lockout
-- (`locked_until`). Times are unix seconds. The codes themselves are not here;
-- the DS keeps their salted hashes in memory only.
--
-- `mailbox` is HMAC-SHA256 of the normalized address under a key only the DS
-- holds (otp.rs `MailboxKey`), hex-encoded. The CHECK admits exactly 64
-- lowercase hex characters, so an address cannot be stored here.
--
-- The table holds only mailboxes active in the last window. `expires_at` is
-- when a row stops mattering (its last code's expiry, or its lockout's end);
-- the triggers below delete every row past it on each insert or update. No
-- sweep, no timer.
--
-- Holds no user id. Additive and backward-compatible: one CREATE TABLE, one
-- index, two triggers. Shipped clients never see it.
CREATE TABLE IF NOT EXISTS otp_mailbox (
    mailbox      TEXT PRIMARY KEY CHECK (
        length(mailbox) = 64 AND mailbox NOT GLOB '*[^0-9a-f]*'
    ),
    failed       INTEGER NOT NULL DEFAULT 0 CHECK (failed >= 0),
    locked_until INTEGER,
    last_sent_at INTEGER NOT NULL DEFAULT 0,
    sends        INTEGER NOT NULL DEFAULT 0 CHECK (sends >= 0),
    window_start INTEGER,
    updated_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL CHECK (expires_at >= updated_at)
);
CREATE INDEX IF NOT EXISTS idx_otp_mailbox_expires_at ON otp_mailbox(expires_at);

CREATE TRIGGER IF NOT EXISTS otp_mailbox_prune_on_insert AFTER INSERT ON otp_mailbox
BEGIN
    DELETE FROM otp_mailbox WHERE expires_at < NEW.updated_at;
END;

CREATE TRIGGER IF NOT EXISTS otp_mailbox_prune_on_update AFTER UPDATE ON otp_mailbox
BEGIN
    DELETE FROM otp_mailbox WHERE expires_at < NEW.updated_at;
END;
