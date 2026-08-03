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
- `account_id_pub` BLOB _(ML-DSA-44 pub key, 1312 bytes since #668 — was 32-byte Ed25519; added migration 13)_
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
- `type` TEXT NOT NULL DEFAULT 'message' — `'message'` | `'edit'` | `'delete'`
- `target_message_id` TEXT _(the message an `edit`/`delete` envelope acts on)_

**Deletion.** A **self-delete** ("delete for everyone") does NOT use a `'delete'`
envelope — it sends an ordinary `'message'` envelope carrying an E2EE **redaction
control frame** (`0xF6`, see [mls.md](./mls.md#message-deletion--delete-for-everyone-e2ee-redaction)),
so the server never learns which message was redacted, and separately removes the
original envelope. A `type='delete'` **tombstone** (empty ciphertext,
`target_message_id` set) is written only by **admin-delete** (server-authorized
moderation); recipients apply it epoch-independently on ingest.

**`sent_at` is compared lexically, so its precision is load-bearing.** The
message cursor advances on `sent_at > watermark`, and this one column carries
both client-issued stamps (`chrono::Utc::now().to_rfc3339()`, sub-second) and
DS-issued ones. A DS stamp must therefore also carry sub-second digits: a
whole-second `…T09:00:00+00:00` sorts *below* `…T09:00:00.123456789+00:00`
(`'+'` 0x2B < `'.'` 0x2E), which silently buried admin tombstones written in the
same second as the message they redact — the delete appeared to succeed and no
recipient ever applied it. The DS additionally stamps a tombstone strictly above
the conversation's **floor** (`sent_at_after` / `TOMBSTONE_FLOOR` in
`pollis-delivery/src/messages.rs`), so client/DS clock skew cannot reintroduce
the same burial.

That floor is the greater of `MAX(sent_at)` over `message_envelope` **and**
`MAX(last_fetched_at)` over `conversation_watermark`, both scoped to the
conversation (#692). The envelope side alone is not enough: envelope GC deletes
rows once every current member device has watermarked past them, and since the
TTL arm's removal that deletion is purely watermark-gated — so a fully-collected
conversation ends up with *no* envelopes, `MAX(sent_at)` goes NULL, and the stamp
falls back to wall-clock `now`. A recipient whose watermark came from a client
clock running ahead of the DS clock would then never fetch the tombstone (the
cursor only moves forward, so an envelope at or below it is skipped permanently,
not merely delayed) and the deleted message would stay readable on that device
forever. `conversation_watermark` rows outlive envelope GC — they are only ever
advanced, or deleted with the device itself — so they are the evidence the floor
must also consult. Pinned by `messages::tombstone_floor_tests` and
`pollis-delivery/tests/envelope_retention.rs` (Part F).

**Sealed sender (#331, #607).** Attribution is taken from the MLS credential inside
the ciphertext, never from `sender_id` — the ingest reader ([mls.md](./mls.md#sealed-sender-331))
decrypts and reads the credential's `{user_id}:{device_id}`, so a server-written
`sender_id` cannot forge or reveal authorship. Envelope-sender *blinding* is now
**unconditional** (#607): every `type='message'`/redaction envelope is written
`sealed = 1` with a non-identifying sentinel `sender_id` (the string `"sealed"`)
rather than the real sender, so a Turso breach/subpoena of the stored table reveals
nothing about who sent which message. There is no `POLLIS_SEAL_SENDER` flag and no
way to emit an unsealed envelope (invariant test in `flows/sealed_sender.rs`).
Because the stored `sender_id` no longer identifies the author, edit/delete
authorization moved fully **client-side / cryptographic** (Solution A) — the DS
membership-gates edit/self-delete and admin-gates admin-delete, and authorship is
enforced on ingest by matching the MLS credential to the target's author, NOT by a
server policy on this column. `sender_id` stays `NOT NULL` and the sentinel is a
valid value, so the previously-shipped app keeps working (see migration 000008's
backward-compat note; version 000007 is deliberately skipped). **No data
migration** was needed for the cutover — sealing was never previously on, so there
are no pre-existing unsealed rows to convert; new sends simply start sealing. Edit
envelopes (`type='edit'`) keep the real `sender_id` so the DS can membership-gate
on the authenticated editor.

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
- `created_at` TEXT NOT NULL DEFAULT now

Global convergent-encryption dedup: one row (and one R2 blob) per unique file, shared by every message that carries it across all conversations and users. Written via the DS `/v1/attachments/register` endpoint (`apply_register_attachment`), never a client-side INSERT.

**Deletion is reference-counted server-side (#690, migration 000012).** The row — and its R2 object — is collected **only when no still-existing message references the hash**, via a single conditional predicate: `DELETE FROM attachment_object WHERE content_hash = ?1 AND NOT EXISTS (SELECT 1 FROM attachment_ref ar JOIN message_envelope me ON me.id = ar.message_id WHERE ar.content_hash = ?1)` in `apply_delete_attachment`. The count is **derived** — a declaration counts only while its [message_envelope](#message_envelope) still exists (see [attachment_ref](#attachment_ref)). The old delete was unconditional and stranded any other conversation still referencing the file (its attachment 404'd). Legacy rows predating the migration hold no references and are collectable as before (no worse than the old unconditional delete); any send from an updated client re-references the hash and promotes it to counted protection.

### attachment_ref _(migration 000012, #690)_
- PK: (`content_hash`, `message_id`)
- `content_hash` TEXT NOT NULL _(the referenced [attachment_object](#attachment_object))_
- `message_id` TEXT NOT NULL _(the message that carries the attachment)_
- `created_at` TEXT NOT NULL DEFAULT now

One row = "message `message_id` carries the file with `content_hash`". Because the DS cannot read message content (bodies are E2EE ciphertext in [message_envelope](#message_envelope)), it cannot parse attachment references itself — the client DECLARES each one on send (`/v1/attachments/register`, carrying the `message_id`).

**The count is derived, not maintained (#690 Blocking 1 & 2).** A declaration counts only while a `message_envelope` with that id still exists — the reference count is `attachment_ref ⋈ message_envelope`, never the raw rows. So a reference is **released** simply by deleting its message envelope, through the already-authorized paths: self/admin delete, watermark-gated envelope GC (#689), the retention sweep (#720), account/group teardown. Nothing has to *remember* to release, so no deleter can forget (the #691 ELECTRON lesson), and a reference can never outlive its message. `/v1/attachments/delete` no longer mutates `attachment_ref` — it only runs the conditional collect above — so an unauthorized caller naming another member's still-live message cannot strand anything (Blocking 1); releasing is not an action of that endpoint. A declaration whose id names an envelope that never existed (a forged register) or has already gone never counts, and is reaped in bulk by `sweep_envelope_gc` so the table stays bounded. The same derived count gates the R2 object (`/v1/r2/presign` refuses a `delete` while `object_is_referenced` holds — `broker.rs`). PK `(content_hash, message_id)` makes registration idempotent (a retried send cannot double-count) and indexes `content_hash` left-most for the join probe.

**Metadata-exposure trade-off (chosen deliberately; see `docs/metadata-retention-policy.md` §2/§5).** Storing `message_id` in the clear moves the attachment↔message linkage that previously lived only inside the MLS-encrypted payload out to the operator: joining `attachment_ref` to `message_envelope.conversation_id` reveals which messages — and thus which conversations — share a given convergent file. It does **not** reveal the file (plaintext is never seen) or the sender (sealed, #607). The opaque-`ref_token` alternative would hide that graph but leaks references forever when a device loses local state and cannot release a token an admin never held — i.e. it fails deletion, the very thing #690 exists to make correct. We chose the counted, robust option; the incremental exposure is the co-reference graph over hashes the operator already stores.

### conversation_watermark _(migration 5, re-keyed in migration 16)_
- PK: (`conversation_id`, `user_id`, `device_id`)
- `conversation_id` TEXT NOT NULL
- `user_id` TEXT NOT NULL
- `device_id` TEXT NOT NULL
- `last_fetched_at` TEXT NOT NULL

Used by the envelope cleanup sweep — the `CLEANUP_CHANNEL_ENVELOPES` / `CLEANUP_DM_ENVELOPES` DELETEs in `pollis-delivery/src/messages.rs` — to decide when it is safe to drop a row from `message_envelope`. The **trigger** is server-side (#689): the DS runs `sweep_envelope_gc` on a fixed cadence (`POLLIS_DS_GC_SWEEP_SECS`, default 3600s) over every conversation with envelopes, so GC no longer rides a member's ingest path and a conversation whose members all go quiet is still collected. The per-conversation `apply_envelope_gc` (the `/v1/envelopes/gc` endpoint) runs the same watermark-gated cleanup on demand; both share the one cleanup chokepoint. A row is deleted **only** when every **non-revoked** device of every current member has watermarked past `sent_at`. Retention is bounded by the slowest member device and by nothing else — invariant **I3** in `docs/backend-core-invariants.md`.

There is **no TTL, and no time-based gate of any kind may be added here.** An OR'd `sent_at < datetime('now','-30 days')` arm used to sit alongside the watermark gate; because it was OR'd it deleted on its own, without consulting a single watermark, so encrypted mail no recipient device had ever collected was destroyed 30 days after it was sent, and any member offline for longer silently and permanently lost messages. That is failure mode **F3**. Age is not evidence of delivery.

Every branch of the remaining gate fails closed: a member device that has never reported a watermark breaks the `COUNT(devices) = COUNT(watermarks)` check (the LEFT JOIN yields NULL, which `COUNT` skips), so the CASE returns NULL and `sent_at < NULL` deletes nothing; an empty member-device roster aggregates to `COUNT 0 = COUNT 0` with `MIN(...) = NULL` over zero rows and likewise deletes nothing. An unresolvable or fully-vacated roster is never read as "everybody has collected".

**Revoked devices are excluded from the roster the watermark gate is measured against** (`AND ud.revoked_at IS NULL` in the `user_device` join, #685). A revoked device can never rejoin the tree (invariant I5), so it must not count: otherwise its stale watermark pins `MIN(cw)` down, or — with no watermark row at all — it breaks the `COUNT(devices) = COUNT(watermarks)` "every device reported" check and disables pruning **entirely**, leaving envelopes every live device has long since read on the server forever. This mirrors `commit::current_member_devices`, which excludes revoked devices from the commit-log retention floor for the same reason.

That mirroring is **enforced, not merely documented** (#722). "Which member devices count toward retention" has three implementations that had to agree by hand — the two `CLEANUP_*` DELETEs and `commit::current_member_devices` — and a divergence is an I5 violation surfacing as dropped mail (roster too narrow) or retention that never clears (roster too wide). The roster half of each DELETE now lives in the `channel_member_device_rows!` / `dm_member_device_rows!` macros in `messages.rs`, `concat!`-ed back into the statement (the executed SQL is byte-identical, pinned by a test). `messages::roster_parity_tests` SELECTs the *same* join chain the DELETE aggregates over and asserts it resolves the identical device list as the Rust function, over a fixture covering plain members, mixed live/revoked devices, all-revoked members, non-members, member devices with no watermark row, and both conversation shapes.

Seed paths (so a new device or a pre-join user doesn't block cleanup retroactively). All three skip revoked devices (`revoked_at IS NULL`, #685) — a revoked device is not part of the GC roster, so seeding it a row would just re-introduce the wedge:
- group member-add (`pollis-delivery` `groups.rs::add_member_rows`) seeds one row per (channel, device) for the joining user at join time.
- DM create / DM member-add (`pollis-delivery` `profile.rs`) seed per (member, device).
- `register_device` seeds per conversation the user is already a member of, for the newly-registered device.

Teardown: `POST /v1/devices/revoke` (`account.rs::apply_revoke_device`) DELETEs the revoked device's watermark rows in the same transaction as the tombstone, scoped `WHERE user_id = actor AND device_id = ?` (#685). The read-time filters above already make the prune floor correct, so this is the "invalid states unrepresentable" half: a cursor that can never advance stops existing rather than relying on every future reader remembering to exclude it. Note `POST /v1/auth/logout` DELETEs the `user_device` row outright, and the watermark join hangs off `user_device`, so a logged-out device's leftover rows are already invisible to the gate.

### voice_presence _(removed in migration 18)_
Dropped. LiveKit's `RoomService.ListParticipants` / `ListRooms` is the source
of truth for who is currently in a voice channel. The shadow table drifted
on every crash/force-kill/network blip; querying LiveKit directly closed
that class of bug.

### user_device _(migration 11 + 13 + post-baseline 000011)_
- `device_id` TEXT PK
- `user_id` TEXT NOT NULL FK users
- `device_name` TEXT
- `created_at` TEXT NOT NULL DEFAULT now
- `last_seen` TEXT NOT NULL DEFAULT now
- `device_cert` BLOB _(migration 13; since #668 a **v2** cert — a 2420-byte ML-DSA-44 signature by `account_id_pub` binding BOTH leaf keys below, so neither is uncertified. It was the classic→PQ overlap that needed that; #669 ended the overlap but the cert format and both keys are unchanged)_
- `cert_issued_at` TEXT _(migration 13)_
- `cert_identity_version` INTEGER _(migration 13)_
- `mls_signature_pub` BLOB _(migration 13; the device's raw 32-byte **Ed25519** MLS leaf key — the one the retired classic suite's leaves signed with. No longer the DS request-auth credential since #668, and since #669 no live suite mints a leaf under it; still generated, still published, and still bound by the v2 cert, because `load_or_create_device_signer` is keyed by signature *scheme* and a group persisted under an older code point must stay readable.)_
- `mls_signature_pub_pq` BLOB _(post-baseline 000011, #668; the device's raw 1312-byte **ML-DSA-44** MLS leaf key — the one `CS_PQ` leaves sign with, and the credential `X-Pollis-Signature` is verified against (`pollis-delivery/src/auth.rs`). A separate column and not an overload of `mls_signature_pub`: overwriting that one with a 1312-byte key would break auth for every already-shipped client the instant the migration ran. Nullable — NULL means the device predates #668; such a row is skipped by `resign_stale_device_certs`, treated as `AbsentRetry` by `verify_added_devices`, and self-heals on that device's next `ensure_device_cert`.)_
- `pq_capable` INTEGER NOT NULL DEFAULT 0 _(post-baseline 000010; **DEAD since #669** — retired in place, not dropped, because migrations must stay additive. Nothing writes it (`devices.rs::mark_pq_capable` and both `UPDATE user_device SET pq_capable = 1` statements are gone) and nothing reads it. Under #454 P2 the DS publish/replenish endpoints set it to 1 when a device published a hybrid KeyPackage pool — server-derived from what landed in `mls_key_package`, never a client UPDATE — and #454 P5 read it two ways: per-roster by `roster_is_fully_pq_capable`, and deployment-wide by `fleet_is_fully_pq_capable` (no row with `revoked_at IS NULL` and `last_seen` inside 90 days still at 0). Both had to pass before a group was born on or migrated to the hybrid suite. #669 retired the classic suite, so there is no classic-only device for the flag to distinguish; all three readers are deleted — see `.codesight/wiki/mls.md`.)_

### mls_key_package _(migration 3 + 11 + post-baseline 000010)_
- `ref_hash` TEXT PK _(KeyPackageRef hash, hex)_
- `user_id` TEXT NOT NULL FK users
- `key_package` BLOB NOT NULL _(TLS-serialized KeyPackage)_
- `claimed` INTEGER NOT NULL DEFAULT 0
- `created_at` TEXT NOT NULL DEFAULT now
- `device_id` TEXT _(migration 11)_
- `ciphersuite` INTEGER NOT NULL DEFAULT 1 _(post-baseline 000010; MLS code point. Still written and still queried after #669 — every package now carries `0x0052` (`CIPHERSUITE_PQ`), which is also what the DS assumes when a publish/claim request omits the field (it defaulted to the classic `1` before #669). A column and not derived-on-read because `key_package` is an opaque TLS blob the DS must not parse. Claims narrow on it: under #454 that kept the classic and hybrid pools disjoint, and it still keeps a package published under a retired code point from being served for a current group — see `pollis-delivery/src/devices.rs`. The SQL default of 1 is left as-is because migrations must stay additive; no live writer relies on it.)_

### mls_commit_log _(migration 3 + 14)_
- `seq` INTEGER PK AUTOINCREMENT
- `conversation_id` TEXT NOT NULL
- `epoch` INTEGER NOT NULL _(epoch BEFORE this commit)_
- `sender_id` TEXT NOT NULL FK users
- `commit_data` BLOB NOT NULL _(TLS-serialized MLS Commit)_
- `created_at` TEXT NOT NULL DEFAULT now
- `added_user_id` TEXT _(migration 14, NULL if no adds)_
- `added_device_ids` TEXT _(migration 14, comma-separated)_
- `generation` INTEGER NOT NULL DEFAULT 0 _(commit-log-DB migration 000004, #454 P4 — the **suite generation**)_
- UNIQUE INDEX `idx_mls_commit_conv_gen_epoch` on `(conversation_id, generation, epoch)` _(migration 000004, replacing `idx_mls_commit_conv_epoch`)_

**Suite generations (#454 P4).** MLS binds the ciphersuite into the group, so a
conversation cannot be switched to another suite in place: the migration stands up
a *successor* group for the same conversation and moves the roster into it by
Welcome. #454 used this to move classic conversations onto the PQ suite; since #669
retired the classic suite it is the mechanism for moving off a **retired code
point** — `0x0052` is provisional and the draft has renumbered once already. A
successor restarts at MLS epoch 0, so the monotone
key widens from `(conversation_id, epoch)` to `(conversation_id, generation,
epoch)`, ordered **lexicographically**. Every row that exists today is
generation 0, and a pre-hybrid client — which sends no generation field — is a
generation-0 writer, so nothing about its behaviour changes. The DS accept rule
(`pollis_delivery::commit::submit_commit` / the pure `accepts`) has exactly two
branches:
- **Continue** the head lineage: `generation = MAX(generation)` and
  `based_on_epoch` = that lineage's head. Byte-for-byte the pre-P4 rule.
- **Open** the next lineage: `epoch = 0`, `generation = MAX(generation) + 1`, and
  the submitter must name the head it closes in `closes_epoch`. That makes a
  migration a compare-and-swap on the OLD head too, so a commit landing on the old
  lineage between the migrator's read and its submit invalidates the migration
  rather than being orphaned by it. A generation can never be opened twice.

### mls_welcome _(migration 3 + 11; now on the commit-log DB)_
- `id` TEXT PK _(ULID)_
- `conversation_id` TEXT NOT NULL
- `recipient_id` TEXT NOT NULL FK users
- `welcome_data` BLOB NOT NULL _(TLS-serialized Welcome)_
- `delivered` INTEGER NOT NULL DEFAULT 0
- `created_at` TEXT NOT NULL DEFAULT now
- `recipient_device_id` TEXT _(migration 11)_
- `generation` INTEGER NOT NULL DEFAULT 0 _(migration 000004, #454 P4)_ — the lineage this Welcome **admits into**, so a recipient can tell "added to the group you're in" from "moved to the successor group" BEFORE applying the blob. It needs that before, not after: `max_past_epochs = 0` means it must finish draining its current lineage first.
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
- `generation` INTEGER NOT NULL DEFAULT 0 _(migration 000004, #454 P4)_

Both writers (`writes::upsert_group_info` and the inline write in the DS submit
bundle) guard the upsert on `(generation, epoch)` **lexicographically**, not on
`epoch` alone. The generation term is load-bearing at a migration: the successor
lineage's epoch 1 is numerically BELOW the retired lineage's last epoch, so an
epoch-only guard would reject the successor's GroupInfo and strand every
externally-joining device on the retired suite.

### mls_commit_since _(commit-log-DB migration 000003, #539)_
Per-device commit-log catch-up high-water — the signal the **retention floor** is
computed from. On its catch-up a client reports the epoch it is caught up FROM
(its current local MLS epoch); the DS records it and prunes `mls_commit_log` below
the floor. Lives on the commit-log DB; the DS is the sole writer.
- `conversation_id` TEXT NOT NULL
- `user_id` TEXT NOT NULL _(FK users dropped — cross-DB, like the sibling log tables)_
- `device_id` TEXT NOT NULL
- `since_epoch` INTEGER NOT NULL _(the device's applied MLS epoch; it still needs commits `>= this`)_
- `generation` INTEGER NOT NULL DEFAULT 0 _(migration 000004, #454 P4)_ — the reported high-water is the PAIR `(generation, since_epoch)`, upserted monotone **lexicographically**. A per-column `MAX` would get the migration backwards: moving to the successor lineage at epoch 0 is forward progress even though the epoch drops.
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

Under suite generations the floor becomes a **pair** — a generation floor plus an
epoch floor within the head generation (`closed_generation_floor` alongside
`prune_floor`):
- The epoch floor is the two-tier floor above, computed strictly WITHIN the head
  generation over devices that have reported themselves in it. A device still on
  an older lineage has no comparable epoch there, so it counts as *unreported* —
  which conservatively disables Tier 1 rather than letting a stale lower epoch
  masquerade as a bound.
- The generation floor retires whole CLOSED lineages, which epoch pruning alone
  would keep forever (all their epochs sit below their own head, never below the
  head generation's floor). Tier 1: delete generations below the MIN reported
  generation — every current member device has moved past them, so zero loss.
  Tier 2: once the head generation has itself run `PRUNE_MAX_BEHIND_HEAD` epochs,
  a device stranded on an older lineage is by construction at least that far
  behind head, which is already the accepted-loss condition. The LIVE lineage is
  never retired wholesale, whatever the reports say.

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
- `account_id_pub` BLOB NOT NULL _(ML-DSA-44 pub authoritative at this version)_
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
