# Database

Two databases. Remote schema starts from `000000_baseline.sql` (a full canonical dump) plus additive migrations (`000NNN_*.sql`). Local schema is in `local_schema.sql`.

## Where the remote schema lives (#875)

`pollis-schema/` — its own dependency-free crate, holding `migrations/` (main DB)
and `migrations-log/` (commit-log DB) plus the Rust constants that embed them:
`BASELINE_SQL`, `LOG_DB_SCHEMA`, `POST_BASELINE_MIGRATIONS`,
`POST_BASELINE_LOG_MIGRATIONS`, and the ordered `main_scripts()` /
`log_scripts()` / `single_db_scripts()`. `pollis-core::db` re-exports the four
constants, so existing import paths are unchanged.

It is a separate crate because `pollis-delivery` must not depend on
`pollis-core` (see `.codesight/wiki/mls.md` — the DS is the sole writer to the
MLS control plane and does not take the client core into its build). Before the
split, that independence was paid for with ~22 hand-rolled `const SCHEMA` blocks
across the DS's tests, which had drifted into eleven different `user_device`
definitions and several `group_member` tables with no primary key — so those
tests could create duplicate membership rows production rejects. DS tests now
call `pollis_schema::apply::{main_db, log_db, single_db}` (the `libsql` feature,
a dev-dependency) and run against exactly what ships. `pollis-device-cert` is
the same pattern for the device-cert wire format.

Adding a migration: drop the numbered file in `pollis-schema/migrations/` (or
`migrations-log/`) **and** add it to the matching list in
`pollis-schema/src/lib.rs`. `every_migration_file_is_listed` fails the build if
you add the file and forget the list — which would otherwise mean prod applies
the migration and no test harness does.

## How schema changes ship

1. Write a new migration file: `pollis-schema/migrations/000NNN_description.sql`. Version number must be the next integer.
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

> **A note on migration numbers.** Numbers below the baseline (`000000_baseline.sql`)
> refer to a **pre-baseline** series that no longer exists as files — the baseline
> collapsed them. Where this doc says e.g. "migration 13" or "migration 18" it means
> that historical series, not `pollis-schema/migrations/`. Post-baseline
> migrations are cited with their full six-digit name. The commit-log DB has its own
> independent series under `migrations-log/`, starting again at `000001`; do not
> confuse the two. Main-DB `000007` is a permanent hole — never reuse it.

## Remote Database (Turso)

Source: `pollis-schema/migrations/000000_baseline.sql` + numbered migrations `000001`+.

### How the DS connects to it (#777)

`pollis-delivery/src/db.rs` **pools** libsql connections; `Db::conn()` returns a
`ConnGuard` (derefs to `libsql::Connection`, parks it again on drop) rather than
a fresh connection. It has to: for a remote database `Database::connect()` builds
a whole new `hyper::Client`, and hyper's connection pool lives in the client — so
one connection per request meant one TCP + TLS handshake per request, which
multiplies the DS↔Turso distance instead of adding to it (a large part of the
~1 530 ms/request dev regression in #658).

Three rules keep reuse safe, all enforced in `db.rs` rather than by convention:

- **Checkout is exclusive.** The connection is removed from the pool and only the
  guard's `Drop` returns it, so no two callers hold one. Required: a connection
  carries per-connection state (`changes()`, `last_insert_rowid()`) and one hrana
  stream whose statements serialize on a single mutex — a process-wide shared
  connection would corrupt the former and funnel every DS query through the
  latter.
- **Parked connections expire.** A `query()` leaves its hrana stream open
  (describe + cursor never close it) and the server reaps idle streams, so
  anything parked past `DEFAULT_MAX_IDLE` (3s) is discarded instead of reused.
  That is precisely the sporadic-traffic case where the handshake cost is
  irrelevant anyway.
- **Checkout never blocks.** The pool is elastic (mint on miss, cap the *parked*
  count, never the live count) because handlers nest checkouts — `submit` holds a
  `log_db` connection while taking a `db` one, and in single-DB deploys those are
  the same pool. A fixed-size blocking pool would deadlock there.

Transactions are untouched: hrana's `transaction()` calls `open_stream()`, so
every transaction runs on its own stream regardless of pooling. Pooling also does
not widen concurrency — every request already had its own connection; they are
now reused rather than rebuilt.

### users
- `id` TEXT PK
- `email` TEXT NOT NULL UNIQUE
- `username` TEXT
- `phone` TEXT
- `avatar_url` TEXT
- `created_at` TEXT NOT NULL DEFAULT now
- `account_id_pub` BLOB _(ML-DSA-44 pub key, 1312 bytes since #668 — was 32-byte Ed25519. Folded into the baseline)_
- `identity_version` INTEGER NOT NULL DEFAULT 1 _(increments on identity reset. Folded into the baseline)_
- `preferred_name` TEXT _(migration `000001`)_
- There is **no** `identity_key` column. This doc used to list one as "legacy, unused"; it does not exist in the schema (#804). The local DB has an unrelated table of that name — see below.

**`email` is the account's identity, so it must be canonicalized before it reaches
this table** — `NOT NULL UNIQUE` cannot merge `" a@x.com "` with `"a@x.com"`, it just
lets both exist as two accounts for one person. Canonical form is **trim only**
(deliberately *not* lowercased: only the OTP store key is lowercased, in
`pollis_delivery::otp::normalize_email`). Both writers apply it at the function
holding the INSERT, not at their handlers, because both are also called directly by
in-process harnesses:

| writer | function | when |
|---|---|---|
| DS (authoritative, and the only one) | `pollis_delivery::otp::apply_verify_otp` | every sign-in |

**There is exactly one writer since #910.** `pollis-core` used to carry a twin
(`auth::resolve_or_create_user_by_email`, reached from the `#[cfg(debug_assertions)]`
no-DS `dev_login` shortcut) that INSERTed the `users` row client-side — the one live
counter-example to *"Never add a client-side remote INSERT/UPDATE/DELETE — extend a DS
endpoint"*. It is gone, along with the other two direct-to-Turso fallbacks it turned
out to be keeping company with (`key_packages`' replenish/publish and
`account_identity::generate_account_identity`). Keeping two hand-maintained twins in
step was itself a standing cost — before #875 they had already drifted, the client copy
not trimming the address.

A DS-less client is not a degraded configuration, it is a non-functional one: every
remote write is a `POST /v1/...`, so such a client could not send a message or register
a device either. `dev_login` now fails with that message rather than creating a local
account that 401s on everything.

`pollis-core/tests/no_client_side_remote_writes.rs` keeps it that way: it parses the
remote table list out of `pollis-schema` and fails the build if any non-test code in
`pollis-core` writes one.

### conversation _(migration `000016`, #880)_
- `id` TEXT PK — the conversation-id namespace, shared by all three kinds
- `kind` TEXT NOT NULL CHECK (`dm` | `group` | `channel`)
- `created_at` TEXT NOT NULL DEFAULT now
- UNIQUE index on (`id`, `kind`) — redundant against the PK today; it exists to back the composite foreign key the tightening phase needs (see below)

**What it is for.** `dm_channel`, `groups` and `channels` are three tables with
three separate primary keys, so SQLite accepts the same id in all three — and
`is_member` (`pollis-delivery/src/writes.rs`) ORs across all three on one id. A
DM whose id is a victim group's id therefore made `is_member(victim_group, you)`
true: membership of a group you were never in, and with it commit injection, a
GroupInfo/Welcome overwrite and a LiveKit token for their room. Not a ULID
collision — every creation endpoint takes the id from the request body, so an
attacker *names* an existing conversation.

`conversation` owns that namespace. Every DS create path claims the id here **in
the same transaction** as the row it names (`writes::claim_conversation_id`), so
a second claim of one id cannot commit: the primary key refuses it and the
creation rolls back whole. `conversation_id_taken` still runs first and is still
worth having — it turns the refusal into a clean 403 instead of a 500 — but it is
no longer the only thing standing between an attacker and a shared id.

**Append-only.** Rows are never deleted, not even when the conversation is. A
claimed id is claimed forever, because the attack is *reuse*: a deleted
conversation is the most attractive id to squat (members' clients may still hold
MLS state keyed to it), and with `foreign_keys=OFF` in production the baseline's
`ON DELETE CASCADE` clauses do not fire. The cost is one small row per
conversation ever created, with no user linkage.

(The "a deleted group leaves its `group_member` and `channels` rows behind" that
used to motivate this paragraph was literally true and is now fixed —
`pollis-delivery/src/teardown.rs` deletes them by name. The registry stays
append-only regardless: id reuse is the attack, and not re-opening that window
does not depend on teardown being complete.)

**Enforced by triggers since `000017` (#948).** Each of the three tables now
carries a `BEFORE INSERT` and a `BEFORE UPDATE OF id` guard trigger that
`RAISE(ABORT)`s unless the registry holds exactly `(NEW.id, that table's
kind)` — so a row the registry never granted, or one naming another kind's id,
is refused **by the database**, with no `pollis-delivery` code in the path.
Triggers rather than a foreign key for two reasons:
- production Turso runs with **`foreign_keys=OFF`**, so any FK is inert there;
  triggers fire regardless of the pragma, which the commit-log DB already
  relies on (`migrations-log/000005`). A composite `(id, kind)` FK (a bare one
  is satisfied by the same parent row for two children) would also mean
  rebuilding all three tables — SQLite's non-additive 12-step dance;
- the triggers are **validators, not claimers**: `claim_conversation_id`
  remains the one writer (same transaction, so the trigger sees the
  uncommitted claim), which made `000017` deployable against the running DS
  with no code change. A claiming trigger would have double-inserted against
  the live DS and failed every creation until the next deploy.
Test fixtures that seed these tables raw must claim first — the registry
insert precedes the row it names, exactly as production does. The DB-level
refusal is pinned in `pollis-delivery/tests/conversation_namespace.rs` (the
"guard triggers" section), and `db-audit.yml` (workflow_dispatch) runs the
read-only collision + registry-gap audit against dev or prod where the
credentials live.

### groups
- `id` TEXT PK — also registered in `conversation` with `kind='group'` (#880)
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
- `id` TEXT PK — also registered in `conversation` with `kind='channel'` (#880), including the default text/voice channels `groups/create` makes
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

**The CLIENT side is correct as it stands, and must not be "fixed" to match
(#875).** `chrono::Utc::now().to_rfc3339()` is `SecondsFormat::AutoSi`, which
emits 0, 3, 6 or 9 fraction digits — it **omits** the fraction only when the
instant's nanoseconds are genuinely zero, and it never **truncates** a real one.
A fraction-less client stamp therefore denotes `.000000000`, the earliest instant
of its second, so sorting below every fractional stamp in that second is the
*correct* answer and `'+' < '.'` is what produces it. The #692 bug was a
hand-rolled DS formatter that threw a real fraction away; that is a different
thing, and only the DS needed `SecondsFormat::Nanos`. Switching the ~25
`to_rfc3339()` call sites in `pollis-core` to `Nanos` would change no ordering
and buy nothing.

What #875 *did* change is where the client's format is decided: the five sites
that write a `message_envelope.sent_at` now all go through
`pollis_core::commands::messages::envelope_sent_at`, so the format is one
function with `sent_at_format_tests` around it rather than five independent
`now()` calls. The sites that write LOCAL-only timestamps (`message.deleted_at`,
`message.edited_at`, `mls_self_update.last_at`, profile/group `created_at`, …)
deliberately do **not** use it — those columns are never compared against a
cursor, and routing them through an envelope-specific helper would imply a
constraint they do not have.

Four tests pin the ordering: `sent_at_format_tests::
sent_at_never_truncates_a_real_fraction` (pollis-core), `timestamp_tests::
lexical_order_matches_chronological_order_across_precisions`,
`admin_delete_visibility_tests::admin_tombstone_always_reaches_a_caught_up_recipient`
(which runs the zero-nanosecond shape through the real delete path) and
`…::a_zero_nanosecond_client_message_still_outsorts_the_preceding_tombstone`
(the reverse direction — a message sent on a second boundary after a tombstone
still clears the recipient's watermark). Any change to timestamp formatting on
either side must keep all four green.

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
- `id` TEXT PK — also registered in `conversation` with `kind='dm'` (#880)
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
- `status` TEXT NOT NULL DEFAULT 'pending' CHECK (`pending`, `accepted`, `declined`)

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

### read_cursor_sync _(migration 000018, #844)_
- `user_id` TEXT PK
- `nonce` BLOB NOT NULL _(12-byte AES-256-GCM nonce, fresh per push)_
- `blob` BLOB NOT NULL _(AES-256-GCM ciphertext + tag over the padded cursor JSON)_
- `updated_at` TEXT NOT NULL DEFAULT now

One row per account: the user's read positions, so a conversation read on the
laptop is not unread on the phone. **Ciphertext, unlike its neighbour
`user_preferences`,** and that difference is the whole point. Preferences in the
clear cost the operator's knowledge of a theme name; a read cursor in the clear
would be a timestamped log of which conversations a human reads and when — a
behavioural profile richer than the envelope metadata the threat model already
concedes. The client seals it with AES-256-GCM under a key derived by HKDF-SHA256
from the **account identity key** (every device of the account holds a copy; the
DS never has it in the clear), after padding the payload to the size buckets in
`pollis_core::commands::messages::framing`. The DS validates only the envelope —
base64, nonce length, ≤ 256 KiB — and stores the bytes verbatim
(`pollis-delivery/tests/read_cursor_sync.rs` pins byte-exact custody, since a
server that "helpfully" re-encoded the blob would break every other device's tag
check). Last-writer-wins is safe because the merge is a client-side `max()` over
per-conversation positions that only move forward. What the server still learns:
that the blob changed, its size bucket, and when.

### pin_keystate _(migration 000019, #99)_
- `conversation_id` TEXT PK _(the MLS conversation — group id or DM id, same key as `mls_group_info`)_
- `wrapped_kpin` BLOB NOT NULL _(AES-256-GCM ciphertext of the 32-byte pin master key under the epoch's exporter-derived KEK)_
- `nonce` BLOB NOT NULL _(12 bytes, fresh per re-wrap)_
- `generation` INTEGER NOT NULL DEFAULT 0
- `epoch` INTEGER NOT NULL
- `updated_at` TEXT NOT NULL DEFAULT now
- `updated_by_device_id` TEXT NOT NULL _(diagnostic; authz is the signed request)_

One mutable row per MLS conversation: the CURRENT wrap of `Kpin`, the key every
pin snapshot in the conversation is sealed under. Re-wrapped (~60 bytes) whenever
the epoch advances — O(1) per commit regardless of pin count — under the same
lexicographic `(generation, epoch)` CAS as `mls_group_info`, so a stale wrap can
never roll the row back to an epoch a removed member could still derive. A MINT
(`mint: true` on `POST /v1/pins/keystate`) is insert-only — refused whenever any
row exists, even at a newer epoch — which is what makes a key fork
unrepresentable when two devices race to create the first key. The DS can open
neither this wrap nor the pins it protects. Purged with its conversation, and
deliberately NOT purged when a member leaves (the remaining members' pin access
lives in this row).

### pinned_message _(migration 000019, #99)_
- `id` TEXT PK _(pin ULID)_
- `conversation_id` TEXT NOT NULL _(the channel or DM the message lives in)_
- `message_id` TEXT NOT NULL _(no FK — a pin snapshot deliberately outlives envelope GC)_
- `pinned_by` TEXT NOT NULL
- `encrypted_content` BLOB NOT NULL _(AES-256-GCM under the conversation's `Kpin`: a padded JSON snapshot of the message)_
- `nonce` BLOB NOT NULL _(12 bytes)_
- `pinned_at` TEXT NOT NULL DEFAULT now
- UNIQUE `(conversation_id, message_id)`; index `(conversation_id, pinned_at DESC)`

Written once at pin time, never re-encrypted. The snapshot repeats
`message_id`/`conversation_id` INSIDE the ciphertext so a spliced row is detected
client-side instead of rendering as a relocated pin. Membership gates every read
and write (`writes::is_member` accepts all three id kinds). Rows a departing user
pinned go with their account; the conversation keeps other members' pins.

### vault_message _(migration 000020, #107)_
- `id` TEXT PK _(entry ULID, minted client-side, stable across edits)_
- `user_id` TEXT NOT NULL _(owner; bound to the authenticated user on every touch)_
- `nonce` BLOB NOT NULL _(12 bytes, fresh per save)_
- `blob` BLOB NOT NULL _(AES-256-GCM ciphertext + tag over the padded entry JSON)_
- `created_at` TEXT NOT NULL _(client-supplied; kept on edits)_
- `updated_at` TEXT NOT NULL DEFAULT now
- index `(user_id, created_at DESC)`

The Vault: `read_cursor_sync`'s construction per entry — HKDF-SHA256 from the
ACCOUNT identity key (info `pollis-vault-v1`) → AES-256-GCM — so every enrolled
device decrypts every entry and the DS decrypts none. This is the stated,
cryptographic exception to accepted loss (2): a new device starts FULL here. The
entry body, its pinned flag and its attachment references all live inside the
ciphertext; the upsert's update arm carries `WHERE vault_message.user_id = ?`
so a colliding ULID from another account is a refused write, not an overwrite.
Hard-deleted on entry delete and purged with the account.

### vault_attachment_ref _(migration 000020, #107)_
- `content_hash` TEXT NOT NULL
- `vault_message_id` TEXT NOT NULL
- `created_at` TEXT NOT NULL DEFAULT now
- PK `(content_hash, vault_message_id)`; index on `vault_message_id`

The vault's leg of the attachment-GC liveness predicate: `live_ref_exists`
(`pollis-delivery/src/messages.rs`) now ORs "an `attachment_ref` joined to a
live envelope" with "a `vault_attachment_ref` joined to a live `vault_message`",
so a file that exists only in someone's vault survives every collect pass.
Maintained by `/v1/vault/save` (replace-on-save — an edit that drops a file
releases it) and `/v1/vault/delete`; reaped by `sweep_envelope_gc` if ever
orphaned. The user→hash pairing it records is already conceded by the signed
upload presign.

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

### custom_emoji_object _(migration 000015, #848)_
- `content_hash` TEXT PK _(SHA-256 of the **re-encoded** bytes)_
- `r2_key` TEXT NOT NULL _(`emoji/<content_hash>.<ext>`, DERIVED server-side — never accepted from a client)_
- `content_type` TEXT NOT NULL _(`image/webp` or `image/gif` — the only two the re-encoder emits, allowlisted at the DS)_
- `size_bytes` INTEGER NOT NULL _(≤ 48 KiB, re-checked at registration and bound into the presigned PUT's signature)_
- `animated` INTEGER NOT NULL DEFAULT 0
- `created_at` TEXT NOT NULL DEFAULT now

Global dedup for custom emoji: one row (and one R2 blob) per unique image, shared by every group that registered it — fifteen groups adding party-parrot produce fifteen [group_emoji](#group_emoji) rows and exactly one object. The hash is of the bytes `pollis-core`'s `encode_emoji` produced, not of the user's file, so two people uploading the same picture in different formats still converge; the re-encoder is deterministic, which is what makes that true.

**These blobs are NOT encrypted**, unlike [attachment_object](#attachment_object). Deliberate (#848): the product rule is that a member of group A may use A's emoji while talking in group B, and everyone in B must *render* it — there is no key a B-only member could hold, so an encrypted emoji is one that cannot cross a group boundary. Message content stays E2EE; what leaves the envelope is `<:shortcode:hash>`. Cost, stated plainly: the operator can see every custom-emoji image and which group registered it — not who sent one, where, or in what message.

**Collection is reference-counted, derived exactly as attachments are.** The row goes only when no `group_emoji` row names its hash: `DELETE FROM custom_emoji_object WHERE content_hash = ?1 AND NOT EXISTS (SELECT 1 FROM group_emoji WHERE content_hash = ?1)`. Removing a group emoji runs that collect inline; `POST /v1/emoji/gc` sweeps in bulk for references dropped without going through it (notably group deletion, which releases via `release_group_emoji`). The R2 blob is gated by the same predicate — `/v1/r2/presign` refuses both `delete` **and** `put` for a referenced emoji hash (`broker.rs`), the `put` gate mattering more here than for media because the object is public and unencrypted, so an overwrite would replace an image every referencing group renders.

### group_emoji _(migration 000015, #848)_
- PK: (`group_id`, `shortcode`)
- `group_id` TEXT NOT NULL
- `shortcode` TEXT NOT NULL _(`[a-z0-9_]{2,32}`, validated at the DS)_
- `content_hash` TEXT NOT NULL _(the referenced [custom_emoji_object](#custom_emoji_object))_
- `created_by` TEXT NOT NULL
- `created_at` TEXT NOT NULL DEFAULT now
- INDEX: (`content_hash`), (`created_by`)

One row = "group `group_id` calls this object `:shortcode:`". The PK makes a shortcode unique within its group (two members racing to add `:parrot:` cannot both win) while the same image in another group is a second row pointing at the one object.

**Permissions.** *Adding* requires current membership of the target group; *removing* requires being the row's creator or a group admin. *Using* is broader and is the #848 rule verbatim: `user_can_use_emoji` is true when the emoji is registered to **any** group the user is currently a member of, with no conversation term at all — so a member of group A may send A's emoji in group B, and a user in neither cannot send it but still renders it. That predicate is also what fills the picker (`list_usable_emoji`), so the set offered and the set permitted cannot drift. It cannot be enforced server-side at send time — the DS never sees message content — so `prepare_emoji_text` applies it on the sender's device, degrading a disallowed token to plain `:shortcode:` rather than failing the send.

**Bounds.** There is deliberately **no per-group cap** (#848 is explicit). The bound is per-person: `EMOJI_MAX_PER_USER` (1000) rows per creator across all groups, which with the 48 KiB object ceiling caps one account's worst-case storage at 48 MiB — and usually far less, since their second copy of a popular emoji costs a row and no bytes.

### conversation_watermark _(migration 5, re-keyed in migration 16)_
- PK: (`conversation_id`, `user_id`, `device_id`)
- `conversation_id` TEXT NOT NULL
- `user_id` TEXT NOT NULL
- `device_id` TEXT NOT NULL
- `last_fetched_at` TEXT NOT NULL _(the message cursor — how far the device has read)_
- `reported_at` TEXT _(migration 000012, #720; server-stamped wall-clock time of the device's LAST report — the device-liveness signal, distinct from the message cursor. Nullable: pre-migration rows are NULL and treated as live)_

**The two columns are in different text formats, deliberately (#908).** A text timestamp's format is decided by one thing: what it is compared against. `last_fetched_at` is compared lexically against `message_envelope.sent_at`, so it is **RFC 3339** — clients write it through `pollis_core::commands::messages::envelope_sent_at`, and the DS's own server-side seeds write it through `pollis_delivery::messages::seeded_watermark_cursor`. `reported_at` is compared against `datetime('now', ?)` in the `CLEANUP_*` predicate, so it stays in **SQLite's `YYYY-MM-DD HH:MM:SS`**. Until #908 the three seed paths (device registration, DM create / DM member-add, group join) wrote `datetime('now')` into *both*; because a space (0x20) sorts below a `T` (0x54), a seeded cursor compared as older than every RFC 3339 stamp sharing its calendar day — the opposite of the "this device has already consumed the backlog" the seed exists to assert. Fail-safe (over-pin, under-collect — envelopes were kept, never lost), but the seed did not do what its comment claimed. `seeded_watermark_cursor` is the DS-side counterpart to `envelope_sent_at`: one function owns the format so five call sites cannot each get it wrong.

Used by the envelope cleanup sweep — the `CLEANUP_CHANNEL_ENVELOPES` / `CLEANUP_DM_ENVELOPES` DELETEs in `pollis-delivery/src/messages.rs` — to decide when it is safe to drop a row from `message_envelope`. The **trigger** is server-side (#689): the DS runs `sweep_envelope_gc` on a fixed cadence (`POLLIS_DS_GC_SWEEP_SECS`, default 3600s) over every conversation with envelopes, so GC no longer rides a member's ingest path and a conversation whose members all go quiet is still collected. The per-conversation `apply_envelope_gc` (the `/v1/envelopes/gc` endpoint) runs the same watermark-gated cleanup on demand; both share the one cleanup chokepoint. That endpoint has **no client caller and is not meant to have one** — it is operator tooling, classified `Audience::Operator` in `pollis-api`'s endpoint table (so `ds_post` will not accept it) and documented in `docs/ds-operator-endpoints.md` (#925). A row is deleted **only** when every **non-revoked, live** device of every current member has watermarked past `sent_at`. Retention is bounded by the slowest member device **and by device liveness** — invariant **I3** in `docs/backend-core-invariants.md`.

**Device-liveness bound (#720).** A member device that has not **reported** a watermark within `POLLIS_DS_WATERMARK_STALE_MONTHS` (**configured 12 months**; code default 6 when unset) stops pinning: the `CLEANUP_*` `?2` arm excludes it from the roster (`cw.reported_at IS NULL OR cw.reported_at >= datetime('now', ?2)`), mirroring the revoked-device exclusion. The bound is keyed on `reported_at` (the device's last report), **never** on `last_fetched_at` or the envelope's `sent_at`, so a device that reported recently pins every envelope below its cursor however old — this is **not** a TTL. The predicate lives OUTSIDE the shared roster macro (which stays byte-for-byte identical to `commit::current_member_devices` for the I5 parity test); it is an envelope-only bound. Both GC paths pass the same window through the one chokepoint, so they cannot diverge. A NULL `reported_at` (pre-migration row, or a device with no watermark row) is treated as live and keeps pinning (fail-closed). Reporting refreshes `reported_at` to now, so the bound is reversible: a returning device pins again immediately, without being revoked or removed from any roster. The trade is the **third accepted message loss** (a device dormant past the window may return to gaps). N was set to **12 months** on 2026-08-04 — rationale in `docs/metadata-retention-policy.md` §1, disclosed publicly on the Learn page and in `CLAUDE.md`.

There is **no TTL, and no time-based gate on the MESSAGE may be added here.** An OR'd `sent_at < datetime('now','-30 days')` arm used to sit alongside the watermark gate; because it was OR'd it deleted on its own, without consulting a single watermark, so encrypted mail no recipient device had ever collected was destroyed 30 days after it was sent, and any member offline for longer silently and permanently lost messages. That is failure mode **F3**. Age is not evidence of delivery. (The #720 liveness bound is gated on the recipient DEVICE's report time, not the message's age, so it is categorically different — it never deletes an envelope a live device has yet to fetch.)

Every branch of the remaining gate fails closed: a member device that has never reported a watermark breaks the `COUNT(devices) = COUNT(watermarks)` check (the LEFT JOIN yields NULL, which `COUNT` skips), so the CASE returns NULL and `sent_at < NULL` deletes nothing; an empty member-device roster — including one emptied because every device is revoked OR stale — aggregates to `COUNT 0 = COUNT 0` with `MIN(...) = NULL` over zero rows and likewise deletes nothing. An unresolvable or fully-vacated roster is never read as "everybody has collected".

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

**Schema-layer I1 triggers (commit-log-DB migration 000005, #691).** Three
`BEFORE` triggers make a gap/rewrite/head-loss physically unrepresentable, as a
second line of defence behind the CAS insert (`submit_commit`) — the invariant
enforced at the lowest layer, not just the protocol layer:
- `trg_mls_commit_log_no_forward_gap` (INSERT) — rejects `NEW.epoch` above the
  lineage head (`MAX(epoch)+1` within `(conversation_id, generation)`). Kills F1
  (skip-ahead gap). The head append and any `epoch ≤ MAX` resubmit (absorbed by
  the CAS `ON CONFLICT`) pass; only a forward jump aborts.
- `trg_mls_commit_log_immutable` (UPDATE) — rejects any change to the chain
  identity (`conversation_id`/`generation`/`epoch`) or payload (`commit_data`).
  Kills the history-rewrite half of F2. The DS never UPDATEs this table, so it
  has no legitimate traffic to fire on.
- `trg_mls_commit_log_keep_head` (DELETE) — rejects deleting the live head
  (`MAX(epoch)` of `MAX(generation)`) **while any earlier commit of the same
  conversation still exists** (`AND EXISTS (… row with a different `seq` …)`).
  Kills the catastrophic half of F2 (the ELECTRON deletion that wedges every
  member: the head regresses out from under members who had applied it, with
  earlier commits still present that it can no longer reconcile against). The head
  is invariant under BOTH retention delete shapes (`delete_commits_below`,
  floor ≤ head−1; `delete_generations_below`, closed generations only), so those
  never trip it. Interior-gap-on-delete is NOT trigger-enforced — a per-row
  `BEFORE DELETE` trigger cannot tell a mid-chain hole from an intermediate state
  of a legitimate bulk prefix prune — so interior contiguity stays with the CAS +
  the clamped retention floor in Rust. Proven by
  `pollis-delivery/tests/commit_log_triggers.rs` (direct-SQL violation attempts).

  *Full-conversation erasure IS allowed (the `EXISTS` clause was narrowed in the
  #691 review; an earlier draft blocked it and took down 81/82 flows scenarios,
  whose `wipe_remote` reset issues a bare `DELETE FROM mls_commit_log`).* The guard
  fires only on a head-with-siblings delete, so a conversation can be emptied:
  bottom-up (the final row is head-and-only-row → allowed), or via a bare
  `DELETE FROM mls_commit_log` (SQLite deletes in ascending `seq`/rowid order and
  the head holds the highest `seq` in its conversation, so it is visited last,
  after its siblings are gone → the `EXISTS` is false). A future delete-account /
  erase-conversation path therefore needs **no** bypass primitive. Cost, stated
  plainly: buggy code that sweeps an entire *live* conversation is no longer stopped
  at the schema layer — acceptable, since that is whole-conversation removal (not
  I1's head-regression failure mode) and nothing above the DB issues an unscoped
  per-conversation delete (`account.rs` account deletion never touches
  `mls_commit_log`).

  Trigger bodies (`BEGIN … ; END`) carry inner `;` that are not statement
  boundaries, so the migration appliers (`scripts/db-apply.sh`, the flows/tui
  test harnesses' `split_sql_statements`) were taught to keep a `CREATE TRIGGER`
  intact until its `END`.

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
commit-log Turso DB** (`LOG_DB_URL`) post-#420. The Delivery Service holds the
only token of any kind for it — clients used to hold a read-only one, and since
#987 hold none: they read the control plane through
`POST /v1/read/conversation-state` and `POST /v1/welcomes/fetch`. Their migrations are
numbered independently in `pollis-schema/migrations-log/` and applied by the
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
- Written **only** through `POST /v1/security-events`, which attributes the row to the signer — a caller cannot forge an entry under another user. Read back by `list_security_events` for the Security settings page.
- Kinds emitted today: `device_enrolled` (`metadata = via=approval,approver=<device>`), `device_rejected`, `device_revoked` (`metadata = name=<device name at revocation>`, #947), `identity_reset`, `secret_key_rotated`. An unknown kind renders as its raw wire string rather than being dropped, so a newer client's event is never invisible on an older one.
- All of these writes are best-effort by design: the state change they record has already committed, so failing the command on a flaky audit write would report a failure for something that did happen. `revoke_device` posts its event immediately after the tombstone lands and *before* the per-conversation MLS reconcile loop, so the trail does not depend on N commits succeeding — "did the revocation take effect?" is the question that flow produces, and it is asked under pressure.

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

### push_token _(migration 000006)_
- `token` TEXT PK _(one Expo push token per device install)_
- `user_id` TEXT NOT NULL
- `platform` TEXT NOT NULL
- `updated_at` TEXT NOT NULL
- Mobile only — desktop never registers, so the fanout is a no-op for desktop-only users.
- **Content bound:** the notification this routes carries only `{conversationId, kind}` — never plaintext, sender, or any content. Consulted solely by `commands::push::notify_new_message`.
- `token` is the PK so a re-register from the same device upserts rather than duplicating.
- Retention: rows unrefreshed for `POLLIS_DS_PUSH_TOKEN_RETENTION_DAYS` are swept by the DS.

### user_groups / user_dms _(migration 000009 — created, then unused)_
Backfilled-then-stale, unread tables. Created by migration `000009` as the directory index for the per-conversation-DB split (#261 Phase 2). #261 was dropped (not-planned), and the maintenance + reads were reverted — but the migration is append-only history and the tables were already applied to prod/dev/test, so they remain **unreferenced**. Note they are not empty: the migration backfills them from current membership at the bottom of the file, so they hold a frozen snapshot of the roster as of the moment it was applied — stale, and misleading if anyone assumes otherwise. No code writes or reads them. Left in place; a future tightening migration can `DROP` them if desired. They **are** now cleared by account/group teardown — that frozen snapshot is real membership metadata about real users, so it has to go when they do.

---

## Deletion and teardown (`pollis-delivery/src/teardown.rs`)

**Nothing in the DS may rely on `ON DELETE CASCADE`.** Production Turso runs with
`foreign_keys=OFF`, so every `REFERENCES ... ON DELETE CASCADE` in the baseline is
inert on every deployment. This was a live data-retention bug until 2026-08:
`delete_account` ran `DELETE FROM users` and `delete_group` ran `DELETE FROM
groups`, each trusting a cascade that never fired, so the device roster, DM
membership, invites, blocks, preferences, recovery blob, security events, push
tokens, directory-index rows and a deleted group's whole channel + message
history all survived.

Teardown is now enumerated in one module:

| Function | Scope |
|---|---|
| `purge_user_rows(conn, user_id)` | every main-DB table naming a user (caller deletes the `users` row last) |
| `purge_conversation_rows(conn, conversation_id)` | envelopes + the reactions / attachment refs hanging off them + cursors |
| `purge_group(conn, group_id)` | each channel's history, then the group's own rows; returns the dead conversation ids |
| `purge_dm_channel(conn, dm_id)` | a DM with no members left |
| `purge_user_log_rows` / `purge_device_log_rows` / `purge_conversation_log` | the commit-log DB (`mls_commit_since`, `mls_group_info`, `mls_welcome`, `mls_commit_log`) |

Rules that matter:

- **Main-DB teardown is one transaction.** The commit-log DB is a separate Turso
  and cannot join it, so its purges run **after** the main commit — deliberately
  in that order, so a rolled-back teardown can never have already destroyed a
  still-live conversation's MLS state. Leftover rows for a dead conversation are
  recoverable; deleted state for a living one is not.
- **Two tables are exempt, with reasons carried in code**
  (`EXEMPT_FROM_USER_PURGE`): `account_key_log` (published, client-verified,
  append-only) and `mls_commit_log` for a *surviving* conversation (deleting a
  departed member's commits gaps the epoch chain for everyone still in it — the
  baseline's `sender_id ... ON DELETE CASCADE` would do exactly that if FKs were
  ever switched on). `conversation` is append-only by its own design (#880).
- **Adding a table with a `user_id` means adding it to the teardown.**
  `pollis-delivery/tests/teardown.rs` reads the live schema and fails if a table
  naming a user or a conversation is in neither the purge list nor an exemption
  list. It found `mls_commit_since` on its first run — a table nothing had ever
  deleted from.
- **Every delete is scoped to the id being torn down**, and the suite pins that
  deleting one account/group changes not one row of another (the #878 defect
  class).

---

## Local Database (SQLite, per-user, encrypted on every platform)

Source: `pollis-core/src/db/local_schema.sql`

File path: `pollis_{user_id}.db`, encrypted with a key from the device keystore.

**Windows links SQLCipher differently (#992).** SQLCipher's OpenSSL cannot be linked next to the BoringSSL that `libwebrtc-sys` puts in the same image under MSVC (1,253 duplicate symbols), so Windows takes `rusqlite`'s plain `sqlcipher` (linked) feature and gets the library from the `pollis-sqlcipher` workspace crate: the stock SQLCipher amalgamation compiled with `-DSQLCIPHER_CRYPTO_LIBTOMCRYPT`, whose ~20 crypto primitives are implemented over the RustCrypto crates already in the graph. No OpenSSL enters the Windows image at all. The ciphertext is identical to every other platform's — AES-256-CBC pages, HMAC-SHA512, SQLCipher 4 defaults.

Before #992 Windows wrote this file **in the clear**: a second sqlite3 amalgamation from `libsql` had been winning the `sqlite3_*` symbols on every shipped Windows build until #988 removed it, so `PRAGMA key` was a silently-ignored unknown pragma. `LocalDb::open_at` therefore checks every existing file for the unencrypted `SQLite format 3\0` header and, on a hit, overwrites the file and its `-wal`/`-shm`/`-journal` sidecars and recreates the database empty — never opening it, never falling back to plaintext.

Full reasoning: the rusqlite note in `pollis-core/Cargo.toml` and `docs/security-whitepaper.md` §7.0. Pinned by `sqlcipher_is_the_sqlite_we_actually_linked` (now with no `cfg` exception on any platform), `the_local_database_file_is_encrypted_at_rest`, `a_plaintext_database_is_destroyed_not_opened` and `the_crypto_provider_is_the_one_this_platform_documents` (#998) in `pollis-core/src/db/local.rs`. All four run on Linux in `mls-tests.yml`, natively on `windows-latest` in `.github/workflows/windows-link.yml`, and — since #998 — natively on `macos-latest` in `mls-tests.yml`'s `macos-at-rest` job, which is the first macOS runner in this repo to execute a test at all.

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

### read_cursor _(#844)_
- `conversation_id` TEXT PK
- `last_read_at` TEXT NOT NULL
- `last_read_message_id` TEXT NOT NULL
- `updated_at` TEXT NOT NULL DEFAULT now

Where the human has read up to, per conversation. Unread counts are DERIVED from
it against the local `message` table rather than stored as a number — a count
records *how many* without recording *which*, so it cannot be reconciled against
the rows on disk nor merged with another device's idea of the same number. The
position is a pair because `sent_at` alone is sender-supplied and can tie; it is
compared as a row value so it matches the `ORDER BY sent_at DESC, id DESC` the
message list itself uses. The cursor only ever moves FORWARD
(`advance_read_cursor`), which is what makes the cross-device merge a `max()`
with no locking and no conflict resolution. Synced to the other devices as
ciphertext — see [`read_cursor_sync`](#read_cursor_sync-migration-000018-844).

### ui_state
- `key` TEXT PK
- `value` TEXT NOT NULL
- `updated_at` TEXT NOT NULL DEFAULT now

### contact_verification _(account-key TOFU pins)_
- `peer_user_id` TEXT PK
- `account_id_pub` BLOB NOT NULL _(first key ever seen for this peer)_
- `identity_version` INTEGER NOT NULL
- `verified` INTEGER NOT NULL DEFAULT 0 _(1 once the user confirms the safety number)_
- `first_seen_at`, `updated_at` TEXT NOT NULL DEFAULT now
- **Why it is local.** This is the pin that makes a malicious Turso write to `users.account_id_pub` *detectable*. Storing it remotely would let the same attacker rewrite the evidence. Written by `batch_check_and_pin_account_keys` (`commands/safety.rs`) on every group reconcile and DM ingest; a mismatch emits a `KeyChanged` realtime event and clears `verified`.
- The pin is per **user**, not per conversation, so verifying a peer once shields them everywhere they appear.

### mls_generation _(suite-migration lineage, #454 P4)_
- `conversation_id` TEXT PK
- `generation` INTEGER NOT NULL
- `updated_at` TEXT NOT NULL DEFAULT now
- Which suite *generation* of a conversation's MLS group this device is on. MLS binds the ciphersuite into the group, so moving a live conversation to a new suite means a **successor group** restarting at epoch 0 — a second lineage. `GroupId` is the conversation id verbatim at generation 0 and `"{conversation_id}#g{generation}"` after.

### mls_self_update _(PCS sweep bookkeeping, #666)_
- `conversation_id` TEXT PK
- `last_epoch` INTEGER NOT NULL
- `last_at` TEXT NOT NULL
- When this device last rotated its own leaf in this conversation, so the launch-driven sweep can honour `SELF_UPDATE_INTERVAL` (7 days + per-conversation jitter) without a background timer.

### pin_key_cache _(#99)_
- `conversation_id` TEXT PK _(MLS conversation id)_
- `kpin` BLOB NOT NULL _(the 32-byte pin master key, plaintext — this DB is the same trust domain as the MLS state itself)_
- `wrapped_generation` / `wrapped_epoch` INTEGER _(the newest pair this device published a wrap at — the no-network staleness check)_
- `updated_at` TEXT

Load-bearing, not a convenience: `max_past_epochs = 0` destroys the previous
exporter at merge, so a device that lost `Kpin` cannot recover it from a stale
server wrap — only from this row or a peer's fresh re-wrap.

### vault_entry_cache _(local cache, #107)_
- `id` TEXT PK, `content` TEXT NOT NULL _(decrypted entry body — same shape as `message.content`)_, `pinned` INTEGER, `created_at` / `updated_at` TEXT

The decrypted mirror of the remote `vault_message` rows, replaced wholesale by
each sync (the remote is the source of truth) — which is also what lets the
vault render offline. Deliberately NOT named `vault_message`: local and remote
table names stay disjoint so the `no_client_side_remote_writes` guard can tell
the two apart by name alone.

### user_cache
- `id` TEXT PK
- `username` TEXT NOT NULL
- `updated_at` TEXT NOT NULL DEFAULT now
- Username lookup cache so the UI can render a sender without a remote round trip. Not authoritative — `users` in Turso is.

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
