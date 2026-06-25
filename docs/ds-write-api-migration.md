# DS Write-API Migration — Inventory + Design (Slice 3)

**Status:** design (no code changes in this slice)
**Goal:** make the desktop/mobile client **read-only against Turso** and move **every
remote write** behind the Delivery Service (`pollis-delivery`). After this slice the
client connects to Turso for `SELECT` only; all `INSERT`/`UPDATE`/`DELETE` against the
shared DB happen inside the DS, behind authenticated, typed `POST /v1/...` endpoints
that enforce authorization the client cannot bypass.

This document is the **map of the territory** (every remote write site, classified) plus
the **target endpoint design**, the **authorization rule per write**, the writes that
need **serialization/CAS or transactional grouping**, and the **migration order + test
strategy**.

---

## 0. Background: two databases, one of which moves

`pollis-core` carries two databases on `AppState`:

| Field | Backend | Role | This slice |
|---|---|---|---|
| `state.remote_db` | Turso / libSQL (`RemoteDb`, `remote_db.conn()`, `libsql::params!`) | **Shared server DB.** Public metadata + encrypted envelopes. | **Writes move to the DS.** Reads stay direct (read-only). |
| `state.local_db` | per-user SQLite (`rusqlite`, `local_db.lock()` → `db.conn()`, `rusqlite::params!`) | **Device-local secrets.** Plaintext cache, MLS group state, keys. | **Unchanged** — stays client-side. |

**How the two were told apart in the inventory below:** a write is *remote* iff it runs on
a connection obtained from `state.remote_db.conn().await` (async, `libsql::params!`). A write
is *local* iff it runs on `db.conn().execute(...)` from `state.local_db.lock().await` (sync,
`rusqlite::params!`). Example from `messages/send.rs`: the `message` table writes at
`send.rs:78` and `send.rs:122` are **local** (`rusqlite`, `local_db`); the `message_envelope`
write at `send.rs:133` is **remote** (`libsql`, `remote_db`). Only remote writes are in scope.

### What already routes through the DS

The **MLS commit log** already goes through the seam in
`pollis-core/src/commands/mls/delivery.rs::submit_commit`:

- When `config.pollis_delivery_url` is `Some(url)` → `http_submit` → `POST /v1/commits`
  (`pollis-delivery/src/lib.rs:35`, handler in `commit.rs::submit_commit`).
- When `None` (tests / pre-deploy) → `direct_submit` writes `mls_commit_log` directly.

The DS endpoint already serializes commits per conversation race-free / gap-free /
append-only via a single conditional `INSERT ... SELECT ... WHERE based_on = head ... ON
CONFLICT DO NOTHING` (`pollis-delivery/src/commit.rs:1-19, 137-208`). The `SubmitBody`
wire type **already carries** `group_info` and `welcomes` fields
(`pollis-delivery/src/commit.rs:40-46`) — that is the in-progress "make welcomes/group_info
atomic with the commit" stream. **`http_submit` does not yet send them**
(`delivery.rs:110-117` omits both), and the welcome/group_info writes still go direct.

**Everything in §1 except the commit log (domain F, commit row) is what Slice 3 must
migrate.** The welcomes/group_info atomicity work is folding into the same
`/v1/commits` request and is tracked separately; this doc treats the *rest* of the
control-plane and data-plane writes.

---

## 1. Inventory of remote (Turso) writes

Line numbers are approximate (code is read-only this slice; treat file + table +
operation + enclosing command as authoritative, verify line at implementation time).
"Op" is the SQL verb against the **remote** DB. Local-DB writes are excluded.

### A. Messages / envelopes / watermarks / reactions

| # | File | Op | Table | Command / fn | Notes |
|---|---|---|---|---|---|
| A1 | `commands/messages/send.rs:133` | INSERT | `message_envelope` | `send_message` | id, conversation_id, sender_id, ciphertext, reply_to_id, sent_at |
| A2 | `commands/messages/edit_delete.rs` | INSERT | `message_envelope` (`type='edit'`) | `edit_message` | new edit envelope |
| A3 | `commands/messages/edit_delete.rs` | DELETE | `message_envelope` (prior `type='edit'`) | `edit_message` | supersede prior pending edit |
| A4 | `commands/messages/edit_delete.rs` | INSERT | `message_envelope` (`type='delete'` tombstone) | `delete_message` (admin) | |
| A5 | `commands/messages/edit_delete.rs` | DELETE | `message_envelope` | `delete_message` (admin + self) | original + pending edits |
| A6 | `commands/messages/edit_delete.rs` | DELETE | `attachment_object` | `cleanup_attachment` | dedup row for orphaned blob |
| A7 | `commands/messages/reactions.rs` | INSERT OR IGNORE | `message_reaction` | `add_reaction` | id, message_id, user_id, emoji, created_at |
| A8 | `commands/messages/reactions.rs` | DELETE | `message_reaction` | `remove_reaction` | |
| A9 | `commands/messages/ingest.rs` | INSERT…ON CONFLICT | `conversation_watermark` | `ingest_channel_envelopes` | per-device fetch high-water mark |
| A10 | `commands/messages/ingest.rs` | DELETE | `message_envelope` | `ingest_channel_envelopes` | GC envelopes older than 30d / watermark |
| A11 | `commands/messages/ingest.rs` | INSERT…ON CONFLICT | `conversation_watermark` | `ingest_dm_envelopes` | DM watermark |
| A12 | `commands/messages/ingest.rs` | DELETE | `message_envelope` | `ingest_dm_envelopes` | DM envelope GC |

> ⚠️ **Watermark GC (A10/A12) is the subtle one.** Today each reader deletes envelopes it
> believes are consumed. Once writes move to the DS, GC must become a **DS-side** concern
> driven by *all* members' watermarks, not "whatever this one client decided to delete."
> See §2.A and §3.

### B. Groups / channels / membership / invites / join-requests

| # | File | Op | Table | Command / fn | Notes |
|---|---|---|---|---|---|
| B1 | `commands/groups/groups.rs:114` | INSERT | `groups` | `create_group` | id, name, description, owner_id, created_at |
| B2 | `commands/groups/groups.rs:119` | INSERT | `group_member` (`role='admin'`) | `create_group` | creator as admin |
| B3 | `commands/groups/groups.rs:125/133` | INSERT | `channels` | `create_group` | optional default text/voice channels |
| B4 | `commands/groups/groups.rs` | INSERT | `group_invite` | `create_group` | invites for initial recipients |
| B5 | `commands/groups/groups.rs` | UPDATE | `groups` (name/desc) | `rename_group` | + `group_update_log` insert + `group_member.updated_at` bump |
| B6 | `commands/groups/groups.rs` | DELETE | `groups` | `delete_group` | |
| B7 | `commands/groups/channels.rs:49` | INSERT | `channels` | `create_channel` | |
| B8 | `commands/groups/channels.rs` | UPDATE | `channels` (name) + `group_member.updated_at` | `rename_channel` | |
| B9 | `commands/groups/channels.rs` | DELETE | `channels` + `message_envelope` + `attachment_object` | `delete_channel` | cascades envelopes/attachments |
| B10 | `commands/groups/invites.rs:77` | INSERT | `group_invite` | `invite_to_group` | |
| B11 | `commands/groups/invites.rs` | DELETE | `group_invite` | `cancel_invite`, `respond_to_invite` (reject) | |
| B12 | `commands/groups/membership.rs` | INSERT | `group_member` | `respond_to_invite` (accept) | |
| B13 | `commands/groups/membership.rs` | DELETE | `group_member` | `leave_group`, `remove_from_group` | |
| B14 | `commands/groups/membership.rs` | UPDATE | `group_member.role` | `promote_member`, `demote_admin`, `leave_group` (owner handoff) | |
| B15 | `commands/groups/join_requests.rs` | INSERT | `group_join_request` | `create_group_join_request` | |
| B16 | `commands/groups/join_requests.rs` | INSERT | `group_member` | `approve_join_request` | |
| B17 | `commands/groups/join_requests.rs` | DELETE | `group_join_request` | `reject_join_request`, `approve_join_request` | |

### C. Profile / blocks / users / DMs

| # | File | Op | Table | Command / fn | Notes |
|---|---|---|---|---|---|
| C1 | `commands/user.rs` | UPDATE | `users.username` | `set_username` / `update_user_profile` | |
| C2 | `commands/user.rs` | UPDATE | `users.email` | `set_email` | |
| C3 | `commands/blocks.rs:52` | INSERT OR IGNORE | `user_block` | `block_user` | blocker_id, blocked_id |
| C4 | `commands/blocks.rs:63` | UPDATE | `dm_channel_member.accepted_at=NULL` | `block_user` | demote shared DMs to requests |
| C5 | `commands/blocks.rs:84` | DELETE | `user_block` | `unblock_user` | |
| C6 | `commands/dm.rs` | INSERT | `dm_channel` | `create_dm` | |
| C7 | `commands/dm.rs` | INSERT | `dm_channel_member` | `create_dm` | one per participant |
| C8 | `commands/dm.rs` | UPDATE | `dm_channel.accepted_at` | `accept_dm` | conditional `WHERE accepted_at IS NULL` |
| C9 | `commands/dm.rs` | DELETE | `dm_channel_member` | `leave_dm`, `delete_dm` | |
| C10 | `commands/dm.rs` | DELETE | `dm_channel` + `message_envelope` | `reject_dm`, `delete_dm` | |

### D. Key-packages / device registration / device certs

| # | File | Op | Table | Command / fn | Notes |
|---|---|---|---|---|---|
| D1 | `commands/mls/key_packages.rs:96` | INSERT OR IGNORE | `mls_key_package` | `publish_mls_key_package` | ref_hash, user_id, key_package, device_id |
| D2 | `commands/mls/key_packages.rs` | DELETE | `mls_key_package` (unclaimed) | `ensure_mls_key_package` | clear stale unclaimed before refill |
| D3 | `commands/mls/key_packages.rs` | INSERT OR IGNORE | `mls_key_package` (×N) | `ensure_mls_key_package`, `replenish_key_packages` | maintain pool depth (TARGET=5) |
| D4 | `commands/mls/device.rs` | UPDATE | `user_device` (device_cert, mls_signature_pub, …) | `ensure_device_cert` | |
| D5 | `commands/mls/device.rs` | UPDATE | `user_device` (re-sign) | `resign_stale_device_certs` | after identity rotation |
| D6 | `commands/auth.rs` | INSERT OR IGNORE | `user_device` | login / device registration | device_id, user_id, registered_at |
| D7 | `commands/auth.rs` | INSERT | `user_push_token` | `register_push_token` | |

### E. Auth / session / account lifecycle

| # | File | Op | Table | Command / fn | Notes |
|---|---|---|---|---|---|
| E1 | `commands/auth.rs` | INSERT | `users` | `verify_otp` (signup) | id, email, username |
| E2 | `commands/auth.rs` | UPDATE | `user_device.logged_out_at` | `logout` | |
| E3 | `commands/auth.rs` | many (UPDATE/DELETE across `groups`, `group_member`, `user_block`, `dm_channel_member`, `message_envelope`, `users`) | `delete_account` | bulk teardown / anonymize |

### F. MLS control plane — commit / welcome / group_info

| # | File | Op | Table | Command / fn | Status |
|---|---|---|---|---|---|
| F1 | `commands/mls/delivery.rs:64` (direct) / DS | INSERT…ON CONFLICT | `mls_commit_log` | `submit_commit` | **Already behind the DS** (`/v1/commits`) when configured |
| F2 | `commands/mls/group_state.rs` | INSERT | `mls_group_info` | `create_mls_group`, `publish_group_info` | folding into `/v1/commits` (`group_info` field) |
| F3 | `commands/mls/welcomes.rs` / `reconcile.rs:78` | INSERT | `mls_welcome` | `publish_mls_welcome`, `finalize_won_commit` | folding into `/v1/commits` (`welcomes` field) |
| F4 | `commands/mls/welcomes.rs` / `state.rs` | UPDATE/DELETE | `mls_welcome.delivered` | welcome ack / re-arm on local wipe | needs a small DS ack endpoint (§2.D) |

### G. Account identity / recovery / device enrollment / security audit

| # | File | Op | Table | Command / fn | Notes |
|---|---|---|---|---|---|
| G1 | `commands/account_identity.rs` | UPDATE | `users` (account_id_pub, identity_version) | `generate_account_identity`, `reset_identity` | identity rotation |
| G2 | `commands/account_identity.rs` | INSERT | `account_key_log` | same | **append-only transparency log** |
| G3 | `commands/account_identity.rs` | INSERT…ON CONFLICT | `account_recovery` | same | wrapped recovery blob |
| G4 | `commands/account_identity.rs` | INSERT | `security_event` | `reset_identity` | audit |
| G5 | `commands/device_enrollment.rs` | INSERT | `device_enrollment_request` | `start_device_enrollment` | |
| G6 | `commands/device_enrollment.rs` | UPDATE | `device_enrollment_request` (approve) | `approve_device_enrollment` | wrapped_account_key, status |
| G7 | `commands/device_enrollment.rs` | INSERT | `security_event` | enrollment / recovery | audit |
| G8 | `commands/device_enrollment.rs` | many DELETE/UPDATE (`groups`, `group_member`, `dm_channel_member`, `mls_key_package`, `mls_welcome`) | `reset_identity_and_recover` | bulk reset |

**Headline counts (remote write *sites*, ≈):**

| Domain | Sites |
|---|---|
| A. Messages / envelopes / watermarks / reactions | 12 |
| B. Groups / channels / membership / invites | 17 |
| C. Profile / blocks / users / DMs | 10 |
| D. Key-packages / device registration / certs | 7 |
| E. Auth / session / account lifecycle | 3 (one is a large multi-table bulk op) |
| F. MLS commit / welcome / group_info | 4 (F1 already migrated; F2/F3 in flight) |
| G. Account identity / recovery / enrollment / audit | 8 |
| **Total** | **≈61 remote write sites across ~7 domains** |

---

## 2. Target DS endpoint design

### Conventions

- **Base:** all endpoints under `POST /v1/...`. Reads stay client-direct against Turso
  (read-only). The DS is the **sole writer**.
- **Auth (assumed, built by another stream):** every request carries a session token →
  the DS resolves it to an authenticated `caller_user_id` + `caller_device_id`. **No
  endpoint trusts a user_id/sender_id in the body** — the body's actor field must equal
  `caller_user_id`, else `403`. This is the property the client cannot bypass (the whole
  point of read-only Turso).
- **Validation:** the DS revalidates every authorization predicate against the live DB
  inside the same connection/transaction it writes with — never against client-supplied
  state. Membership/role checks are `SELECT ... WHERE` guards in the write statement or a
  checked read immediately preceding it in the same transaction.
- **Idempotency:** mutating endpoints that create rows accept a client-generated ULID as
  the row id (already the pattern) so retries are safe; `INSERT OR IGNORE` / `ON CONFLICT
  DO NOTHING` where a duplicate is benign.
- **Errors:** `403` (not authorized), `404` (target gone), `409` (CAS/serialization
  conflict — caller must re-read and retry), `422` (validation), `200` otherwise.
- **Router:** add routes to `pollis-delivery/src/lib.rs::build_router`; one handler module
  per domain (`commit.rs` is the template). Handlers take `State<AppState>` (= `Arc<Db>`)
  and run the SQL the client used to run, with the auth guard prepended.

### Authorization helpers the DS needs (reused across endpoints)

- `is_group_member(conn, group_id, user_id) -> bool`
- `is_group_admin(conn, group_id, user_id) -> bool`
- `is_group_owner(conn, group_id, user_id) -> bool`
- `is_dm_member(conn, dm_channel_id, user_id) -> bool`
- `channel_group(conn, channel_id) -> Option<group_id>` (to resolve a channel/conversation to its group for membership checks)
- `is_blocked_either_way(conn, a, b) -> bool` (mirror of `blocks.rs:20`)

---

### 2.A. Messages / envelopes / watermarks / reactions

| Endpoint | Body | Auth rule | SQL |
|---|---|---|---|
| `POST /v1/messages` | `{conversation_id, message_id (ULID), ciphertext, reply_to_id?}` | caller must be a member of the conversation's group (channel) or a member of the DM; for DMs, **DS re-checks `is_blocked_either_way` and silently drops** (mirror of `send.rs:45-65`) | `INSERT INTO message_envelope(...)` with `sender_id = caller_user_id` |
| `POST /v1/messages/edit` | `{conversation_id, target_message_id, ciphertext}` | caller is a member **and** owns `target_message_id` (sender) | DELETE prior `type='edit'`, INSERT new `type='edit'` — **one transaction** |
| `POST /v1/messages/delete` | `{conversation_id, target_message_id}` | caller owns the message **or** is admin of the group | self: DELETE own envelope + edits; admin: DELETE + INSERT `type='delete'` tombstone — **one transaction** |
| `POST /v1/reactions` | `{message_id, emoji}` | caller is a member of the message's conversation | `INSERT OR IGNORE INTO message_reaction` with `user_id = caller` |
| `POST /v1/reactions/remove` | `{message_id, emoji}` | caller is a member | `DELETE ... WHERE user_id = caller` |
| `POST /v1/watermarks` | `{conversation_id, last_fetched_at}` | caller is a member | `INSERT ... ON CONFLICT DO UPDATE SET last_fetched_at = MAX(...)` keyed on `(conversation_id, caller_user_id, caller_device_id)` |

**Envelope GC (A10/A12) does NOT become a client endpoint.** Today each client deletes
envelopes it thinks are consumed; with read-only Turso the client can't delete at all, and
even if it could, "this client's watermark" is the wrong signal. **The DS owns GC**: when a
watermark advances, the DS may delete envelopes for that conversation that are below the
**minimum watermark across all current member devices** (and older than the retention floor).
Implement as a step inside `POST /v1/watermarks`, or a periodic DS sweep. This is the only
place where moving writes to the DS *changes behavior on purpose* — and it's the correct
change (a single client GC'ing shared envelopes was already a latent bug per the
"messages must be deliverable to every current member" invariant).

### 2.B. Groups / channels / membership / invites

| Endpoint | Body | Auth rule | SQL |
|---|---|---|---|
| `POST /v1/groups` | `{group_id, name, description?, default_text?, default_voice?}` | any authenticated caller (creating their own group) | **Transaction:** INSERT `groups` (owner_id = caller), INSERT `group_member` (caller, admin), optional channel INSERTs |
| `POST /v1/groups/rename` | `{group_id, name?, description?}` | caller is admin of group | **Transaction:** UPDATE `groups`, INSERT `group_update_log`, bump `group_member.updated_at` |
| `POST /v1/groups/delete` | `{group_id}` | caller is owner | DELETE `groups` (FK-cascade or explicit child deletes — see §3) |
| `POST /v1/channels` | `{group_id, channel_id, name, type}` | caller is admin | INSERT `channels` |
| `POST /v1/channels/rename` | `{channel_id, name}` | caller is admin of the channel's group | UPDATE `channels` + bump watermark |
| `POST /v1/channels/delete` | `{channel_id}` | caller is admin | **Transaction:** DELETE `channels` + `message_envelope` + `attachment_object` for that conversation |
| `POST /v1/invites` | `{invite_id, group_id, invited_user_id}` | caller is admin/member-with-invite-right of group; not blocked either way | INSERT `group_invite` (invited_by = caller) |
| `POST /v1/invites/cancel` | `{invite_id}` | caller is the inviter or a group admin | DELETE `group_invite` |
| `POST /v1/invites/respond` | `{invite_id, accept: bool}` | caller **is** `invited_user_id` of the invite | accept → **transaction** (INSERT `group_member`, DELETE invite); reject → DELETE invite |
| `POST /v1/membership/leave` | `{group_id}` | caller is a member | DELETE own `group_member`; if sole owner, promote another (owner-handoff in same txn) |
| `POST /v1/membership/remove` | `{group_id, user_id}` | caller is admin; target is not last owner | DELETE target `group_member` |
| `POST /v1/membership/role` | `{group_id, user_id, role}` | caller is admin (owner for owner-level changes) | UPDATE `group_member.role` |
| `POST /v1/join-requests` | `{request_id, group_id}` | any authenticated caller; group is joinable; not blocked | INSERT `group_join_request` (user_id = caller) |
| `POST /v1/join-requests/respond` | `{request_id, approve: bool}` | caller is admin of the request's group | approve → txn (INSERT `group_member`, DELETE request); reject → DELETE request |

### 2.C. Profile / blocks / users / DMs

| Endpoint | Body | Auth rule | SQL |
|---|---|---|---|
| `POST /v1/profile` | `{username?, email?}` | caller mutates only their own row | UPDATE `users WHERE id = caller`; username uniqueness enforced by DB constraint → `409` on conflict |
| `POST /v1/blocks` | `{blocked_id}` | caller blocks as themselves; `blocked_id != caller` | **Transaction:** INSERT `user_block` (blocker = caller), UPDATE shared `dm_channel_member.accepted_at = NULL` |
| `POST /v1/blocks/remove` | `{blocked_id}` | caller | DELETE `user_block WHERE blocker_id = caller` |
| `POST /v1/dms` | `{dm_channel_id, member_ids[]}` | caller is among members; **DS checks block both ways** for each pair | **Transaction:** INSERT `dm_channel` (created_by = caller) + one `dm_channel_member` per member |
| `POST /v1/dms/respond` | `{dm_channel_id, action: accept|reject}` | caller is a member of the DM | accept → UPDATE `accepted_at` (conditional); reject → DELETE channel |
| `POST /v1/dms/leave` | `{dm_channel_id}` | caller is a member | DELETE own `dm_channel_member` |
| `POST /v1/dms/delete` | `{dm_channel_id}` | caller is a member (DM is 1:1 or last member) | **Transaction:** DELETE members + `dm_channel` + envelopes |

### 2.D. Key-packages / device registration / certs / welcomes-ack

| Endpoint | Body | Auth rule | SQL |
|---|---|---|---|
| `POST /v1/key-packages` | `{packages: [{ref_hash, key_package}], device_id}` | caller publishes only for their own `(user_id, device_id)`; `device_id` belongs to caller | `INSERT OR IGNORE INTO mls_key_package` (user_id = caller) |
| `POST /v1/key-packages/replenish` | `{packages: [...], device_id}` | same | DELETE stale unclaimed for caller's device + INSERT pool — **one transaction** |
| `POST /v1/devices/cert` | `{device_id, device_cert, mls_signature_pub, cert_*}` | caller owns `device_id`; cert binds caller's identity | UPDATE `user_device WHERE device_id = ? AND user_id = caller` |
| `POST /v1/devices/register` | `{device_id}` | caller registers their own device | `INSERT OR IGNORE INTO user_device` (user_id = caller) |
| `POST /v1/push-tokens` | `{token, device_id, platform}` | caller's device | INSERT `user_push_token` (user_id = caller) |
| `POST /v1/welcomes/ack` | `{welcome_ids[]}` (or `{conversation_id, device_id}`) | caller is the `recipient_id` of those welcomes | UPDATE/DELETE `mls_welcome.delivered WHERE recipient_id = caller` |

> **Key-package claiming caveat:** *claiming* a peer's key package (marking `claimed=1`)
> happens as part of building a commit. With the DS as sole writer this must be a
> DS-side step of the **add path** — fold the claim into `/v1/commits` (the commit that
> adds the device) so the claim and the commit are atomic. Do not expose a raw
> "claim someone else's KP" endpoint.

### 2.E. Auth / session / account lifecycle

| Endpoint | Body | Auth rule | SQL |
|---|---|---|---|
| `POST /v1/users` (signup) | `{user_id, email, username}` | gated by **verified OTP** (the OTP exchange itself is a DS/auth-stream concern); creates the caller's own row | INSERT `users` |
| `POST /v1/session/logout` | `{device_id}` | caller's device | UPDATE `user_device.logged_out_at` |
| `POST /v1/account/delete` | `{}` | caller deletes only themselves | **Large multi-table transaction** (owner-handoff, anonymize `users`, DELETE blocks/dm-members/envelopes). Best run as a **single DS-side stored procedure / one transaction** — do not let the client orchestrate N separate write calls |

### 2.G. Account identity / recovery / enrollment / audit

| Endpoint | Body | Auth rule | Serialization |
|---|---|---|---|
| `POST /v1/identity/rotate` | `{account_id_pub, identity_version, recovery_blob}` | caller; `identity_version` must equal current+1 | **CAS on `users.identity_version`** + **append `account_key_log`** atomically (see §3) |
| `POST /v1/recovery` | `{salt, nonce, wrapped_key}` | caller | `INSERT ... ON CONFLICT(user_id) DO UPDATE` |
| `POST /v1/security-events` | `{event_id, kind, device_id, metadata}` | caller (server-attested fields the DS fills itself) | append-only INSERT `security_event` |
| `POST /v1/enrollment/start` | `{request_id, new_device_id, ephemeral_pub}` | caller starts enrollment for their own account | INSERT `device_enrollment_request` |
| `POST /v1/enrollment/approve` | `{request_id, wrapped_account_key}` | caller is an **already-enrolled** device of the same user; approves by `approved_by_device_id = caller_device` | **Transaction:** UPDATE request + INSERT `security_event` |
| `POST /v1/account/reset` | `{...}` | caller | **Large multi-table transaction** (mirror of `delete_account`) — DS-side procedure |

---

## 3. Serialization / CAS / transactional grouping

**Needs CAS / serialization (compare-and-swap against current DB state, retry on `409`):**

- **`mls_commit_log` (F1)** — already the canonical CAS endpoint (`based_on == head`).
- **`users.identity_version` rotation (G1/G2)** — must be CAS: read current version, write
  `version+1`, **append `account_key_log`**, all in one transaction; reject if another
  rotation raced (`409`). The key-log is an append-only transparency log; a forked/duplicated
  version is exactly the "invalid state" the core invariant forbids. Model it like the commit
  log: `INSERT ... SELECT ... WHERE version = (SELECT identity_version FROM users WHERE id=?)`.
- **`dm_channel.accept` (C8)** — conditional `WHERE accepted_at IS NULL` is already a mild
  CAS; keep that guard server-side.
- **Group owner-handoff on `leave`/`delete`** — last-owner promotion must be serialized so two
  concurrent leaves can't both strip the last owner. Do the read-and-promote in one transaction
  with a `WHERE` guard on current role counts.

**Plain idempotent writes (no CAS — safe to retry, dup is benign):**

- Messages, reactions, watermarks (A1, A7, A8, A9/A11), key-packages (D1/D3), device register
  (D6), push tokens (D7), security events (append-only). Use client ULIDs + `INSERT OR IGNORE`
  / `ON CONFLICT`.

**Must be transactional groups (multiple tables, one request, all-or-nothing):**

- `create_group` (B1+B2+B3+B4), `rename_group` (B5), `delete_channel` (B9),
  invite-accept (B12 + delete invite), join-approve (B16 + delete request),
  `block_user` (C3+C4), `create_dm` (C6+C7…), `delete_dm` (C9+C10),
  `replenish_key_packages` (D2+D3), identity-rotate (G1+G2+G3+G4),
  enrollment-approve (G6+G7), and the two big lifecycle ops `delete_account` (E3) /
  `reset_identity_and_recover` (G8). Each becomes **one DS request running one DB
  transaction**, not N client calls.

> libSQL note: confirm transaction semantics available to the DS handler (BEGIN/COMMIT on the
> `libsql::Connection`, or the `INSERT ... SELECT ... WHERE` pattern for the CAS cases). The
> commit endpoint already proves the conditional-insert approach; the multi-table txns need
> an explicit transaction or batched statement.

---

## 4. Migration order, risk, and test strategy

### Recommended slice order (low-risk → high-risk)

1. **Idempotent leaf writes first** — reactions (A7/A8), watermarks (A9/A11), key-packages
   (D1/D3), push tokens (D7), device register (D6), security events. No CAS, no cross-table
   FK, benign duplicates. These validate the endpoint+auth scaffolding cheaply.
2. **Messages** (A1–A5) — high traffic but each write is contained; edit/delete are small
   transactions. The block re-check (DM send) must move server-side correctly.
3. **Groups/channels/membership** (B*) — cross-table transactions and FK ordering; the
   meat of the migration. Do create/rename/delete, then membership, then invites/join-requests.
4. **DMs/blocks/profile** (C*) — depends on block helper + DM membership checks being on
   the DS already.
5. **Identity/recovery/enrollment** (G*) — needs the CAS key-log machinery; do after the
   pattern is proven on the commit log.
6. **Account lifecycle bulk ops** (E3, G8) — last, because they touch every table and must
   be one transaction; easiest to get wrong, highest blast radius.
7. **Fold welcomes/group_info into `/v1/commits`** (F2/F3) — coordinate with the in-flight
   atomicity stream; add the welcomes-ack endpoint (F4).
8. **Flip Turso to read-only for clients** — only after every domain above is migrated.
   The client's `remote_db` connection should be downgraded to a read-only token; any
   stray write then fails loudly in tests rather than silently bypassing the DS.

### Cross-table / FK risks

- **`delete_*` cascades** (B6 group, B9 channel, C10 DM, E3 account): get child-row delete
  order right inside the transaction, or rely on FK `ON DELETE CASCADE` if the schema has it
  (verify per table — some may not). A partial cascade across two client calls is exactly the
  invalid state read-only-Turso is meant to prevent; one transaction removes the risk.
- **Envelope GC ownership** moving from per-client to DS (A10/A12) — behavior change, must
  preserve the "every current member can still fetch" invariant; GC floor = min watermark
  across current member devices.
- **Key-package claim** is a *peer's* row mutated during add — must fold into `/v1/commits`,
  never a standalone client endpoint.

### Test harness (`src-tauri/tests/flows`) — running the DS in-process

Today the harness builds a `TestWorld` with `RemoteDb::connect_local(test_turso.db)` and a
shared `Config` (`flows/harness.rs:55-85`), and every client's `AppState` writes Turso
directly because `config.pollis_delivery_url` is `None`.

To exercise the DS path **in-process** (no network, deterministic):

1. **Spin up the DS over the same DB file.** `pollis-delivery::build_router(Arc<Db>)` already
   takes an `Arc<Db>` over a libSQL connection (`pollis-delivery/src/lib.rs:32`,
   `Db::connect_local` in `db.rs`). In `TestWorld::setup`, after creating the test Turso DB,
   build a DS router over the **same** local libSQL file and `tokio::spawn` it on an ephemeral
   `127.0.0.1:0` listener (the DS's own tests already `tokio::spawn` submit calls —
   `pollis-delivery/tests/serialize.rs:119`).
2. **Point clients at it.** Set `config.pollis_delivery_url = Some("http://127.0.0.1:<port>")`
   so `submit_commit` and the new write seams route through `http_submit`-style calls instead
   of direct writes.
3. **Inject DS auth in-process.** Since auth maps session→user_id, the harness seeds the DS
   with the test clients' sessions (or runs the DS in a test mode that trusts an
   `X-Test-User` header) so authorization predicates execute for real against test users.
4. **Keep a `direct` mode** for unit-level tests by leaving `pollis_delivery_url = None`
   until a given domain is migrated; flip per-domain as each seam lands, so the same flow
   tests assert identical outcomes on both paths during the transition.
5. **Add a negative-auth suite:** assert that a client cannot write as another user
   (`sender_id`/`user_id` spoof → `403`) and cannot mutate a group it isn't an admin/member
   of — these are the invariants the migration exists to enforce, so they need explicit
   "tries to create the invalid state and is rejected" tests per the core principle.

---

## 5. Top-3 risks

1. **Account-lifecycle bulk ops (E3 `delete_account`, G8 `reset_identity_and_recover`).**
   They touch nearly every table; if migrated as N client calls instead of one DS
   transaction, a mid-sequence failure leaves a half-deleted account (orphaned memberships,
   un-anonymized rows) — precisely the invalid state read-only-Turso is meant to forbid.
   Must be a single server-side transaction; highest blast radius; migrate last.
2. **Identity-rotation transparency log (G1/G2) needs real CAS.** A duplicated or forked
   `account_key_log` / `identity_version` is a correctness violation of the account-key
   transparency guarantee. It must be serialized the way the commit log is
   (`INSERT ... WHERE version = current`), not a plain UPDATE — easy to under-build.
3. **Envelope GC ownership shift (A10/A12).** Moving deletion from "each client deletes what
   it read" to a DS-side floor driven by all members' watermarks is a deliberate behavior
   change that, if mis-scoped, can delete envelopes a slow/offline current member hasn't
   fetched — breaking the "every current member must be able to read their messages"
   invariant. Get the min-watermark-across-current-member-devices floor exactly right and
   cover it with an offline-member test.
