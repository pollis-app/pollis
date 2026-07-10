# Database

Two databases. Remote schema starts from `000000_baseline.sql` (a full canonical dump) plus additive migrations (`000NNN_*.sql`). Local schema is in `local_schema.sql`.

## How schema changes ship

1. Write a new migration file: `pollis-core/src/db/migrations/000NNN_description.sql`. Version number must be the next integer.
2. Run it by hand against your dev Turso DB to test.
3. Merge to main. When a release tag is pushed, `.github/workflows/desktop-release.yml` runs `scripts/db-apply.sh` against **production** after all builds succeed and before the release job uploads artifacts. A migration failure aborts the release.

The bash runner (pure `curl` + `jq`, no Node/toolchain install) reads `schema_migrations` from the target DB, diffs against the files on disk, and applies pending ones atomically (libsql batch with step conditions — any failure rolls the whole batch back and records nothing).

Nobody ever applies migrations to prod by hand. Prod is CI-only.

## Migrations must be additive and backward-compatible

**This is the hard rule. Every migration must be safe for the currently-shipped version of the desktop app to run against.**

Why: desktop users update on their own schedule. After a release ships, there will be a mix of old-and-new app versions hitting prod for days or weeks. The schema must work for both.

Safe (additive):
- `CREATE TABLE` (new table)
- `ALTER TABLE … ADD COLUMN` — column must be nullable or have a DEFAULT so old INSERTs without the column still succeed
- `CREATE INDEX`
- New CHECK constraints that every existing row already satisfies

Unsafe (requires a multi-release dance — don't do these casually):
- `DROP TABLE`, `DROP COLUMN`, `ALTER … RENAME`
- Changing a nullable column to `NOT NULL`
- Tightening a CHECK constraint
- Anything that makes the old app's SQL fail

If you genuinely need to remove something, the pattern is: (1) ship an app version that no longer reads/writes the doomed thing, (2) wait long enough that nearly all users have updated, (3) *then* drop it in a later migration. Stage over multiple releases.

## Remote Database (Turso)

Source: `pollis-core/src/db/migrations/000000_baseline.sql` + numbered migrations `000001`+.

### users
- `id` TEXT PK
- `email` TEXT NOT NULL UNIQUE
- `username` TEXT
- `phone` TEXT
- `identity_key` TEXT _(legacy, unused)_
- `avatar_url` TEXT
- `created_at` TEXT NOT NULL DEFAULT now
- `account_id_pub` BLOB _(Ed25519 pub key, added migration 13)_
- `identity_version` INTEGER NOT NULL DEFAULT 1 _(increments on reset, migration 13)_

### groups
- `id` TEXT PK
- `name` TEXT NOT NULL
- `description` TEXT
- `icon_url` TEXT
- `owner_id` TEXT NOT NULL
- `created_at` TEXT NOT NULL DEFAULT now

### group_member
- PK: (`group_id`, `user_id`)
- `group_id` TEXT NOT NULL FK groups
- `user_id` TEXT NOT NULL FK users
- `role` TEXT NOT NULL DEFAULT 'member'
- `joined_at` TEXT NOT NULL DEFAULT now

### channels
- `id` TEXT PK
- `group_id` TEXT NOT NULL FK groups
- `name` TEXT NOT NULL
- `description` TEXT
- `channel_type` TEXT NOT NULL DEFAULT 'text' _(text or voice)_
- `created_at` TEXT NOT NULL DEFAULT now

### message_envelope
- `id` TEXT PK
- `conversation_id` TEXT NOT NULL _(channel ID or DM channel ID)_
- `sender_id` TEXT NOT NULL _(server-writable; NOT trusted for attribution — see below)_
- `ciphertext` TEXT NOT NULL _(MLS-encrypted, hex-prefixed with `mls:`)_
- `reply_to_id` TEXT
- `sent_at` TEXT NOT NULL
- `delivered` INTEGER NOT NULL DEFAULT 0
- `sealed` INTEGER NOT NULL DEFAULT 0 _(migration 000008; sealed sender, #331)_

**Sealed sender (#331).** Attribution is taken from the MLS credential inside the
ciphertext, never from `sender_id` — the ingest reader ([mls.md](./mls.md#sealed-sender-331))
decrypts and reads the credential's `{user_id}:{device_id}`, so a server-written
`sender_id` cannot forge or reveal authorship. This is **always on**. `sealed = 1`
additionally marks an envelope whose `sender_id` column is a non-identifying
sentinel (the string `"sealed"`) rather than the real sender — envelope-sender
*blinding*, so a Turso breach/subpoena of the stored table reveals nothing about
who sent which message. Blinding is gated behind `POLLIS_SEAL_SENDER` (default
**OFF**, dormant); the column and both code paths ship now so flipping it on later
is a config change, not a migration. `sender_id` stays `NOT NULL` and the sentinel
is a valid value, so the previously-shipped app keeps working (see migration
000008's backward-compat note; version 000007 is deliberately skipped).

Honest scope: this is an **at-rest** defense only. The DS still authenticates
every write with an `X-Pollis-User` header and gates on membership, so a *live* DS
operator still sees the sender in real time. Closing that axis is v1.5
anonymous-membership (not shipped — tracked in #489).

### dm_channel
- `id` TEXT PK
- `created_by` TEXT NOT NULL
- `created_at` TEXT NOT NULL DEFAULT now

### dm_channel_member
- PK: (`dm_channel_id`, `user_id`)
- `dm_channel_id` TEXT NOT NULL FK dm_channel
- `user_id` TEXT NOT NULL FK users
- `added_by` TEXT NOT NULL
- `added_at` TEXT NOT NULL DEFAULT now
- `accepted_at` TEXT _(migration 15, NULL = pending request for this member)_

### user_block _(migration 15)_
- PK: (`blocker_id`, `blocked_id`)
- `blocker_id` TEXT NOT NULL FK users
- `blocked_id` TEXT NOT NULL FK users
- `created_at` TEXT NOT NULL DEFAULT now
- Directional — A blocking B does not imply B blocks A. Enforcement checks both directions, so once either side blocks, neither can DM or group-invite the other.

### group_invite
- `id` TEXT PK
- `group_id` TEXT NOT NULL FK groups
- `inviter_id` TEXT NOT NULL FK users
- `invitee_id` TEXT NOT NULL FK users
- `created_at` TEXT NOT NULL DEFAULT now
- **No `status` column.** All rows are implicitly pending. Deleted on accept or decline.

### group_join_request
- `id` TEXT PK
- `group_id` TEXT NOT NULL FK groups
- `requester_id` TEXT NOT NULL FK users
- `created_at` TEXT NOT NULL DEFAULT now
- `reviewed_by` TEXT FK users
- `reviewed_at` TEXT
- `status` TEXT NOT NULL DEFAULT 'pending' CHECK (pending, approved, rejected)
- UNIQUE: (`group_id`, `requester_id`)

### user_preferences
- `user_id` TEXT PK FK users
- `preferences` TEXT NOT NULL DEFAULT '{}'
- `updated_at` TEXT NOT NULL DEFAULT now

### message_reaction
- `id` TEXT PK
- `message_id` TEXT NOT NULL
- `user_id` TEXT NOT NULL FK users
- `emoji` TEXT NOT NULL
- `created_at` TEXT NOT NULL DEFAULT now
- UNIQUE: (`message_id`, `user_id`, `emoji`)

### attachment_object
- `content_hash` TEXT PK _(SHA-256 of plaintext)_
- `r2_key` TEXT NOT NULL
- `mime_type` TEXT NOT NULL
- `size_bytes` INTEGER NOT NULL
- `created_at` TEXT NOT NULL DEFAULT now

### conversation_watermark _(migration 5, re-keyed in migration 16)_
- PK: (`conversation_id`, `user_id`, `device_id`)
- `conversation_id` TEXT NOT NULL
- `user_id` TEXT NOT NULL
- `device_id` TEXT NOT NULL
- `last_fetched_at` TEXT NOT NULL

Used by the envelope cleanup sweep in `get_channel_messages` and `get_dm_messages` to decide when it is safe to drop a row from `message_envelope`. A row is deleted when EITHER it is older than 30 days OR every registered device of every current member has watermarked past `sent_at` (the `OR` is deliberate — one slow device must not pin storage forever; the TTL is the hard ceiling).

Seed paths (so a new device or a pre-join user doesn't block cleanup retroactively):
- `add_member_to_group` seeds one row per (channel, device) for the joining user at join time.
- `create_dm_channel` / `add_user_to_dm_channel` seed per (member, device).
- `register_device` seeds per conversation the user is already a member of, for the newly-registered device.

### voice_presence _(removed in migration 18)_
Dropped. LiveKit's `RoomService.ListParticipants` / `ListRooms` is the source
of truth for who is currently in a voice channel. The shadow table drifted
on every crash/force-kill/network blip; querying LiveKit directly closed
that class of bug.

### user_device _(migration 11 + 13)_
- `device_id` TEXT PK
- `user_id` TEXT NOT NULL FK users
- `device_name` TEXT
- `created_at` TEXT NOT NULL DEFAULT now
- `last_seen` TEXT NOT NULL DEFAULT now
- `device_cert` BLOB _(migration 13)_
- `cert_issued_at` TEXT _(migration 13)_
- `cert_identity_version` INTEGER _(migration 13)_
- `mls_signature_pub` BLOB _(migration 13)_

### mls_key_package _(migration 3 + 11)_
- `ref_hash` TEXT PK _(KeyPackageRef hash, hex)_
- `user_id` TEXT NOT NULL FK users
- `key_package` BLOB NOT NULL _(TLS-serialized KeyPackage)_
- `claimed` INTEGER NOT NULL DEFAULT 0
- `created_at` TEXT NOT NULL DEFAULT now
- `device_id` TEXT _(migration 11)_

### mls_commit_log _(migration 3 + 14)_
- `seq` INTEGER PK AUTOINCREMENT
- `conversation_id` TEXT NOT NULL
- `epoch` INTEGER NOT NULL _(epoch BEFORE this commit)_
- `sender_id` TEXT NOT NULL FK users
- `commit_data` BLOB NOT NULL _(TLS-serialized MLS Commit)_
- `created_at` TEXT NOT NULL DEFAULT now
- `added_user_id` TEXT _(migration 14, NULL if no adds)_
- `added_device_ids` TEXT _(migration 14, comma-separated)_

### mls_welcome _(migration 3 + 11; now on the commit-log DB)_
- `id` TEXT PK _(ULID)_
- `conversation_id` TEXT NOT NULL
- `recipient_id` TEXT NOT NULL FK users
- `welcome_data` BLOB NOT NULL _(TLS-serialized Welcome)_
- `delivered` INTEGER NOT NULL DEFAULT 0
- `created_at` TEXT NOT NULL DEFAULT now
- `recipient_device_id` TEXT _(migration 11)_
- UNIQUE INDEX `idx_mls_welcome_recipient` on `(conversation_id, recipient_id, recipient_device_id)` _(commit-log-DB migration 000002, #430 P2)_ — one live Welcome per recipient device. It is the conflict target the DS submit bundle's and `/v1/welcomes/resubmit`'s idempotent `ON CONFLICT … DO UPDATE` upserts key on, so a re-sent Welcome refreshes the blob and re-arms delivery (`delivered = 0`) instead of stacking a duplicate row. The migration collapses any pre-existing duplicates (keeping the newest per tuple) before adding the index.

`mls_welcome`, `mls_commit_log`, and `mls_group_info` live on the **separate
commit-log Turso DB** (`LOG_DB_URL`) post-#420, where the Delivery Service holds
the only read-write token and clients hold a read-only token. Their migrations are
numbered independently in `pollis-core/src/db/migrations-log/` and applied by the
desktop-release workflow's second `db-apply` step (`MIGRATIONS_DIR=…/migrations-log`).

### mls_group_info _(migration 13)_
- `conversation_id` TEXT PK
- `epoch` INTEGER NOT NULL
- `group_info` BLOB NOT NULL _(TLS-serialized MlsMessage containing GroupInfo)_
- `updated_at` TEXT NOT NULL DEFAULT now
- `updated_by_device_id` TEXT NOT NULL

### mls_commit_since _(commit-log-DB migration 000003, #539)_
Per-device commit-log catch-up high-water — the signal the **retention floor** is
computed from. On its catch-up a client reports the epoch it is caught up FROM
(its current local MLS epoch); the DS records it and prunes `mls_commit_log` below
the floor. Lives on the commit-log DB; the DS is the sole writer.
- `conversation_id` TEXT NOT NULL
- `user_id` TEXT NOT NULL _(FK users dropped — cross-DB, like the sibling log tables)_
- `device_id` TEXT NOT NULL
- `since_epoch` INTEGER NOT NULL _(the device's applied MLS epoch; it still needs commits `>= this`)_
- `updated_at` TEXT NOT NULL DEFAULT now
- PRIMARY KEY `(conversation_id, user_id, device_id)`; INDEX `idx_mls_commit_since_conv` on `(conversation_id)`

**Retention floor (I4, #539).** Without pruning, `mls_commit_log` grows with
membership-churn × time. The DS (sole writer) prunes commits below a floor,
event-driven on commit-append (`POST /v1/commits`) and on a device's catch-up
report (`GET /v1/commits/:id?since=&user_id=&device_id=`) — never on a timer.
Two tiers (`pollis_delivery::commit::prune_floor`, modelled in
`specs/tla/Delivery.tla` Spec B):
- **Tier 1 (zero loss):** floor = MIN applied epoch across all CURRENT member
  devices, minus a small slack. Everyone still needs commits `>= that`, so nothing
  anyone is waiting on is deleted — the spec's `NoLossForCurrentMember` (guarded by
  the SLOWEST member, never the fastest). Only applied when the WHOLE roster has
  reported; an unreported/legacy member pins the floor at 0. A revoked device is
  excluded (it can't rejoin — I5).
- **Tier 2 (hard cap):** floor = `head − PRUNE_MAX_BEHIND_HEAD`, bounding storage +
  catch-up even against a perpetually-offline device. A member pruned past its epoch
  reads an earliest-available epoch above its own, trips the client gap detector
  (`invariants::classify` → `GapRecover`), and external-joins at head — forfeiting
  only the pruned-gap messages (accepted loss #1). `may_rejoin` (I5) still blocks a
  removed/revoked device from that rejoin.

Distinct from `conversation_watermark` (main DB), which tracks message-envelope
FETCH progress, not applied MLS epoch — the two GC floors are computed
independently.

### account_recovery _(migration 13)_
- `user_id` TEXT PK FK users
- `identity_version` INTEGER NOT NULL
- `salt` BLOB NOT NULL
- `nonce` BLOB NOT NULL
- `wrapped_key` BLOB NOT NULL _(account_id_key.private encrypted under Secret Key)_
- `created_at` TEXT NOT NULL DEFAULT now
- `updated_at` TEXT NOT NULL DEFAULT now

### device_enrollment_request _(migration 13)_
- `id` TEXT PK
- `user_id` TEXT NOT NULL FK users
- `new_device_id` TEXT NOT NULL
- `new_device_ephemeral_pub` BLOB NOT NULL
- `verification_code` TEXT NOT NULL
- `wrapped_account_key` BLOB _(filled on approval)_
- `status` TEXT NOT NULL CHECK (pending, approved, rejected, expired)
- `created_at` TEXT NOT NULL DEFAULT now
- `expires_at` TEXT NOT NULL
- `approved_by_device_id` TEXT

### security_event _(migration 13)_
- `id` TEXT PK
- `user_id` TEXT NOT NULL FK users
- `kind` TEXT NOT NULL
- `device_id` TEXT
- `created_at` TEXT NOT NULL DEFAULT now
- `metadata` TEXT

### account_key_log _(migration 000005)_
Append-only history of account identity keys — the data source for the
account-key transparency tenant (#330). One row per key version per user, never
updated or deleted; mirrors `mls_commit_log`'s shape (an AUTOINCREMENT `seq` for
global ordering + a UNIQUE index enforcing the per-subject invariant).
- `seq` INTEGER PK AUTOINCREMENT _(stable global ordering, the log leaf order)_
- `user_id` TEXT NOT NULL
- `account_id_pub` BLOB NOT NULL _(Ed25519 pub authoritative at this version)_
- `identity_version` INTEGER NOT NULL
- `created_at` TEXT NOT NULL DEFAULT now
- UNIQUE INDEX `idx_account_key_log_user_version` on `(user_id, identity_version)` — one row per version per user; a duplicate INSERT conflicts rather than silently forking the history.
- Dual-written in lock-step with `users.account_id_pub` by `generate_account_identity` (v1 at signup) and `reset_identity` (+1 per rotation). Migration backfills the current key of every user that already has an `account_id_pub`.

### user_groups / user_dms _(migration 000009 — created, then unused)_
Empty, unread tables. Created by migration `000009` as the directory index for the per-conversation-DB split (#261 Phase 2). #261 was dropped (not-planned), and the maintenance + reads were reverted — but the migration is append-only history and the tables were already applied to prod/dev/test, so they remain **empty and unreferenced**. No code writes or reads them. Left in place; a future tightening migration can `DROP` them if desired.

---

## Local Database (SQLite, per-user, encrypted)

Source: `pollis-core/src/db/local_schema.sql`

File path: `pollis_{user_id}.db`, encrypted with a key from the OS keystore.

### kv
- `key` TEXT PK
- `value` TEXT NOT NULL

### identity_key
- `id` INTEGER PK CHECK (id = 1) _(single row)_
- `public_key` BLOB NOT NULL
- `created_at` TEXT NOT NULL DEFAULT now

### message
- `id` TEXT PK
- `conversation_id` TEXT NOT NULL
- `sender_id` TEXT NOT NULL
- `ciphertext` BLOB NOT NULL
- `content` TEXT _(decrypted plaintext, NULL if decryption failed)_
- `reply_to_id` TEXT
- `sent_at` TEXT NOT NULL
- `received_at` TEXT NOT NULL DEFAULT now
- `delivered` INTEGER NOT NULL DEFAULT 0
- `edited_at` TEXT
- `deleted_at` TEXT

The `message` table is bounded by a **device-local retention window** (#150). It
is not unbounded history — old rows are evicted to cap disk use on this device.
See [Local message retention](#local-message-retention) below.

### dm_conversation
- `id` TEXT PK
- `peer_user_id` TEXT NOT NULL UNIQUE
- `created_at` TEXT NOT NULL DEFAULT now

### preferences
- `preferences` TEXT NOT NULL DEFAULT '{}' _(single row, local mirror of remote)_
- `updated_at` TEXT NOT NULL DEFAULT now

### ui_state
- `key` TEXT PK
- `value` TEXT NOT NULL
- `updated_at` TEXT NOT NULL DEFAULT now

### mls_kv _(OpenMLS storage provider)_
- PK: (`scope`, `key`)
- `scope` TEXT NOT NULL
- `key` BLOB NOT NULL
- `value` BLOB NOT NULL

---

## Local message retention

The local `message` table is **bounded by a device-local retention window** (#150),
so message history on a device does not grow without limit.

- **Setting:** stored in `ui_state` under the key `message_retention_days` — an
  integer count of days. `0` means **Forever** (eviction disabled). The allowed
  values are `0` / `30` / `90` / `365`, validated by the Rust core
  (`set_message_retention`). This is **device-local** — it lives in the local
  SQLite DB and is **never synced** to remote/Turso or to the user's other devices.
- **Eviction:** the sweep (`run_message_eviction`, also fired immediately when the
  setting changes) deletes `message` rows whose `received_at` is older than the
  window. Eviction is keyed on `received_at` (when the row landed on this device),
  not `sent_at`, so a backfilled-but-old message gets its full window locally.
- **mls_kv is never evicted.** Only the `message` table is bounded; MLS group state
  (`mls_kv`) is retained so the device stays a valid group member and can keep
  decrypting and receiving *new* messages. Bounded history never breaks delivery.
- **Reclaiming disk:** the DB runs with `auto_vacuum=INCREMENTAL`; after a sweep
  deletes rows, `incremental_vacuum` returns the freed pages to the filesystem
  rather than leaving the file pre-grown.

This is purely a local storage cap. It does not affect other devices, other
members, or delivery of new messages — see the "History is bounded, not flaky"
product principle in `CLAUDE.md`.

---
_Back to [index.md](./index.md)_
