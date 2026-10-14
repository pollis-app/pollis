-- #1305: the DS's failed-request log — the server half of "why did this
-- user's request fail?", matched to the user's on-device diagnostics log by a
-- random per-request id the client sends (X-Pollis-Request-Id).
--
-- Kept deliberately tight: an operator debugging aid, not an archive.
--
-- Nothing here can name a user. There is no user, device, email or IP column
-- for one to go in; `route` is the router's matched pattern (never the raw
-- path), and `error`/`detail` are DS-authored text that the DS scrubs
-- (pollis_api::redact::scrub) before the insert.
--
-- The invalid states are refused by the table itself, not by code:
--   * a non-error status          -> CHECK (status BETWEEN 400 AND 599)
--   * an id that could carry data -> CHECK on request_id's length + charset
--   * unbounded text              -> CHECK on every text column's length
--   * unbounded growth            -> the AFTER INSERT trigger below drops rows
--     older than 72 hours and everything beyond the newest 500, on every
--     insert. No sweep, no timer, and no code path that can skip it.
--
-- Additive and backward-compatible: CREATE TABLE/INDEX/TRIGGER only. A DS
-- running before this migration is applied fails its inserts, which it treats
-- as best-effort and ignores.
CREATE TABLE ds_error_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    at         INTEGER NOT NULL,
    request_id TEXT CHECK (
        request_id IS NULL
        OR (length(request_id) BETWEEN 1 AND 32 AND request_id NOT GLOB '*[^0-9A-Za-z]*')
    ),
    method     TEXT NOT NULL CHECK (method IN ('GET', 'POST', 'PUT', 'PATCH', 'DELETE')),
    route      TEXT NOT NULL CHECK (length(route) BETWEEN 1 AND 128 AND substr(route, 1, 1) = '/'),
    status     INTEGER NOT NULL CHECK (status BETWEEN 400 AND 599),
    error      TEXT CHECK (error IS NULL OR length(error) <= 200),
    detail     TEXT CHECK (detail IS NULL OR length(detail) <= 1000)
);
CREATE INDEX idx_ds_error_log_at ON ds_error_log(at);

CREATE TRIGGER ds_error_log_bound AFTER INSERT ON ds_error_log
BEGIN
    DELETE FROM ds_error_log
     WHERE at < NEW.at - 259200
        OR id <= NEW.id - 500;
END;
