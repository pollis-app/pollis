# Database

Two databases. Remote schema is frozen in `remote_schema.sql`; changes go in numbered migration files (`000NNN_*.sql`) run by hand against Turso. Local schema is in `local_schema.sql`.

## Remote Database (Turso)

Source: `src-tauri/src/db/migrations/remote_schema.sql` + migrations `000001` through `000015`.

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
- `sender_id` TEXT NOT NULL
- `ciphertext` TEXT NOT NULL _(MLS-encrypted, hex-prefixed with `mls:`)_
- `reply_to_id` TEXT
- `sent_at` TEXT NOT NULL
- `delivered` INTEGER NOT NULL DEFAULT 0

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

### mls_welcome _(migration 3 + 11)_
- `id` TEXT PK _(ULID)_
- `conversation_id` TEXT NOT NULL
- `recipient_id` TEXT NOT NULL FK users
- `welcome_data` BLOB NOT NULL _(TLS-serialized Welcome)_
- `delivered` INTEGER NOT NULL DEFAULT 0
- `created_at` TEXT NOT NULL DEFAULT now
- `recipient_device_id` TEXT _(migration 11)_

### mls_group_info _(migration 13)_
- `conversation_id` TEXT PK
- `epoch` INTEGER NOT NULL
- `group_info` BLOB NOT NULL _(TLS-serialized MlsMessage containing GroupInfo)_
- `updated_at` TEXT NOT NULL DEFAULT now
- `updated_by_device_id` TEXT NOT NULL

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

---

## Local Database (SQLite, per-user, encrypted)

Source: `src-tauri/src/db/migrations/local_schema.sql`

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
_Back to [index.md](./index.md)_
