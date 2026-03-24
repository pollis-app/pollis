-- Create migration tracking table
CREATE TABLE schema_migrations (
    version     INTEGER PRIMARY KEY,
    description TEXT NOT NULL,
    applied_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Backfill any NULL usernames before adding the NOT NULL constraint.
-- Uses the email prefix + last 6 chars of the ULID to guarantee uniqueness.
UPDATE users
SET username = LOWER(SUBSTR(email, 1, INSTR(email, '@') - 1))
            || '_'
            || LOWER(SUBSTR(id, -6))
WHERE username IS NULL;

-- Recreate users with NOT NULL UNIQUE on username.
-- SQLite doesn't support ALTER COLUMN, so we copy-rename.
PRAGMA foreign_keys = OFF;

CREATE TABLE users_new (
    id           TEXT PRIMARY KEY,
    email        TEXT NOT NULL UNIQUE,
    username     TEXT NOT NULL UNIQUE,
    phone        TEXT,
    identity_key TEXT,
    avatar_url   TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO users_new SELECT * FROM users;

DROP TABLE users;

ALTER TABLE users_new RENAME TO users;

PRAGMA foreign_keys = ON;

CREATE INDEX idx_users_username ON users(username);

-- Record migrations
INSERT INTO schema_migrations (version, description) VALUES
    (1, 'username NOT NULL UNIQUE, index on users.username, schema_migrations table');
