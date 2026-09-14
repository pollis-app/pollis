-- #1088: one mailbox, one account — case-insensitively.
--
-- `users.email` is `NOT NULL UNIQUE`, so the address string IS the account
-- identity. The UNIQUE is byte-exact, while the OTP store keys on
-- `trim(lower(email))`: `Alice@x.com` and `alice@x.com` were one mailbox to the
-- code that mails the OTP and two accounts to the table that stores them. Sign
-- in with a different capitalisation and you got a second, empty account —
-- while your real one, its groups and its devices stayed where they were.
--
-- The DS now canonicalizes to `trim(lower(..))` at every read and write of
-- `users.email` (`otp::apply_verify_otp` holds the INSERT, `email_change`
-- holds the UPDATE, `directory::user_by_identifier` holds the lookup). This
-- migration makes the database agree, in two steps.

-- 1. Backfill. Only rows that cannot collide: if some other row already holds
--    the lowercase spelling, leave both alone — that is the genuine
--    two-accounts-one-mailbox case, and merging accounts is not something a
--    migration may decide.
UPDATE users
   SET email = lower(trim(email))
 WHERE email <> lower(trim(email))
   AND NOT EXISTS (
       SELECT 1 FROM users other
        WHERE other.id <> users.id
          AND other.email = lower(trim(users.email))
   );

-- 2. Enforce it. After step 1 the only rows that can still differ by case are
--    those genuine duplicate pairs, so this index is already satisfied unless
--    such a pair exists — in which case it fails and aborts the release, which
--    is the correct outcome: two accounts sharing a mailbox needs a human to
--    decide which one survives, not a silent merge. Confirm ahead of a release
--    with:
--
--      SELECT COUNT(*) - COUNT(DISTINCT lower(trim(email))) FROM users;  -- want 0
--
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email_lower
    ON users (lower(trim(email)));
