-- #1182: bots. A bot is a user (`users.kind = 'bot'`) scoped to one group for
-- its whole life; its hosts are its devices (`user_device` rows under it).
--
-- WHAT THE DATABASE REFUSES, with no pollis-delivery code in the path:
--   * a `kind = 'bot'` user without its `bot` row, or a `bot` row whose user is
--     a human;
--   * a bot that is not a member of its own `bot.group_id`, a bot that is a
--     member of any other group, and a bot whose role is `admin`;
--   * a group owned (`groups.owner_id`) by a bot, and a group deleted while it
--     still has bots;
--   * a bot email other than exactly `bot-<user id, lowercased>@bot.invalid`,
--     and a human email at `bot.invalid` (or any subdomain of it);
--   * a bot in a DM, invited to a group, asking to join one, or holding a push
--     token;
--   * a trigger claim for an installation that is not a bot.
--
-- `.invalid` is reserved by RFC 2606/6761 and can never receive mail, so the
-- address satisfies `users.email NOT NULL UNIQUE` without being a second way
-- into the account. The DS emails nothing but the sign-in OTP and the email
-- change challenge, and refuses both for these addresses.
--
-- INSERT ORDER (SQLite has no deferred constraints, and triggers fire per row):
-- the `bot` row is the claim, so it comes FIRST and every other row is checked
-- against it. The DS creates a bot in one transaction, in this order:
--
--     bot  ->  group_member (role 'member')  ->  users (kind 'bot')  ->  user_device ...
--
-- so by the time a `kind = 'bot'` user exists, its `bot` row and its
-- membership already do, and the users trigger can require both.
--
-- DELETE ORDER is the reverse, users first: the `bot` row may not be deleted
-- while its user exists, and the bot's membership may not be deleted while its
-- `bot` row exists. Account deletion of a bot therefore runs
--
--     users  ->  bot  ->  group_member, user_device, ... (teardown.rs)
--
-- and no committed state ever has a bot user without its row or membership.
-- A group with bots cannot be deleted until its bots are, which makes group
-- teardown delete them first, through the same account-deletion path.
--
-- Validators, not claimers (as in 000017): no trigger writes a row, it only
-- refuses one. Production runs with foreign_keys=OFF; triggers fire regardless.
--
-- Additive and backward-compatible: one defaulted column with an already
-- satisfied CHECK (every existing row is 'human'), three new tables, triggers
-- that every existing row already satisfies. A shipped client never reads
-- `users.kind` and sees a bot as an ordinary member.

ALTER TABLE users ADD COLUMN kind TEXT NOT NULL DEFAULT 'human'
    CHECK (kind IN ('human', 'bot'));

-- One row per bot. `created_by` is an audit pointer to the admin who created
-- it, blanked (NULL) when that admin's account is deleted; the bot belongs to
-- the group, not to them.
CREATE TABLE IF NOT EXISTS bot (
    user_id     TEXT PRIMARY KEY CHECK (length(user_id) BETWEEN 1 AND 128),
    group_id    TEXT NOT NULL CHECK (length(group_id) BETWEEN 1 AND 128),
    created_by  TEXT CHECK (created_by IS NULL OR length(created_by) BETWEEN 1 AND 128),
    description TEXT CHECK (description IS NULL OR length(description) BETWEEN 1 AND 500),
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_bot_group ON bot(group_id);
CREATE INDEX IF NOT EXISTS idx_bot_created_by ON bot(created_by);

-- A host asking to become a bot's device: either the first host of a new bot
-- ('create', carrying the bot's account key generated on the host) or another
-- host of an existing bot ('add_host'). Filed unauthenticated and short-lived;
-- the admin finds it by the hash of the code the host printed, and the host
-- reads its outcome with the secret whose hash is `poll_verifier`.
--
-- For 'add_host' the admin's signed host approval is stored and relayed to the
-- bot's existing hosts; one of them wraps the account key to `ephemeral_pub`.
-- The DS verifies neither: the hosts verify the approval, the new host the wrap.
CREATE TABLE IF NOT EXISTS bot_pairing_request (
    id                   TEXT PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    kind                 TEXT NOT NULL CHECK (kind IN ('create', 'add_host')),
    code_lookup          BLOB NOT NULL CHECK (length(code_lookup) = 32),
    poll_verifier        BLOB NOT NULL CHECK (length(poll_verifier) = 32),
    ephemeral_pub        BLOB NOT NULL CHECK (length(ephemeral_pub) = 32),
    device_id            TEXT NOT NULL CHECK (length(device_id) BETWEEN 1 AND 64),
    device_name          TEXT CHECK (device_name IS NULL OR length(device_name) BETWEEN 1 AND 64),
    -- 'create' only: the bot's ML-DSA-44 account public key.
    account_id_pub       BLOB CHECK (account_id_pub IS NULL OR length(account_id_pub) = 1312),
    -- The bot and its group: 'add_host' from the start, 'create' once approved.
    user_id              TEXT,
    group_id             TEXT,
    status               TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved')),
    -- The approving admin ('create': who created the bot; 'add_host': who
    -- signed the host approval) and the device they approved from.
    reviewed_by          TEXT,
    approver_device_id   TEXT,
    -- 'add_host' only: the admin's signed host approval, and the wrap.
    host_approval        BLOB CHECK (host_approval IS NULL OR length(host_approval) BETWEEN 1 AND 16384),
    wrapped_account_key  BLOB CHECK (wrapped_account_key IS NULL OR length(wrapped_account_key) BETWEEN 1 AND 8192),
    wrapped_by_device_id TEXT,
    created_at           TEXT NOT NULL DEFAULT (datetime('now')),
    -- Unix seconds.
    expires_at           INTEGER NOT NULL,
    CHECK (kind != 'create' OR (account_id_pub IS NOT NULL
                                AND host_approval IS NULL
                                AND wrapped_account_key IS NULL)),
    CHECK (kind != 'add_host' OR (account_id_pub IS NULL
                                  AND user_id IS NOT NULL
                                  AND group_id IS NOT NULL)),
    CHECK (status != 'pending' OR wrapped_account_key IS NULL),
    CHECK (status != 'approved' OR (user_id IS NOT NULL
                                    AND group_id IS NOT NULL
                                    AND reviewed_by IS NOT NULL
                                    AND approver_device_id IS NOT NULL)),
    CHECK (kind != 'add_host' OR status != 'approved'
           OR (host_approval IS NOT NULL
               AND wrapped_account_key IS NOT NULL
               AND wrapped_by_device_id IS NOT NULL)),
    CHECK (host_approval IS NULL OR (reviewed_by IS NOT NULL AND approver_device_id IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_bot_pairing_code ON bot_pairing_request(code_lookup);
CREATE INDEX IF NOT EXISTS idx_bot_pairing_user ON bot_pairing_request(user_id);
CREATE INDEX IF NOT EXISTS idx_bot_pairing_expiry ON bot_pairing_request(expires_at);

-- One claim per (installation, trigger): when several hosts of a bot see the
-- same trigger, the first claim wins and the others discard their actions.
-- `installation_id` is the bot's user id while a bot has one installation.
-- Pruned by the retention sweep after `bots::TRIGGER_CLAIM_RETENTION_SECS`.
CREATE TABLE IF NOT EXISTS bot_trigger_claim (
    installation_id TEXT NOT NULL CHECK (length(installation_id) BETWEEN 1 AND 128),
    trigger_key     TEXT NOT NULL CHECK (length(trigger_key) BETWEEN 1 AND 256),
    device_id       TEXT NOT NULL CHECK (length(device_id) BETWEEN 1 AND 64),
    -- Unix seconds.
    claimed_at      INTEGER NOT NULL,
    PRIMARY KEY (installation_id, trigger_key)
);
CREATE INDEX IF NOT EXISTS idx_bot_trigger_claim_age ON bot_trigger_claim(claimed_at);

-- ---------------------------------------------------------------- bot rows

-- A bot row is created before its user, never for an existing account (no
-- human becomes a bot), for a real group, and never for an id that already
-- sits in another group or owns one.
CREATE TRIGGER IF NOT EXISTS bot_insert_guard
BEFORE INSERT ON bot
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM users WHERE id = NEW.user_id)
  OR NOT EXISTS (SELECT 1 FROM groups WHERE id = NEW.group_id)
  OR EXISTS (SELECT 1 FROM group_member
             WHERE user_id = NEW.user_id
               AND (group_id != NEW.group_id OR role = 'admin'))
  OR EXISTS (SELECT 1 FROM groups WHERE owner_id = NEW.user_id)
BEGIN
    SELECT RAISE(ABORT, 'bot row must precede its user and name a real group');
END;

CREATE TRIGGER IF NOT EXISTS bot_update_guard
BEFORE UPDATE OF user_id, group_id ON bot
FOR EACH ROW
WHEN NEW.user_id != OLD.user_id OR NEW.group_id != OLD.group_id
BEGIN
    SELECT RAISE(ABORT, 'a bot is scoped to one group for its whole life');
END;

CREATE TRIGGER IF NOT EXISTS bot_delete_guard
BEFORE DELETE ON bot
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM users WHERE id = OLD.user_id)
BEGIN
    SELECT RAISE(ABORT, 'delete the bot user before its bot row');
END;

-- ---------------------------------------------------------------- users

CREATE TRIGGER IF NOT EXISTS users_bot_insert_guard
BEFORE INSERT ON users
FOR EACH ROW
WHEN NEW.kind = 'bot' AND (
       NOT EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.id)
    OR NOT EXISTS (SELECT 1 FROM group_member gm JOIN bot b ON b.user_id = gm.user_id
                   WHERE gm.user_id = NEW.id AND gm.group_id = b.group_id
                     AND gm.role != 'admin')
    OR NEW.email != lower('bot-' || NEW.id || '@bot.invalid'))
BEGIN
    SELECT RAISE(ABORT, 'a bot user needs its bot row, its membership and its derived email');
END;

CREATE TRIGGER IF NOT EXISTS users_human_insert_guard
BEFORE INSERT ON users
FOR EACH ROW
WHEN NEW.kind = 'human' AND (
       EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.id)
    OR lower(trim(NEW.email)) LIKE '%@bot.invalid'
    OR lower(trim(NEW.email)) LIKE '%.bot.invalid')
BEGIN
    SELECT RAISE(ABORT, 'a human account may not be a bot or use a bot.invalid address');
END;

CREATE TRIGGER IF NOT EXISTS users_kind_immutable
BEFORE UPDATE OF kind ON users
FOR EACH ROW
WHEN NEW.kind != OLD.kind
BEGIN
    SELECT RAISE(ABORT, 'users.kind is immutable');
END;

CREATE TRIGGER IF NOT EXISTS users_bot_identity_guard
BEFORE UPDATE OF id, email ON users
FOR EACH ROW
WHEN OLD.kind = 'bot'
 AND (NEW.id != OLD.id OR NEW.email != lower('bot-' || NEW.id || '@bot.invalid'))
BEGIN
    SELECT RAISE(ABORT, 'a bot keeps its id and its derived email');
END;

CREATE TRIGGER IF NOT EXISTS users_human_email_guard
BEFORE UPDATE OF email ON users
FOR EACH ROW
WHEN OLD.kind = 'human'
 AND (lower(trim(NEW.email)) LIKE '%@bot.invalid' OR lower(trim(NEW.email)) LIKE '%.bot.invalid')
BEGIN
    SELECT RAISE(ABORT, 'a human account may not use a bot.invalid address');
END;

-- ---------------------------------------------------------------- group_member

CREATE TRIGGER IF NOT EXISTS group_member_bot_insert_guard
BEFORE INSERT ON group_member
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.user_id
             AND (group_id != NEW.group_id OR NEW.role = 'admin'))
BEGIN
    SELECT RAISE(ABORT, 'a bot is a non-admin member of its own group only');
END;

CREATE TRIGGER IF NOT EXISTS group_member_bot_update_guard
BEFORE UPDATE OF group_id, user_id, role ON group_member
FOR EACH ROW
WHEN (EXISTS (SELECT 1 FROM bot WHERE user_id = OLD.user_id)
      AND (NEW.user_id != OLD.user_id OR NEW.group_id != OLD.group_id))
  OR EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.user_id
             AND (group_id != NEW.group_id OR NEW.role = 'admin'))
BEGIN
    SELECT RAISE(ABORT, 'a bot is a non-admin member of its own group only');
END;

CREATE TRIGGER IF NOT EXISTS group_member_bot_delete_guard
BEFORE DELETE ON group_member
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE user_id = OLD.user_id AND group_id = OLD.group_id)
BEGIN
    SELECT RAISE(ABORT, 'a bot leaves its group only by being deleted');
END;

-- ---------------------------------------------------------------- groups

CREATE TRIGGER IF NOT EXISTS groups_bot_owner_insert_guard
BEFORE INSERT ON groups
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.owner_id)
BEGIN
    SELECT RAISE(ABORT, 'a bot cannot own a group');
END;

CREATE TRIGGER IF NOT EXISTS groups_bot_owner_update_guard
BEFORE UPDATE OF owner_id ON groups
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.owner_id)
BEGIN
    SELECT RAISE(ABORT, 'a bot cannot own a group');
END;

CREATE TRIGGER IF NOT EXISTS groups_bot_delete_guard
BEFORE DELETE ON groups
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE group_id = OLD.id)
BEGIN
    SELECT RAISE(ABORT, 'delete a group''s bots before the group');
END;

-- ---------------------------------------------------------------- the rest of the social graph

CREATE TRIGGER IF NOT EXISTS dm_channel_member_bot_insert_guard
BEFORE INSERT ON dm_channel_member
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.user_id)
BEGIN
    SELECT RAISE(ABORT, 'a bot cannot be in a DM');
END;

CREATE TRIGGER IF NOT EXISTS dm_channel_member_bot_update_guard
BEFORE UPDATE OF user_id ON dm_channel_member
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.user_id)
BEGIN
    SELECT RAISE(ABORT, 'a bot cannot be in a DM');
END;

CREATE TRIGGER IF NOT EXISTS group_invite_bot_guard
BEFORE INSERT ON group_invite
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE user_id IN (NEW.invitee_id, NEW.inviter_id))
BEGIN
    SELECT RAISE(ABORT, 'a bot cannot invite or be invited');
END;

CREATE TRIGGER IF NOT EXISTS group_join_request_bot_guard
BEFORE INSERT ON group_join_request
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.requester_id)
BEGIN
    SELECT RAISE(ABORT, 'a bot cannot ask to join a group');
END;

CREATE TRIGGER IF NOT EXISTS push_token_bot_insert_guard
BEFORE INSERT ON push_token
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.user_id)
BEGIN
    SELECT RAISE(ABORT, 'a bot has no push token');
END;

CREATE TRIGGER IF NOT EXISTS push_token_bot_update_guard
BEFORE UPDATE OF user_id ON push_token
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.user_id)
BEGIN
    SELECT RAISE(ABORT, 'a bot has no push token');
END;

CREATE TRIGGER IF NOT EXISTS bot_trigger_claim_guard
BEFORE INSERT ON bot_trigger_claim
FOR EACH ROW
WHEN NOT EXISTS (SELECT 1 FROM bot WHERE user_id = NEW.installation_id)
BEGIN
    SELECT RAISE(ABORT, 'trigger claims belong to bot installations');
END;
