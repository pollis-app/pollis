-- #1423: Expo push tickets awaiting their delivery receipt.
--
-- Expo accepts a push with a per-message TICKET, and only later reports what
-- APNs/FCM made of it in a RECEIPT (`/--/api/v2/push/getReceipts`). The errors
-- that matter most -- `DeviceNotRegistered` after an uninstall or an FCM token
-- rotation, `InvalidCredentials` -- often show up only in the receipt. The DS
-- keeps each ticket id here until the GC sweep has asked Expo for its receipt,
-- then deletes it, and deletes the `push_token` row on `DeviceNotRegistered`.
--
-- `token` rides along so a `DeviceNotRegistered` receipt knows which
-- `push_token` row to prune. There is deliberately no user, conversation,
-- device or IP column: nothing here says who was notified about what, and the
-- token is already in `push_token`. Rows go with account deletion
-- (`teardown::purge_user_rows`, via the user's tokens) and with the token.
--
-- Bounded by the table itself: Expo keeps receipts for 24 hours, so a ticket
-- older than that can never be answered. The AFTER INSERT trigger drops such
-- rows on every insert, and the sweep drops them too, so the table cannot grow
-- past a day of pushes even if the sweep never runs.
--
-- Additive and backward-compatible: CREATE TABLE/INDEX/TRIGGER only. A DS
-- running before this migration is applied fails these inserts, which it
-- treats as best-effort and ignores; no client reads or writes this table.
CREATE TABLE IF NOT EXISTS push_ticket (
    ticket_id  TEXT PRIMARY KEY CHECK (length(ticket_id) BETWEEN 1 AND 128),
    token      TEXT NOT NULL CHECK (length(token) BETWEEN 1 AND 256),
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_push_ticket_created ON push_ticket(created_at);
CREATE INDEX IF NOT EXISTS idx_push_ticket_token ON push_ticket(token);

CREATE TRIGGER IF NOT EXISTS push_ticket_bound AFTER INSERT ON push_ticket
BEGIN
    DELETE FROM push_ticket WHERE created_at < NEW.created_at - 86400;
END;
