-- #847 — shareable group invite links / join codes.
--
-- Today an invite cannot exist without naming a registered user: `group_invite`
-- carries an `invitee_id` FK to `users`, so `send_group_invite` must resolve a
-- username or email first. A shareable link has no such addressee, so it needs
-- its own table rather than a nullable `invitee_id` on `group_invite` — making
-- that column nullable would destroy the very invariant that makes the existing
-- table safe (an addressed invite that names nobody).
--
-- TOKEN STORAGE. The link token is `<selector>.<secret>`. Only the SELECTOR and
-- `sha256(secret)` are stored here; the secret half never touches the server at
-- creation time (the client generates the token, hashes it locally, and posts
-- the hash). A dump of this table therefore yields NO working invite: recovering
-- a token means inverting SHA-256 over a 256-bit random secret.
--
-- The split into selector + secret is deliberate. The selector is a public,
-- indexed lookup handle so redemption is an O(1) index probe; the secret is then
-- compared in constant time against `secret_hash`. Looking a row up directly by
-- the secret's hash would work too, but it makes the comparison the DATABASE's
-- job — and the DB's index probe is neither constant-time nor auditable from
-- Rust. Keeping the two halves separate is what lets the constant-time compare
-- be real rather than nominal.
--
-- INVALID STATES. Per CLAUDE.md the invariant is enforced at the lowest layer
-- that can hold it — a CHECK constraint, not code discipline:
--   * `uses <= max_uses` is enforced BY THE DATABASE, so a link cannot be
--     redeemed past its cap even if a caller races the application-level check.
--     This is the constraint that makes over-redemption unrepresentable rather
--     than merely unlikely.
--   * `max_uses > 0` — a zero-use link is a revoked link, and there is exactly
--     one way to express revocation (`revoked_at`), never two.
--   * `uses >= 0`.
--
-- NULL means "no bound" for both `expires_at` and `max_uses` — an unbounded link
-- is the common case (a team's standing #general link) and encoding it as NULL
-- keeps "unlimited" from being a magic sentinel like 0 or -1.
--
-- Additive and backward-compatible per CLAUDE.md: CREATE TABLE + CREATE INDEX
-- only. A shipped client that predates this feature never reads these tables.
CREATE TABLE group_invite_link (
    id          TEXT PRIMARY KEY,
    group_id    TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Public lookup handle. Not a secret, and useless on its own.
    selector    TEXT NOT NULL UNIQUE,
    -- Lowercase hex sha256 of the secret half. The secret is never stored.
    secret_hash TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    -- RFC3339. NULL = never expires.
    expires_at  TEXT,
    -- NULL = unlimited uses.
    max_uses    INTEGER,
    uses        INTEGER NOT NULL DEFAULT 0,
    -- RFC3339. Non-NULL = revoked; revocation is permanent and one-way.
    revoked_at  TEXT,

    CHECK (uses >= 0),
    CHECK (max_uses IS NULL OR max_uses > 0),
    CHECK (max_uses IS NULL OR uses <= max_uses)
);

-- Redemption probes by selector. UNIQUE above already indexes it; this index
-- serves the group's management list ("show me this group's live links").
CREATE INDEX idx_invite_link_group ON group_invite_link(group_id, revoked_at);

-- #847 — redemption audit trail.
--
-- Two jobs, both security rather than product:
--   1. It is the durable half of redemption rate limiting. The in-process
--      limiter in `pollis-delivery/src/ratelimit.rs` is per-instance memory and
--      resets on deploy; a brute-force bound that a rolling restart clears is
--      not a bound. Counting recent FAILED attempts per actor here survives
--      restarts and is shared across instances.
--   2. It makes "who joined via which link" answerable after the fact, which is
--      the question an admin actually asks when a link leaks.
--
-- `succeeded` is stored rather than inferred: a failed attempt has no link to
-- point at (the token matched nothing), so `link_id` is nullable and the
-- attempt is keyed on the actor instead.
CREATE TABLE group_invite_link_redemption (
    id          TEXT PRIMARY KEY,
    -- NULL when the presented token matched no live link.
    link_id     TEXT REFERENCES group_invite_link(id) ON DELETE SET NULL,
    -- The authenticated user who presented the token.
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    attempted_at TEXT NOT NULL DEFAULT (datetime('now')),
    succeeded   INTEGER NOT NULL DEFAULT 0 CHECK (succeeded IN (0, 1))
);

-- The rate-limit probe: recent failures for one actor, newest first.
CREATE INDEX idx_invite_redemption_actor
    ON group_invite_link_redemption(user_id, attempted_at DESC);

-- A user may hold at most one SUCCESSFUL redemption per link, so a replayed
-- link never double-counts `uses` for the same person.
CREATE UNIQUE INDEX idx_invite_redemption_once
    ON group_invite_link_redemption(link_id, user_id)
    WHERE succeeded = 1;
