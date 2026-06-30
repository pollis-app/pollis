# READ-ONLY Turso Token Audit — Goal B #419 Slice 3

**Question:** When `config.pollis_delivery_url` is `Some` (prod), every remote
(`state.remote_db`) write from `pollis-core` must go through the Delivery
Service. Any remote write that still executes directly against `remote_db` while
a DS is configured will FAIL under a read-only token and break that feature.

**Branch audited:** `feature/otp-server-bootstrap`
**Scope:** all of `pollis-core/src/`. Local writes (`rusqlite` against
`state.local_db` — the `message`, `user_cache`, `contact_verification`,
`preferences`, `mls_kv` tables) are excluded; they stay client-side and are fine
under a read-only Turso token. Reads (SELECT / `query`) are always allowed.

---

## VERDICT

**NOT ready for a read-only token.** Bucket C is non-empty. There is **one hard,
live blocker** plus several unconditional-direct writes that will error under a
read-only token:

- **BLOCKER (breaks group membership):** `reconcile.rs:557` — the inline
  key-package CLAIM (`UPDATE mls_key_package SET claimed=1 ... RETURNING`) runs
  unconditionally on `remote_db` on the live add path. Under a read-only token,
  *adding anyone to any group or DM fails*.
- **BLOCKER (breaks returning-device login):** `auth.rs:362` — `verify_otp_ds`
  calls the direct-writing `register_device()` for a known device re-login; the
  `INSERT/UPDATE user_device` is `?`-propagated, so login of an
  already-registered device fails hard under a read-only token while a DS is
  configured.
- **Several best-effort unconditional writes** that won't crash a feature but
  *will* error every time under a read-only token (silent failures + log noise +
  stale rows): `auth.rs:875` (session resume), `auth.rs:1112` (logout cleanup),
  `device_enrollment.rs:696` (recovery security-event).
- **One conditional-direct path:** `device_enrollment.rs:235` writes directly
  when DS is configured *but* no `enrollment_session` is held — reachable after
  an app restart between re-login and enrollment.
- **One latent (dead-at-runtime) blocker:** `key_packages.rs:197`
  `fetch_mls_key_package` — still a registered Tauri command, unconditional
  claim, but no internal or frontend caller.

Everything else (all CRUD: groups, channels, invites, join-requests, membership,
DM, messages send/edit/delete, reactions, blocks, push, r2 attachments, profile,
key-package publish/replenish, MLS commit/group-info/welcome, account
delete/revoke, email-change, identity reset) is correctly seamed (bucket A) or
lives in a `*_direct`/legacy fn only reached on the `None` path (bucket B).

---

## THE CRITICAL LIST — bucket C (unconditional / DS-configured direct writes)

| # | file:line | op | table | enclosing fn | severity | fix |
|---|-----------|----|-------|-------------|----------|-----|
| C1 | `commands/mls/reconcile.rs:557-569` | UPDATE … RETURNING | `mls_key_package` | `reconcile_group_mls_impl` | **FATAL — breaks all adds** | DS must own the claim. Add a DS step (server-side claim as part of `/v1/commits`, or a new session/sig-gated `/v1/key-packages/claim` that atomically sets `claimed=1` and returns the KP bytes). The client needs the KP *bytes* to build the Add before it can submit the commit, so a pure "fold into /v1/commits" is not enough on its own — the claim+return must happen before/within submit. |
| C2 | `commands/auth.rs:362` (→ `register_device` 1033/1045/1054) | INSERT…ON CONFLICT / INSERT OR IGNORE | `user_device`, `conversation_watermark` | `verify_otp_ds` (else-branch: known device re-login) | **FATAL — breaks returning-device login** (`?`-propagated) | Route the known-device `last_seen` touch through a DS endpoint, or make it best-effort. The DS already has session-gated `/v1/auth/register-device`; this branch needs an authenticated equivalent (device-signature, since the device has a published cert by re-login time). |
| C3 | `commands/auth.rs:875` (→ `register_device`) | INSERT…ON CONFLICT / INSERT OR IGNORE | `user_device`, `conversation_watermark` | `get_session` | best-effort (err logged, non-fatal) | Same `register_device` direct write. Failure is swallowed, so login still works, but it errors on every session resume under RO (no `last_seen` refresh, noisy log). Route through DS (device-signed) or guard with `if pollis_delivery_url.is_none()`. |
| C4 | `commands/auth.rs:1112-1115` | DELETE | `user_device` | `logout` (`delete_data == true`) | best-effort (`let _ =`) | Stale row left behind under RO (comment already calls it "harmless… overwritten on next login"). Either route through a DS `/v1/devices/...` (but no signing key at logout — see comment) or guard with `is_none()` so it doesn't error under RO. |
| C5 | `commands/device_enrollment.rs:696-702` | INSERT | `security_event` | `recover_with_secret_key` | best-effort (`let _ =`) | Errors under RO (swallowed). Pre-cert, so can't device-sign — defer the audit event until after `finalize_enrollment`/`ensure_device_cert`, or send it through the session-gated path the rest of enrollment uses, or drop it. |
| C6 | `commands/device_enrollment.rs:234-250` | INSERT | `device_enrollment_request` | `start_device_enrollment` (`_` arm of `match (delivery_url, enrollment_session)`) | conditional — fires when DS configured **AND** `enrollment_session` is `None` | The `_` arm catches `(Some(_), None)` as well as `(None, _)`. Split it: `(Some(_), None)` should surface a "session expired, re-login" error, not do a direct write that fails under RO. Reachable after an app restart drops the in-memory `enrollment_session`. |
| C7 | `commands/mls/key_packages.rs:197-208` | UPDATE … RETURNING | `mls_key_package` | `fetch_mls_key_package` | latent — registered Tauri command (`lib.rs:490`), but **no internal or frontend caller** | Same claim problem as C1. Currently unreachable from the UI, but it's an exported `#[tauri::command]`, so it should be removed or routed before the flip to avoid a foot-gun. Its own doc comment says it "folds into `/v1/commits` … a later domain." |

Notes on C1/C7: both are `UPDATE … RETURNING` — i.e. a **write masquerading as a
read**. A read-only token allows the `SELECT` but rejects the `UPDATE`, so they
cannot be "left as reads." C1 is the live path (group/DM membership reconcile);
C7 is dead-at-runtime but still wired as a command.

---

## Bucket B — direct-fallback fns (only reached on the `None` path)

These never run when a DS is configured, so they are **safe at runtime under a
read-only token**, but they are what keeps `config.resend_api_key` and the
write-capable Turso token referenced. List for the Slice-3 secret-drop cleanup:

| file:line | op / fn | what it keeps alive |
|-----------|---------|---------------------|
| `commands/auth.rs:100` `request_otp_direct` (uses `resend_api_key` at `auth.rs:152`) | client-side OTP email send via Resend | `config.resend_api_key` — droppable once DS owns OTP send |
| `commands/auth.rs:432` `verify_otp_direct` (→ INSERT `users` 494, → `generate_account_identity`) | legacy all-client signup | write token |
| `commands/auth.rs:699` `verify_email_change_direct` (UPDATE `users.email` 750) | legacy client email swap | write token |
| `commands/account_identity.rs:203` `generate_account_identity` (UPDATE `users` 222, INSERT `account_key_log` 230, INSERT `account_recovery` 237) | v1 identity bootstrap; only called from `verify_otp_direct:527` + dev_login | write token |
| `commands/account_identity.rs:545-580` `reset_identity` None-branch (UPDATE `users` 551, INSERT `account_key_log` 556, INSERT `account_recovery` 561) | identity rotation fallback | write token |
| `commands/mls/delivery.rs:93` `direct_submit` (INSERT `mls_commit_log` 110, INSERT `mls_group_info` 135, INSERT `mls_welcome` 153) | commit/group-info/welcome mirror | write token |

`turso_token` (`config.rs:6`, used at `state.rs:144`) is the token being
downgraded to read-only; the remote-db connection itself still works for reads.

---

## Bucket A — correctly seamed (`match pollis_delivery_url { Some => DS, None => direct }`)

Every write below runs its direct branch **only** when no DS is configured —
safe under a read-only token. (Direct-write line cited; the seam is the
`match`/`if` immediately above it.)

| file | seam @ | DS endpoint | direct write (None branch) |
|------|--------|-------------|----------------------------|
| `messages/send.rs` | 133 | `/v1/messages/send` | `message_envelope` INSERT 148 |
| `messages/edit_delete.rs` | 116 | `/v1/messages/delete` | `message_envelope` DEL/DEL/INS 127/131/137 |
| `messages/edit_delete.rs` | 205 | `/v1/messages/delete` | `message_envelope` DEL/DEL 216/222 |
| `messages/edit_delete.rs` | 377 | `/v1/attachments/delete` | `attachment_object` DEL 385 |
| `messages/edit_delete.rs` | 506 | `/v1/messages/edit` | `message_envelope` DEL/INS 522/528 |
| `messages/ingest.rs` | 138 / 389 | `/v1/watermarks/advance` | `conversation_watermark` upsert 152 / 403 |
| `messages/ingest.rs` | 164 / 415 | `/v1/envelopes/gc` | envelope cleanup 172 / (DM) |
| `messages/reactions.rs` | 27 / 59 | `/v1/reactions/add`,`/remove` | `message_reaction` INS 40 / DEL 70 |
| `groups/groups.rs` | 122 / 209 / 286 | `/v1/groups/create`,`update`,`delete` | `groups`,`group_member`,`channels` 137-156 / 222-234 / 295 |
| `groups/channels.rs` | 50 / 112 / 191 | `/v1/channels/create`,`update`,`delete` | `channels`,`message_envelope`,`conversation_watermark` 64 / 124-130 / 200-210 |
| `groups/invites.rs` | 79 / 174 / 227 | `/v1/invites/create`,`accept`,`decline` | `group_invite` + (accept→`add_member_to_group`) 90 / 183-186 / 236 |
| `groups/join_requests.rs` | 58 / 208 / 292 | `/v1/join-requests/create`,`approve`,`reject` | `group_join_request` + `add_member_to_group` 68 / 218-220 / 302 |
| `groups/membership.rs` | 95 / 172 / 266 | `/v1/members/remove`,`/v1/groups/leave`,`/v1/members/role` | `group_member`,`groups` 105 / 181 / 277; solo-delete guarded by `is_none()` 212-213 |
| `dm.rs` | 116 / 324 / 385 / 439 / 496 | `/v1/dm/create`,`accept`,`add`,`remove`,`leave` | `dm_channel`,`dm_channel_member`,`message_envelope` 127-158 / 339 / 400-408 / 469 / 507-526 |
| `user.rs` | 52 / 142 | `/v1/profile/update`,`/v1/profile/preferences` | `users` 65 / `user_preferences` 152 |
| `blocks.rs` | 52 / 99 | `/v1/blocks/add`,`/remove` | `user_block` INS 65/76 / DEL 110 |
| `push.rs` | 42 | `/v1/push-tokens` | `push_token` upsert 55 |
| `r2.rs` | 467 | `/v1/attachments/register` | `attachment_object` INS 478 |
| `mls/delivery.rs` | 60 | `/v1/commits` | `direct_submit` (bucket B) |
| `mls/group_state.rs` | 79 | `/v1/group-info` | `mls_group_info` upsert 99 |
| `mls/welcomes.rs` | 129 / 189 | `/v1/welcomes/ack`,`/reset` | `mls_welcome` UPDATE 147 / 209-216 |
| `mls/key_packages.rs` | 159 / 241 / 315 | `/v1/key-packages`,`/replenish` | `mls_key_package` INS/DEL 172 / 255-264 / 327 |
| `mls/device.rs` | 204 / 376 | `/v1/auth/...`/`/v1/devices/resign` | `user_device` UPDATE 254 / 397 |
| `device_enrollment.rs` | 570 / 772 / 896 / 1021 | `/v1/enrollment/approve`,`/v1/account/reset-recover`,`/v1/welcomes/purge`,`/v1/enrollment/reject` | `device_enrollment_request`,`security_event`,`groups`,`group_member`,`mls_key_package`,`user_device`,`mls_welcome` 594-607 / 808-883 / 913 / 1040-1050 |
| `auth.rs` | 1214 / 1462 | `/v1/account/delete`,`/v1/devices/revoke` | `users`,`group_member`,`mls_key_package`,`message_envelope`,`user_device` 1247-1303 / 1468-1473 |
| `auth.rs (verify_otp)` | 179 | `/v1/auth/verify-otp` + session-gated establish/register | bucket-B `verify_otp_direct` |
| `auth.rs (email change)` | 592 / 642 | `/v1/auth/request-email-change-otp`,`/verify-email-change` | bucket-B `verify_email_change_direct` |
| `account_identity.rs (reset)` | 512 / 606 | `/v1/account/rotate-identity`,`/v1/security-events` | bucket-B None-branch / `security_event` INS 622 |

---

## READ-path confirmation (RO-token safe)

All remote `query`/SELECT calls are unaffected by a read-only token. Spot-checked
the read-only remote accesses that prior agents annotated as "stays direct":
`auth.rs:387` `read_account_id_pub`, `auth.rs:678` email-change username mirror,
`auth.rs:833/893` get_session user-existence + enrollment recompute,
`messages/read.rs:266` username backfill, `safety.rs` `fetch_account_key`,
`transparency.rs:127` self-audit, `voice_e2ee.rs:52/202`, `mls/sweep.rs:34`,
`mls/reconcile.rs:398-510` roster/KP/device snapshots,
`key_packages.rs:286` replenish count — all reads, all fine.

The only writes-masquerading-as-reads are the two `UPDATE … RETURNING`
key-package claims (C1, C7) — handled in the critical list above.
