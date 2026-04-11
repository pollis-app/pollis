-- Account identity, device cross-signing, and new-device enrollment.
--
-- Introduces a long-lived per-user account identity key (account_id_key),
-- a device-cross-signing column on user_device, a wrapped-recovery blob
-- table, a GroupInfo table for MLS external-commit joins, a device
-- enrollment request table for the inbox-approval flow, and a security
-- event log.
--
-- See MULTI_DEVICE_ENROLLMENT.md at the repo root for the full design.
--
-- NOTE: existing user-scoped data (users, devices, groups, messages, …)
-- is incompatible with the new identity/cross-signing model and must be
-- truncated manually ONCE before this migration runs for the first
-- time. The truncation SQL lives outside this file so re-running all
-- migrations from a clean rebuild does not wipe live data.
-- Run against Turso manually.


-- ── Step 1: account identity on users ─────────────────────────────────
-- account_id_pub: Ed25519 public key, published once per user; rotated
-- only on soft-recovery (reset_identity).
-- identity_version: increments on every reset so old devices can detect
-- they've been orphaned and self-wipe.

ALTER TABLE users ADD COLUMN account_id_pub BLOB;
ALTER TABLE users ADD COLUMN identity_version INTEGER NOT NULL DEFAULT 1;


-- ── Step 2: device cross-signing on user_device ───────────────────────
-- Every device publishes a signature by account_id_key binding its
-- MLS signing public key. Every client verifies this cert before
-- accepting an Add or external-join commit.
--
-- device_cert = Ed25519_sign(
--     account_id_key.private,
--     device_id || mls_signature_pub || issued_at || identity_version
-- )

ALTER TABLE user_device ADD COLUMN device_cert BLOB;
ALTER TABLE user_device ADD COLUMN cert_issued_at TEXT;
ALTER TABLE user_device ADD COLUMN cert_identity_version INTEGER;
ALTER TABLE user_device ADD COLUMN mls_signature_pub BLOB;


-- ── Step 3: wrapped account identity private key ──────────────────────
-- One row per user. Stores account_id_key.private encrypted under a
-- wrap key derived from the user's Secret Key via HKDF-SHA256. The
-- Secret Key itself is NEVER stored server-side. Overwritten on
-- rotate_secret_key and reset_identity.

CREATE TABLE account_recovery (
    user_id          TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    identity_version INTEGER NOT NULL,
    salt             BLOB NOT NULL,
    nonce            BLOB NOT NULL,
    wrapped_key      BLOB NOT NULL,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now'))
);


-- ── Step 4: GroupInfo blobs for external-commit joining ──────────────
-- Required for the Secret Key recovery path: a new device uses these
-- to construct an MLS external commit that joins the group without a
-- Welcome. Updated by any member after every epoch change.

CREATE TABLE mls_group_info (
    conversation_id      TEXT PRIMARY KEY,
    epoch                INTEGER NOT NULL,
    group_info           BLOB NOT NULL,
    updated_at           TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by_device_id TEXT NOT NULL
);


-- ── Step 5: device enrollment requests ───────────────────────────────
-- The new device posts a row here and publishes a livekit inbox event.
-- An existing device of the same user approves or rejects. On approval
-- the existing device HPKE-encrypts account_id_key.private to the
-- requester's ephemeral pub and writes it to wrapped_account_key.
-- TTL is 10 minutes (enforced by expires_at; clients should ignore
-- expired rows and a background sweep can flip status to 'expired').

CREATE TABLE device_enrollment_request (
    id                       TEXT PRIMARY KEY,
    user_id                  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    new_device_id            TEXT NOT NULL,
    new_device_ephemeral_pub BLOB NOT NULL,
    verification_code        TEXT NOT NULL,
    wrapped_account_key      BLOB,
    status                   TEXT NOT NULL
        CHECK (status IN ('pending', 'approved', 'rejected', 'expired')),
    created_at               TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at               TEXT NOT NULL,
    approved_by_device_id    TEXT
);

CREATE INDEX idx_enrollment_user_pending
    ON device_enrollment_request(user_id, status)
    WHERE status = 'pending';


-- ── Step 6: security event log ───────────────────────────────────────
-- Breadcrumbs for a future Security settings page. Written on every
-- enrollment, rejection, identity reset, and secret key rotation.
-- Never surfaced as an interruption; users opt in to viewing.

CREATE TABLE security_event (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    device_id  TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    metadata   TEXT
);

CREATE INDEX idx_security_event_user
    ON security_event(user_id, created_at DESC);


-- ── Done ─────────────────────────────────────────────────────────────
INSERT INTO schema_migrations (version, description) VALUES
    (13, 'account identity, device cross-signing, enrollment, group info, security log');
