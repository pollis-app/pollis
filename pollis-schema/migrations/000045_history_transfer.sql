-- #1735: copy message history between two of one account's enrolled devices.
--
-- The DS is a relay for sealed chunks it cannot read: the receiving device (dst)
-- files a signed request, the source device (src) answers with a signed
-- acceptance carrying the key-encapsulation ciphertext, then uploads chunks
-- sealed under a key only the two devices derive. Protocol and threat model:
-- `docs/history-transfer-design.md`.
--
-- Invariants held HERE, by the database, not by the handlers:
--   * src and dst differ (CHECK);
--   * one OPEN transfer per receiving device (partial UNIQUE index);
--   * an acceptance exists exactly when the transfer is accepted or later, and
--     a requested transfer carries none (CHECK);
--   * a chunk is stored only for an ACCEPTED transfer, only inside the
--     8-chunk window above the receiver's ack, never below it, and only
--     contiguously (BEFORE INSERT trigger);
--   * a stored chunk's bytes never change (BEFORE UPDATE trigger), so an
--     acknowledged index can never be rewritten under the receiver;
--   * sealed chunks are 1 byte .. 1 MiB (CHECK).
--
-- Times are unix seconds (INTEGER), so expiry tests inject `now`.
--
-- Additive and backward-compatible: three new tables, nothing older reads them.

CREATE TABLE IF NOT EXISTS history_transfer (
    transfer_id   TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL,
    src_device_id TEXT NOT NULL,
    dst_device_id TEXT NOT NULL,
    via           TEXT NOT NULL CHECK (via IN ('link', 'sibling')),
    state         TEXT NOT NULL CHECK (state IN ('requested', 'accepted', 'completed', 'declined', 'cancelled')),
    req           BLOB NOT NULL CHECK (length(req) BETWEEN 1 AND 262144),
    req_sig       BLOB NOT NULL CHECK (length(req_sig) BETWEEN 1 AND 8192),
    acc           BLOB CHECK (acc IS NULL OR length(acc) BETWEEN 1 AND 65536),
    acc_sig       BLOB CHECK (acc_sig IS NULL OR length(acc_sig) BETWEEN 1 AND 8192),
    -- Highest chunk index the receiver has imported and acknowledged; -1 = none.
    acked_through INTEGER NOT NULL DEFAULT -1 CHECK (acked_through >= -1),
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    CHECK (src_device_id != dst_device_id),
    CHECK ((acc IS NULL) = (acc_sig IS NULL)),
    CHECK (state != 'requested' OR acc IS NULL),
    CHECK (state NOT IN ('accepted', 'completed') OR acc IS NOT NULL)
);

-- One open transfer per receiving device; a new request replaces the old one.
CREATE UNIQUE INDEX IF NOT EXISTS idx_history_transfer_open_dst
    ON history_transfer(dst_device_id) WHERE state IN ('requested', 'accepted');
CREATE INDEX IF NOT EXISTS idx_history_transfer_src ON history_transfer(src_device_id, state);
CREATE INDEX IF NOT EXISTS idx_history_transfer_user ON history_transfer(user_id);
CREATE INDEX IF NOT EXISTS idx_history_transfer_expires ON history_transfer(expires_at);

CREATE TABLE IF NOT EXISTS history_transfer_chunk (
    transfer_id TEXT NOT NULL,
    idx         INTEGER NOT NULL CHECK (idx >= 0),
    sealed      BLOB NOT NULL CHECK (length(sealed) BETWEEN 1 AND 1048576),
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (transfer_id, idx)
);

-- The window and contiguity, enforced by the database: a chunk lands only for
-- an accepted transfer, strictly above the receiver's ack, at most 8 above it,
-- and either right after the ack or right after a chunk that is stored.
CREATE TRIGGER IF NOT EXISTS history_transfer_chunk_window
BEFORE INSERT ON history_transfer_chunk
BEGIN
    SELECT RAISE(ABORT, 'history_transfer_chunk: transfer not accepted')
    WHERE NOT EXISTS (
        SELECT 1 FROM history_transfer
        WHERE transfer_id = NEW.transfer_id AND state = 'accepted'
    );
    SELECT RAISE(ABORT, 'history_transfer_chunk: index outside the window')
    WHERE NEW.idx <= (SELECT acked_through FROM history_transfer WHERE transfer_id = NEW.transfer_id)
       OR NEW.idx > (SELECT acked_through FROM history_transfer WHERE transfer_id = NEW.transfer_id) + 8;
    SELECT RAISE(ABORT, 'history_transfer_chunk: index not contiguous')
    WHERE NEW.idx != (SELECT acked_through FROM history_transfer WHERE transfer_id = NEW.transfer_id) + 1
      AND NOT EXISTS (
          SELECT 1 FROM history_transfer_chunk
          WHERE transfer_id = NEW.transfer_id AND idx = NEW.idx - 1
      );
END;

-- A stored chunk is immutable: the receiver may already have fetched it.
CREATE TRIGGER IF NOT EXISTS history_transfer_chunk_immutable
BEFORE UPDATE ON history_transfer_chunk
BEGIN
    SELECT RAISE(ABORT, 'history_transfer_chunk: stored chunks are immutable');
END;

-- The receiver's ack only moves forward.
CREATE TRIGGER IF NOT EXISTS history_transfer_ack_monotone
BEFORE UPDATE OF acked_through ON history_transfer
WHEN NEW.acked_through < OLD.acked_through
BEGIN
    SELECT RAISE(ABORT, 'history_transfer: acked_through only moves forward');
END;

-- Durable per-account request budget (the per-IP limiter is not per account).
CREATE TABLE IF NOT EXISTS history_transfer_budget (
    user_id      TEXT PRIMARY KEY,
    window_start INTEGER NOT NULL,
    used         INTEGER NOT NULL CHECK (used >= 0)
);
