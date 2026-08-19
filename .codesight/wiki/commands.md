# Backend Commands

All backend calls from the frontend use `invoke("command_name", { args })` (imported from `frontend/src/bridge`, which routes to Tauri's `invoke`). Dispatch happens in `src-tauri/src/commands/`: a thin `#[tauri::command]` shim per command — registered in `src-tauri/src/lib.rs`'s `invoke_handler!` — forwards the JSON-shaped call into `pollis-core/src/commands/`. The implementations live in `pollis-core` so a CLI / TUI / mobile binding (uniffi) can call them without any shell-runtime dependency. Edit `pollis-core`, not the shims.

The path in each section header below points at the implementation in `pollis-core`. The `#[tauri::command]` shim under `src-tauri/src/commands/` with the same module name re-exports the types and forwards each command verbatim.

> ### ⚠️ This is a PARTIAL listing — read this before trusting it
>
> **Last verified: 2026-08-03 (#714), against `src-tauri/src/lib.rs` at `d13c906`.**
>
> At that point the app registered **167** commands in `tauri::generate_handler![…]`. The prose sections below describe roughly **half** of them; the rest (voice calls/ringing, camera, screenshare, tray, terminal, reactions, safety numbers, retention/eviction, media permissions, transparency self-audit, sound effects, update gating, clipboard, and several groups/messages commands) have **no prose here at all**. Nothing below has been re-verified line by line beyond the corrections noted in this pass, so treat an entry's *arguments and return shape* as a hint, not a contract.
>
> A **complete index of all 167 registered names** is now appended at the bottom of this
> file, so nothing is invisible — but the *prose* coverage is still partial, and the
> arguments/return shapes below remain a hint rather than a contract.
>
> **The authoritative list is the code, not this file.** To get it:
>
> ```bash
> # every registered command name, in module order
> sed -n '/generate_handler!\[/,/^\s*\]) *$/p' src-tauri/src/lib.rs
> ```
>
> Commands added *after* that verification date are appended to the inventories below as they land, so the inventories run slightly ahead of the **167** figure above; the figure is left pinned to the commit it was actually counted at.
>
> Appendix A at the bottom of this page is a mechanically extracted snapshot of that list as of the date above — names only, no invented descriptions. If a command appears there but not in the prose, read its `pollis-core` implementation; do not assume this page's silence means it does not exist.
>
> When you add or change a command, update the relevant section here **and** Appendix A in the same PR (per `CLAUDE.md`: update the wiki alongside the code).

### The client holds no database credential (#987)

Until #987 the split was "writes through the Delivery Service, reads direct to
Turso": every shipped binary carried a whole-database read token, and 102 call
sites across 34 files opened `state.remote_db` to run their own `SELECT`.
That is gone. **Every remote read is now a `POST /v1/…` on `pollis-delivery`**,
exactly like the writes, and `pollis-core` does not depend on `libsql` at all —
so a client-side query is not a policy violation any more, it is a compile error.

Where the client half lives:

| module | covers |
| --- | --- |
| `pollis-core/src/commands/ds_reads.rs` | directory, groups, DMs, messages, accounts, devices, key packages, enrollment, security events, objects |
| `pollis-core/src/commands/mls/ds_reads.rs` | the MLS control plane — GroupInfo, Welcomes, commit batches, membership gates |
| `pollis-api/src/{reads,directory,account_reads}.rs` | the wire types, shared verbatim with the DS |

Four properties are load-bearing and easy to lose in a refactor:

1. **Reads are POST, not GET.** The canonical signing message covers
   `{METHOD}\n{PATH}\n{TIMESTAMP}\n{hex sha256(body)}` and the PATH excludes the
   query string, so a signed `GET …?user_id=…&device_id=…` would carry
   *unauthenticated* parameters — which is exactly the #681 hole. Putting the
   query in a JSON body makes the signature cover it.
2. **`POST /v1/read/conversation-state` answers from ONE read transaction.**
   GroupInfo, the pending-Welcome flag, the lineage heads, the commit batch and
   the two membership gates come back together because the DS read them
   together. Split across two requests, a device can see a GroupInfo without the
   Welcome written atomically beside it, external-join into its own inbound
   Welcome, and be stranded. Do not add a round trip between them.
3. **Commit batches are contiguous or refused.** `head`, `head_generation` and
   the commits are read inside that same transaction, and
   `pollis_api::reads::check_contiguous` rejects a hole server-side. A hole
   reaching the client makes it delete its MLS crypto state; a genuine retention
   prune is reported as `pruned_below` instead, which is a different thing and
   handled differently.
4. **Failure biases stay at the call sites.** `is_member` and
   `device_registered` are `Option<bool>` on the wire and fail **CLOSED**;
   `welcome_pending` fails **OPEN**. They are seven lines apart in
   `mls/group_state.rs`. Collapsing them into one "false on error" helper
   silently inverts one of them — and HTTP makes those failures common where a
   libsql read failure was rare.

Three reads run with no device signature, because the caller provably has no
credential at that moment (#987 §6.5):

| endpoint | credential | why |
| --- | --- | --- |
| `POST /v1/auth/account-probe` | none (IP rate-limited, tightest tier) | runs pre-PIN at every launch: local DB closed, no signer, no `device_id`, no OTP session. Answers `{ exists, has_identity }` for a 128-bit ULID the caller read out of its own `accounts.json`. |
| `POST /v1/read/enrollment` | possession of `request_id` | the enrolling device has no key (that is what it is enrolling for) and its OTP session is *guaranteed* expired — both TTLs are 600s and the session is minted at `verify-otp`, strictly before the request exists. The payload is sealed to an ephemeral X25519 key, so the id confers a status and an unopenable blob. Asking for the approval columns (`verification_code`) requires a full device signature. |
| `POST /v1/read/recovery-blob` | OTP session | the caller is recovering an account on a device with nothing. Every fetch writes a `recovery_blob_fetched` security event. |

### Every DS write body must NAME ITS ACTOR (#875)

`pollis-delivery` resolves the acting user for every `POST /v1/…` write through one
helper, `writes::resolve_actor`:

| DS `require_auth` | actor comes from | body's actor field |
| --- | --- | --- |
| **on** (the default, and production) | the device-signed `X-Pollis-User` | must EQUAL the signer, else `403` |
| **off** (explicit opt-out only) | the body's actor field | **required** — missing/empty is `403` |

`require_auth` defaults to **ON** since #921: `POLLIS_DS_REQUIRE_AUTH` is an
opt-*out*, only `false`/`0`/`no`/`off` disables it, an unrecognised value (a typo
like `ture`) enforces, and starting with it off logs at `ERROR`. It used to
default off, which was survivable only because the twenty missing actor fields
below made the unsigned path `403` on its own — fixing those in #875 turned "auth
off" into a deployment that genuinely works and trusts whatever actor a caller
names, so the default had to move.

So a client body that omits the field works perfectly against a signed deployment
and hard-`403`s against an unsigned one. The field's NAME varies per endpoint —
`actor_id`, `user_id`, `sender_id`, `requester_id`, `created_by`, `owner_id`,
`approver_id`, `blocker_id`, `inviter_id` — so there is no single global check;
the rule is per call site. Sending it is never a privilege escalation (auth-on
binds it to the signer), and omitting it is always a latent outage.

`current_user_id` (`commands/mls/ds_client.rs`) is the value to send when the
command has no `user_id` parameter of its own — it is the same identity `ds_post`
signs as, so the two cannot disagree.

**You can no longer omit it by accident.** Each body is now a Rust struct in
`pollis-api`, the crate `pollis-delivery` parses the request into, and a struct
literal must name every field — so a forgotten actor is a compile error rather
than an absent JSON key. What the type still cannot check is the *value*: writing
`user_id: None` compiles fine and reintroduces the same outage, so `None` on an
actor field needs a comment saying why.

## The DS write API is one declaration (`pollis-api`)

`pollis-api` holds one `#[derive(Serialize, Deserialize)]` struct per
`POST /v1/…` endpoint plus the path it is addressed at, and BOTH ends are built
from it:

- `pollis-core`'s `ds_post(&state, &body)` takes **no path argument** — the path is
  `DsRequest::PATH`, a property of the body type, so a body cannot be sent to the
  wrong endpoint and a route cannot be misspelled.
- `pollis-delivery` re-exports each module (`pollis_delivery::messages::
  SendMessageBody` still resolves) and routes on `<Body as DsRequest>::PATH`.
- `pollis_api::ENDPOINTS` is the table both route-coverage tests walk
  (`pollis-delivery/tests/endpoint_coverage.rs`, `flows/ds_surface.rs`), so a
  declared endpoint that no router serves fails the build rather than 404ing at
  runtime.

**Adding an endpoint:** add the struct to the matching `pollis-api` module, add one
line to the `endpoints!` table in `pollis-api/src/lib.rs`, bump the count in
`endpoint_coverage.rs::the_endpoint_table_covers_the_whole_write_surface`, and
route it in `pollis-delivery` as `.route(<Body as DsRequest>::PATH, post(handler))`.
The flows harness needs no edit — it mounts the real router.

**What the contract does not cover:** response bodies (callers still derive their
own local structs), field *values*, and version skew against an
already-deployed DS — additive/optional/defaulted wire rules still apply.

The `pollis-api` crate is dependency-light (serde only) on purpose, mirroring
`pollis-schema` and `pollis-device-cert`: sharing it does not make the DS depend
on `pollis-core`.

## auth (`commands/auth.rs`)
- `initialize_identity(user_id)` — ensure MLS credentials + KPs, poll welcomes. Requires the local DB to be open (post-`set_pin` / `unlock`).
- `get_identity()` — check if MLS identity exists locally
- `request_otp(email)` — send OTP code to email
- `verify_otp(email, code)` → `AuthResult` — verify OTP, register the device. Does **not** open the local DB; `set_pin` (signup) or `unlock` (resume) does.
- `get_session()` → `AuthResult | null` — rebuild profile from `accounts.json`. Does not open the local DB.
- `get_device_id()` → `string | null` — this device's stable `device_id` (or null pre-registration). Used to build the per-device voice identity (`voice-{user_id}:{device_id}`) so a client can tell which voice participant is itself (#140). **Not a registered Tauri command** as of 2026-08-03 (#714) — it lives in `pollis-core/src/commands/auth.rs` and is called internally; `invoke("get_device_id")` will fail.
- `logout(delete_data)` — clear session, optionally delete local data
- `delete_account(user_id)` — delete account from Turso + local
- `wipe_local_data()` — delete all local databases and keystore entries

## pin (`commands/pin.rs`)
PIN is cryptographically load-bearing — see `pin-design.md`.
- `set_pin(old_pin?, new_pin)` — initial-set sources from `AppState.unlock` (canonical) or the legacy plaintext keystore slots (upgrade fallback). Wraps both keys, deletes the legacy slots, opens the local DB via `load_user_db_with_key`, publishes the device cert.
- `unlock(user_id, pin)` → `UnlockOutcome` — verify PIN, populate `AppState.unlock`, open the local DB, migrate away any #195-vintage legacy slots, publish the device cert.
- `lock()` — drop `AppState.unlock` and close the local DB. Until the next `unlock`, every DB-touching command fails with "Not signed in".
- `get_unlock_state()` → `{ last_active_user, is_unlocked, pin_set }` — frontend uses this to route between pin-entry, pin-create, and the main app.

## autolock (`commands/autolock.rs`)
Idle auto-lock (#851) — the timer that was missing from `pin::lock`. Rust owns the deadline **and** the timer; the renderer only reports activity and reacts to the resulting event.
- `set_auto_lock_timeout(minutes?)` — install this device's idle window, or `null` for Off (the default). Rejects anything outside `AUTO_LOCK_OPTIONS_MINUTES` (`1, 5, 15, 60`), so a window the UI could never produce is unrepresentable. Resets the deadline and re-arms; changing the setting counts as activity.
- `report_user_activity()` — "a human touched this machine". One lock + one field write; the renderer throttles to at most one call per 15s.

**Why the deadline is not in the renderer.** A WebView throttles (and on some platforms suspends) its own timers once the window is hidden or minimized — exactly when an auto-lock most needs to fire, so a JS `setTimeout` would be reliable only in the case that doesn't matter.

**Not polling.** One task sleeps until the current deadline and re-evaluates on wake; reported activity moves the deadline rather than starting a new timer. A generation counter retires the previous task on every reconfigure, so two timers can never race. `Off` spawns nothing at all.

**An active voice session suppresses it** (`VoiceState.room.is_some() || joining`): `pin::lock` closes the local DB, so locking mid-call would leave the call UI unable to read anything while audio kept flowing. The call is treated as continuous activity and the idle clock resumes when it ends.

**Frontend surface:** Security → "Auto-lock" (`SecurityPage.tsx`), next to PIN because it is the same gate reached without pressing anything. The choice is **device-local** (`localStorage`, keyed by user id — see `frontend/src/utils/autoLock.ts`), like font size and the call ringtone: it describes where a machine physically sits, so syncing it would force one answer onto every device. `useAutoLock` (mounted in `AppShell`) pushes the stored value on mount, feeds throttled activity, and disarms on unmount, so the timer is live exactly while there is decrypted content to protect. On expiry Rust locks, then emits the global `auto-lock` event; `App.tsx` routes it through the **same** `handleLock` Cmd/Ctrl+L uses. That handler's transition into `pin-entry` also calls `queryClient.clear()` — without it every message body the user had read would stay decrypted in the module-level React Query cache, which outlives the unmounted shell.

## user (`commands/user.rs`)
- `get_user_profile(user_id)` → `User`
- `update_user_profile(user_id, username?, email?, phone?)` → `User`
- `search_user_by_username(query)` → `User[]`
- `get_preferences(user_id)` → JSON string
- `save_preferences(user_id, preferences_json)`
- ~~`upload_avatar(user_id, file_data, file_name, content_type)`~~ / ~~`get_avatar_url(user_id)`~~ — **neither exists** anywhere in `pollis-core` or `src-tauri` as of 2026-08-03 (#714). Struck out rather than replaced with a guess: `avatar_url` is a field on the user profile returned by `get_user_profile`; whatever writes it is not one of these two commands. Trace it from `frontend/src/hooks/queries/useUserProfile.ts` before documenting a replacement.

## groups (`commands/groups/`)
- `list_user_groups(user_id)` → `Group[]`
- `list_user_groups_with_channels(user_id)` → `GroupWithChannels[]`
- `list_group_channels(group_id, requester_id)` → `Channel[]` — **#917: members-only.** Took no caller until then, so any group's channel list was readable by anyone who named the group id. Channel names are content-adjacent metadata (`#general` says nothing, `#q3-layoffs` says a lot). Non-member → `you are not a member of this group`.
- `create_group(name, description?, owner_id)` → `Group`
- `create_channel(group_id, name, description?, channel_type?)`
- `send_group_invite(group_id, inviter_id, invitee_identifier)`
- `get_pending_invites(user_id)` → `PendingInvite[]`
- `accept_group_invite(invite_id, user_id)`
- `decline_group_invite(invite_id, user_id)`
- `create_group_invite_link(group_id, creator_id, expires_in_hours?, max_uses?)` → `CreatedInviteLink` — #847. Admin-only. The token is minted **client-side**; only the selector and `sha256(secret)` reach the DS. `token`/`url` are returned **once** and are unrecoverable afterwards — the server stores no secret, so there is no "re-copy" for an existing link. Both bounds `null` = unlimited.
- `list_group_invite_links(group_id, user_id)` → `InviteLinkSummary[]` — admin-only. **#875:** a non-member now errors with "you are not a member of this group" and a non-admin member with "only group admins can view invite links"; both used to return `[]`, which is the same value as "this group has no links". `get_group_join_requests` changed identically. Carries **no token field** by construction. `isLive` is computed server-side with the same `datetime()` normalisation redemption uses.
- `revoke_group_invite_link(link_id, user_id)` — one-way; keeps the first revocation timestamp. Admin of the link's own group, re-derived from the link row.
- `redeem_group_invite_link(token, user_id)` → `RedeemedInvite` — accepts a bare code, an `https://pollis.com/invite/…` URL, or `pollis://invite/…`. Adds the caller via the **same** `add_member_rows` path as invite-accept and join-request-approve, then self-heals into the MLS tree by external commit (a redeemer has no inviter to graft them). Every failure — wrong, malformed, revoked, expired, exhausted — returns the identical `INVITE_LINK_ERR`; only rate-limiting (429) is distinguishable, and only because it describes the caller, not the token.
- `request_group_access(group_id, requester_id)` — creates a pending join request. (Documented here as `request_to_join_group` until #714; that name has never existed in the tree.)
- `approve_join_request(request_id, approver_id)`
- `reject_join_request(request_id, approver_id)`
- `remove_member_from_group(group_id, user_id, actor_id)`
- `leave_group(group_id, user_id)`
- `set_member_role(group_id, user_id, role, requester_id)` — promote/demote; requester must be an admin, `role` is `'admin' | 'member'`. (Was documented as `update_member_role`; corrected in #714.)
- `get_group_members(group_id, requester_id)` → `GroupMember[]` — **#917: members-only.** The headline exposure of that issue: this took no caller at all, so any signed-in user who named a group id got the full roster including usernames and avatar URLs, and ids were enumerable because every client held a whole-DB read-only Turso token (#987 removed that token; the roster is now re-derived server-side by `POST /v1/read/group` before it answers). Non-member → `you are not a member of this group` (an error, not `[]` — same reasoning as #875). (Was documented as `list_group_members`; corrected in #714.)
- `search_group_by_slug(slug)` → `GroupPreview` — finds the group whose name derives to `slug`; **errors** if there is no match (it is not a list-returning search). (Was documented as `search_groups(query)` → `Group[]`; corrected in #714.)
  - **#917: deliberately NOT membership-gated — narrowed instead.** This is the discovery step of the join flow (`SearchGroupPage` → `request_group_access`), so a membership check would delete the feature rather than harden it: you search for a group precisely because you are not in it. The guard is therefore the *shape*. It used to return the whole `groups` row; it now returns `GroupPreview` — `{ id, name, description }`, exactly what the join prompt renders.
  - `owner_id` (names a specific user — the roster leak in miniature) and `created_at` (no consumer) are gone. Member count was considered and **deliberately not added**: no consumer, and a per-group population figure is exactly what a slug-enumerating client would want.
  - `GroupPreview` is a distinct struct rather than a trimmed `Group`, so "which fields may an outsider see" is answered once by the type and a field added to `Group` cannot leak here by accident.
  - Still discloses *whether* a group exists at a guessed slug — inherent to slug lookup, and the same trade Discord's vanity invites and Slack's workspace URLs make. O(rows) per call (`derive_slug` is Rust, not SQL, so it cannot be an indexed lookup), and since #987 it runs on the DS, where it shares the tightest rate-limit tier with `account-probe`: those two are the only endpoints whose INPUT is guessable, so the generic write backstop was the wrong bound for them — exactly as it was for invite redemption (#847). `derive_slug` itself moved to `pollis-api` so client and server derive the same slug from the same code rather than from two copies.
- Also registered but undocumented here: `update_group`, `delete_group`, `update_channel`, `delete_channel`, `get_group_join_requests`, `get_my_join_request`. See Appendix A.
- **#917 — group READS take a caller.** #875 unified the *inconsistent* authz on group writes and admin-gated reads; #917 closed the reads that had *no* authz at all. `get_group_members`, `list_group_channels` and `list_group_emoji` each gained a `requester_id` and an `authz::require_member` call; `search_group_by_slug` kept its open access and lost fields instead (see above); and the DS's `apply_create_join_request` began re-deriving group-existence and not-already-a-member server-side rather than documenting them as client-side. Per-endpoint reasoning lives in the doc comments — the rule was decided per endpoint, not applied as a blanket guard.
  - **The residual #917 named is now closed (#987).** That entry used to end: the client's Turso token was short-TTL and read-only but **whole-DB**, so a user who ignored these commands and ran their own `SELECT * FROM group_member` still read any group's roster — the guards made group metadata *un-served by the app*, not *unreadable*, and closing the gap needed a row-scoped token nobody had built. #987 took the other route and removed the credential entirely: there is no client token to scope, `pollis-core` cannot open a database connection, and `POST /v1/read/group` re-derives membership server-side before it answers. The client-side guards stay as the first chokepoint (a wrong answer should fail here, close to the caller, not only at the DS), but they are no longer the *only* enforcement. See the "bound on every read guard" section in `pollis-core/src/commands/groups/authz.rs`.
- **Authorization preflight (#875):** every group-scoped command routes its membership/role check through `pollis-core/src/commands/groups/authz.rs` (`group_role` / `require_member` / `require_admin` / `require_target_member` / `channel_group_role`). One wording for "not a member" (`you are not a member of this group`), one shape for "not an admin" (`only group admins can {action}`). It is a preflight for the error message, not the enforcement point — the DS re-derives the same role from `group_member` and answers 403 (`pollis_delivery::groups::{group_role, is_admin}`, now also used by `emoji.rs`, which previously kept a copy that swallowed a decode error into a silent deny).

## messages (`commands/messages.rs`)
- `send_message(conversation_id, sender_id, content, reply_to_id?, thread_id?, sender_username?)` → `Message`
- `read_thread_messages(thread_id)` → `Vec<ChannelMessage>` — one thread's replies, oldest-first, local-only (#825)
- `list_thread_summaries(conversation_id)` → `Vec<ThreadSummary>` — reply count + last-reply time per thread root
- `get_channel_messages(user_id, channel_id, limit, cursor?)` → `MessagePage`
- `get_dm_messages(user_id, dm_channel_id, limit, cursor?)` → `MessagePage`
- `read_last_messages(conversation_ids)` → `Vec<ChannelMessage>` — the newest message in each of the given conversations, in ONE call (#874). Drives every sidebar preview row (channels, DMs, DM requests): the previous shape was one `read_channel_messages` / `read_dm_messages` per ROW, re-fired on mount, on window focus and on every `deleted_message` event. A single `ROW_NUMBER() OVER (PARTITION BY conversation_id ...)` over the local `message` table, chunked at 400 bound parameters, plus one batched username hydration. Conversations with no local messages are ABSENT from the result — the caller keys by `conversation_id`, so a null placeholder would be a second spelling of the same answer. Ordering key matches `read_local_channel_page` exactly, so a preview and the top of the opened conversation cannot disagree.
- `edit_message(message_id, conversation_id, sender_id, new_content)`
- `delete_message(message_id, user_id)` — hard-deletes the envelope on Turso + the sender's local row. Both branches (self and admin) build the `POST /v1/messages/delete` body through the one `delete_message_body(...)` helper so `actor_id` cannot be forgotten on either — see "Every DS write body must NAME ITS ACTOR" above; it was missing on both before #875, which made deletion 403 on any `require_auth = false` deployment. If the message had attachments (`_att` in the plaintext JSON payload), each `content_hash` is reference-counted against the sender's other non-deleted local messages; unreferenced ones have their `attachment_object` row + R2 object removed (best-effort, logged on failure). Cross-user references are invisible because attachment metadata lives inside the MLS-encrypted payload — convergent encryption means another member re-uploading the same file simply re-registers the dedup row.
- `search_messages(user_id, query, conversation_id?)` → `Message[]`

## dm (`commands/dm.rs`)
- `create_dm_channel(creator_id, member_ids)` → `DmChannel` — seeds creator's `accepted_at` as now, other members' as NULL (pending request). Rejects with `"message request pending"` if a block exists in either direction with any proposed member.
- `list_dm_channels(user_id)` → `DmChannel[]` — only channels where the caller has accepted (`accepted_at IS NOT NULL`) and neither party has blocked the other.
- `list_dm_requests(user_id)` → `DmChannel[]` — channels where the caller's `accepted_at IS NULL` and no block exists with the other participant(s).
- `accept_dm_request(dm_channel_id, user_id)` — idempotent; flips the caller's `accepted_at` to now. The conversation then appears in `list_dm_channels`.
- `get_dm_channel(dm_channel_id)` → `DmChannel`
- `add_user_to_dm_channel(dm_channel_id, user_id, added_by)`
- `remove_user_from_dm_channel(dm_channel_id, user_id, requester_id)`
- `leave_dm_channel(dm_channel_id, user_id)` — if the last member leaves, channel + envelopes are cleaned up.

## blocks (`commands/blocks.rs`)
- `block_user(blocker_id, blocked_id)` — idempotent. Inserts `user_block` row and resets `accepted_at = NULL` on the blocker's `dm_channel_member` rows for every DM shared with the blocked user (so after unblock those conversations reappear as requests).
- `unblock_user(blocker_id, blocked_id)` — deletes the `user_block` row. DM history becomes visible again and un-accepted channels surface in `list_dm_requests`.
- `list_blocked_users(user_id)` → `BlockedUser[]`
- Enforced in: `create_dm_channel`, `send_message` (DM only — group-channel sends are not gated), `send_group_invite`. All three return the identical string `"message request pending"` so the sender cannot infer which gate rejected them.

## bookmarks (`commands/bookmarks.rs`)

Saved messages and message permalinks (#854). **Every command here is device-local**
— pure rusqlite against the encrypted local DB (`bookmark` table in
`db/local_schema.sql`). None of them touch the DS or Turso, and no DS endpoint backs
any of them.

- `save_message(message_id)` — save a message. The conversation is read from the local
  `message` row, never taken from the caller, so a bookmark cannot point at the wrong
  conversation. Errors if this device does not hold the message. Idempotent
  (`message_id` is the primary key, so a duplicate save is unrepresentable).
- `unsave_message(message_id)` → `bool` — true if a bookmark was removed. Works even
  when the message body is gone, so an unavailable saved item stays removable.
- `toggle_saved_message(message_id)` → `bool` — returns the saved state *after* the
  flip, so the caller never re-reads to know what to render.
- `list_saved_messages()` → `SavedMessage[]` — newest save first, LEFT JOINed to the
  local message body. A bookmark deliberately outlives retention eviction and soft
  delete: those rows come back with `available: false` and `content`/`sender_id`/
  `sent_at` all `null`, rather than vanishing from the list or rendering a blank body.
- `resolve_message_permalink(conversation_id, message_id)` → `PermalinkTarget` —
  resolves `pollis://m/<conversation_id>/<message_id>` **strictly against the local
  database**.

  There is deliberately **no server-side permalink lookup, and none may be added** — a
  "fetch this message by id" endpoint is exactly what would turn a permalink from a
  harmless pointer into a leak. A permalink grants nothing, so forwarding one to a
  stranger is safe.

  Resolution requires the message to exist locally **and** to be in the conversation
  the link claims, so a caller cannot pair a real message id with a guessed
  conversation id to confirm where it lives. Every failure mode — unknown message,
  evicted message, mismatched conversation — returns the byte-identical
  `{ found: false, conversation_id: null, message_id: null }`, so the command cannot be
  used as an existence oracle.

  A device that was not in the MLS group when the message was sent never received the
  key material, so it never decrypted the envelope and never wrote it locally. The miss
  is therefore *cryptographic*, not a permission check — accepted loss (1) in
  `CLAUDE.md`.

## mls (`commands/mls/`)
Only three MLS commands are registered (`src-tauri/src/commands/mls.rs`):
- `process_pending_commits(conversation_id, user_id)`
- `poll_mls_welcomes(user_id)`
- `catch_up_all_mls_groups(user_id)`

Corrected 2026-08-03 (#714):
- ~~`reconcile_group_mls(conversation_id, actor_user_id)`~~ — **not a command**. Reconcile is internal: `pollis_core::commands::mls::reconcile_group_mls_impl`, driven by the sweep in `commands/mls/sweep.rs` and by `auth.rs`. It has no shim and no `generate_handler!` entry.
- ~~`generate_mls_key_package(user_id)`~~ — **does not exist** anywhere in the tree under that name.

## device_enrollment (`commands/device_enrollment.rs`)
Every path that produces a fresh `account_id_key` (signup, approval, Secret-Key recovery, identity reset) hands the bytes to `AppState.unlock` — never to the keystore unwrapped. The frontend then routes to pin-create; `set_pin` wraps under the user's PIN and opens the local DB.
- `start_device_enrollment(user_id)` → `EnrollmentHandle`
- `poll_enrollment_status(request_id)` → `EnrollmentStatus`. On `Approved`, populates `AppState.unlock`; defers cert / KP / external-join to `finalize_device_enrollment`. The row read is `POST /v1/read/enrollment`, gated on possession of `request_id` (#987 — see "The client holds no database credential" above): this runs on a device with no local DB, no keystore entry and an already-expired OTP session, so the capability is the only credential that exists. It superseded a `RemoteDb::with_retry` read (#874) whose purpose — surviving a dropped libsql stream rather than stranding the user on a spinner — is now the HTTP client's problem.
- `await_enrollment_approval(request_id)` → `EnrollmentStatus` — resolves once the request reaches a terminal state (#874). What the renderer calls; it replaced a 2-second `setInterval` on `poll_enrollment_status`, which CLAUDE.md bans. The new device is pre-enrollment and cannot subscribe to realtime, so the answer genuinely has to be fetched — but the waiting is the backend's, with 1s→8s exponential backoff and a hard stop at the request's own 10-minute TTL, so it cannot outlive the thing it waits on. The renderer awaits one promise and ignores late answers from superseded requests via a generation counter.
- `approve_device_enrollment(request_id, user_id, verification_code)`
- `reject_device_enrollment(request_id, user_id)`
- `list_pending_enrollment_requests(user_id)` → `PendingEnrollmentRequest[]`
- `recover_with_secret_key(user_id, secret_key)` — same handoff pattern as the approval path.
- `reset_identity_and_recover(user_id, email)` — soft recovery; `reset_identity` populates `AppState.unlock` with the new keypair before this command's local-DB cleanup runs.
- `finalize_device_enrollment(user_id)` — call after `set_pin` completes. Publishes the device cert + a fresh MLS key package, then external-joins every existing group / DM the device isn't in yet. Idempotent for fresh signup.
- `list_user_devices(user_id)` → `DeviceInfo[]`
- `is_current_device_registered(user_id)` → `bool`. The authoritative re-check behind the `device_revoked` inbox nudge: the nudge is per-user so it reaches every device, and only the one that answers `false` signs out (a forged nudge can't evict a valid device). `false` requires an ACTIVE row — revocation is a tombstone, so this filters `revoked_at IS NULL` (#685). Returns `true` when the device id isn't known yet, so early boot never self-logs-out.
- `reset_identity(user_id)` → new secret key. **Not a registered Tauri command** as of 2026-08-03 (#714): it is `pollis_core::commands::account_identity::reset_identity`, reached from the registered `reset_identity_and_recover`.
- Also registered in this module but undocumented here: `list_security_events`. See Appendix A.

## livekit (`commands/livekit/`)
- Tokens are minted by the DS now (#393) — no on-device signer. `get_livekit_token` and friends call `ds_livekit_token` (`POST /v1/livekit/token`); server-side fan-out/roster go through `ds_livekit_send_data` / `ds_livekit_participants`. The client holds no LiveKit API secret.
- `get_livekit_token(room_id, user_id, username)` → token string (identity/name derived server-side; the args are ignored)
- `subscribe_realtime(on_event: Channel)`
- `connect_rooms(room_ids, user_id, username)`

## voice (`commands/voice/`)
**Participant identity** is per-device: `voice-{user_id}:{device_id}` (device_id from `AppState.device_id`; falls back to legacy `voice-{user_id}` pre-login). This lets two devices of the same user coexist in one room instead of colliding on the SFU (#140) — mirrors the realtime/inbox scheme in `livekit/realtime.rs`. Parse the user back out with `voice::user_id_from_voice_identity` (Rust) / `voice/identity.ts` (frontend). The voice event loop **does not play back** audio tracks whose parsed user_id is the local user's (self-hear mute) but still shows them as participants. `RoomEvent::Disconnected` now fully tears down the room + mic stream via `release_voice_resources` (previously leaked).
- `prepare_voice_connection(channel_id, user_id, display_name)` — best-effort warmup fired on user "intent" (hover, route entry). Pre-fetches + caches the DS-minted voice token (identity derived server-side, so it matches the join) and runs a one-shot HTTPS probe to warm DNS / TLS / connection pool. Idempotent; safe to call eagerly. Consumed by the next `join_voice_channel` for the same channel + identity.
- `join_voice_channel(channel_id, user_id, display_name, input_device, output_device, audio_processing)` — connect to LiveKit and publish the local mic. `audio_processing` is the `ApmConfig` struct (AGC + NS + AEC settings) — see [Audio Processing](./audio-processing.md). Consumes a fresh warmup if present and runs `Room::connect` + cpal mic init concurrently to minimise cold-start latency.
- `leave_voice_channel()`

**Transmit gate — push-to-talk, mute, deafen (#849).** All local gating decisions live in one pure state machine, `voice::TransmitGate` (`commands/voice/gate.rs`). The commands below are thin wrappers: each applies one transition, then republishes the result to the three places that consume it — the `transmit_muted` atomic read by the cpal capture callback, the `deafened` atomic read by the playback mixer, and the LiveKit publication's mute state (so remote tiles show the truth). They all return the same `VoiceGateState` snapshot `{ mode, self_muted, deafened, ptt_held, transmitting }`, mirrored in TS as `VoiceGateState` in `frontend/src/types/voice-state.ts`.

Invariant: **`deafened ⇒ self_muted`** — the gate's fields are private and unmuting while deafened also undeafens, so the contradiction is unconstructible. Undeafen restores the mute state deafen displaced; it never blindly unmutes.

- `toggle_voice_mute()` → `VoiceGateState` — **returns the full gate snapshot, not a bool** (changed in #849): a toggle can clear deafen as a side effect, so the renderer must be told the whole outcome.
- `toggle_voice_deafen()` → `VoiceGateState` — silences all *incoming* audio (the mixer zeroes its mixed frame after draining, so undeafening resumes at the live edge) and implies self-mute.
- `set_voice_input_mode(mode)` → `VoiceGateState` — `"voice_activity"` (default; mic open whenever unmuted) or `"push_to_talk"`. Persisted by the renderer in the preferences blob and pushed on load as well as on change, so it is valid to call outside a call. Always drops any held PTT latch.
- `set_voice_ptt_held(held)` → `VoiceGateState` — push-to-talk key down/up.
- `release_voice_ptt()` → `VoiceGateState` — same transition as `set_voice_ptt_held(false)`, named for its reason: the window lost keyboard focus, so the matching keyup will never arrive and the mic must not be left hot. The renderer calls it from `window.blur` / `visibilitychange` / `pagehide`.
- `get_voice_gate_state()` → `VoiceGateState` — read without mutating; used to resync after a renderer reload.

- `set_voice_input_device(device_name)` / `set_voice_output_device(device_name)` — switch device mid-call. Input switch rebuilds APM if the new device's sample rate differs.
- `set_voice_audio_processing(config)` — push live APM config (AGC target, NS level, AEC on/off) without rejoining. Internal echo / noise / AGC state is preserved; only the changed submodule re-initialises.
- `subscribe_voice_events(on_event: Channel)`
- `list_audio_devices()` → `AudioDevice[]`
- `get_last_join_timings()` — debug: most recent `JoinTimings` record (jwt, room connect, mic init, first publish, total).

## r2 (`commands/r2.rs`)
- `upload_file(data, key, content_type)` → URL — caller-chosen key. Legacy; new public objects should use `upload_public_file`.
- `download_file(key)` → bytes — returned over the JSON IPC as an array of integers (~3x inflation). Now only the FALLBACK path for legacy non-content-addressed keys; see `get_public_file_url`.
- `upload_public_file(prefix, data, content_type)` → `UploadResult` — uploads an avatar / group icon under a CONTENT-ADDRESSED key `{prefix}/{sha256}.{ext}` (#874). Avatars used to live at a stable `avatars/{user_id}` overwritten in place, and a mutable key is what made a local cache impossible: nothing short of re-fetching could tell you whether a cached copy was current, so every viewer re-downloaded every avatar on every launch. A new avatar is now a new key, which the profile row already publishes.
- `get_public_file_url(key)` → URL — resolves a public object to a loopback media-server URL, caching the bytes on disk encrypted at rest under the same content-addressed scheme attachments and custom emoji use. The key IS the expected digest, so the integrity check is free and mandatory (these objects are stored unencrypted). Returns `""` — the same sentinel `get_media_url` uses — for a LEGACY key that carries no digest, or when the media server isn't up; the frontend falls back to `download_file` for those.
- ~~`presign_upload(key, content_type)`~~ — **does not exist** under that name as of 2026-08-03 (#714). Presigning is server-side: the DS secrets broker mints URLs via `POST /v1/r2/presign` (see `delete_r2_object` below and `docs/secrets-broker.md`).
- `get_media_url(r2_key, content_hash, content_type)` → URL — builds the loopback media-server URL (`http://127.0.0.1:<port>/<token>/<hash>`) for a cached item; errors if the server isn't started or there is no active unlock. See `pollis-core/src/media_server.rs`.
- `upload_media(path, filename, content_type)` / `download_media(r2_key, content_hash)` — convergent-encryption media path; dedups via `attachment_object` on Turso.
- Internal: `delete_r2_object(state, r2_key)` — DS-presigned DELETE (via `presign_r2`) used by `delete_message` to purge orphaned attachments. Treats 404 as success. The client holds no R2 credentials — every get/put/delete is presigned by the DS secrets broker (`POST /v1/r2/presign`, #393).

## emoji (`commands/emoji.rs`)
Custom per-group emoji (#848). Remote metadata lives in `custom_emoji_object` (content-addressed, one row per stored image) + `group_emoji` (one row per group that uses it); writes go through the DS (`POST /v1/emoji/{create,remove,gc}`). The objects are **unencrypted** in R2, deliberately — see the module docs and migration `000015_custom_emoji.sql`. `create` names its actor as `created_by` and `remove` as `actor_id` (both filled from `current_user_id`) — neither did before #875, so both 403'd on an unsigned deployment; see "Every DS write body must NAME ITS ACTOR" at the top of this page.
- `list_usable_emoji(user_id)` → `CustomEmoji[]` — every custom emoji the user may SEND, i.e. every emoji registered to any group they are a current member of. Discord's rule: usable in ANY conversation, not just the registering group. This is both the picker's source and the permission predicate — deliberately one query, so the set offered and the set permitted cannot drift.
- `list_group_emoji(group_id, requester_id)` → `CustomEmoji[]` — one group's emoji (the management page). **#917: members-only**, and this does *not* contradict #848. #848 requires that an emoji from a group you are in none of still **renders**; that is discharged entirely by `get_emoji_url`, which takes a content hash, no group, and no membership check. *Resolving one hash you were handed* and *enumerating a group's emoji set* are different acts — the second is group metadata (shortcodes name things, `created_by` names members). So the hash stays open and the list closes, the narrowest rule that keeps cross-group rendering intact. The picker was never affected: `list_usable_emoji` has always joined through `group_member`.
- `upload_group_emoji(group_id, shortcode, path)` → `CustomEmoji` — reads the file from disk (no bytes over IPC), **re-encodes** it to WebP/GIF under 48 KiB (`encode_emoji`), content-hashes the *re-encoded* bytes, skips the R2 PUT on a dedup hit, uploads under a length-bound presign, then registers via the DS. `shortcode` must be `[a-z0-9_]{2,32}`.
- `remove_group_emoji(group_id, shortcode)` — DS remove (creator or group admin), then an event-driven GC sweep that deletes any now-unreferenced R2 blob.
- `get_emoji_url(content_hash)` → URL — loopback media-server URL for rendering. The content type is looked up from `custom_emoji_object` rather than passed in — the wire token carries only the hash, so a caller would have had to guess, and guessing wrong derives the wrong R2 key. Fetches the plain object, **verifies `sha256(bytes) == content_hash` and the size ceiling before caching**, then writes into the shared media cache so `media_server.rs` serves it unchanged. Rendering is unauthorized by design — a user in none of the registering groups must still see the emoji. The mobile uniffi bridge (`pollis-core/src/bridge.rs`) exposes `get_emoji_path(content_hash, dest_dir)` instead — no loopback server exists in a sandboxed RN app, so it materialises the verified bytes to a sandbox `file://` path (mirroring `get_media_path`); both share the fetch/verify core `emoji::download_verified_emoji`.
- `prepare_emoji_text(user_id, text)` → `string` — strips `<:shortcode:hash>` tokens the user is not permitted to use, leaving plain `:shortcode:`. The "use" rule cannot be enforced server-side (message content is E2EE, so the DS never learns which emoji a message carries), so it is enforced on the sender's device against the same predicate that fills the picker. Degrades rather than refuses: a message is not worth failing over an emoji.

## overlay (`commands/overlay.rs`)
Runtime application of the closed-overlay relay mode (design `docs/relay-overlay-design.md` §14). Off-by-default; when off no shim runs and every network path is byte-for-byte the direct path.
- `get_overlay_mode()` → `"off" | "prefer" | "strict"` — the CURRENT live mode (the running shim's mode, or `off` when no shim).
- `set_overlay_mode(mode)` — parse + APPLY LIVE. Off→non-off builds the `RealRelayFactory` and starts the loopback SOCKS5 shim; publishing the handle IS the routing, because every overlaid caller reads `AppState::overlay_handle()` on its hot path. non-off→Off drops the handle, which aborts the accept loop and returns every caller to direct; Prefer↔Strict flips the shim's policy mode live (no restart). #987 removed the libsql half of this: there are no database handles to reconnect, and the only overlaid protocol left is HTTPS (the DS and R2), which rides reqwest's own SOCKS support. Idempotent — safe to call on every app start / login (the UI calls it after loading the saved preference; boot calls the same `apply_overlay_mode` with `POLLIS_OVERLAY`). On failure to bring the overlay up it rolls back to the previous working state and errors — never a half-routed app; Strict degrades (surfaced error) rather than silently going direct. Config: `POLLIS_OVERLAY` (mode), `POLLIS_OVERLAY_RELAY` (comma-sep relay endpoints — a POOL: `RealRelayFactory` tries them in health order and fails over to the first success, marking a failed relay dead for a cooldown, fail-open if all dead, single-hop), `POLLIS_OVERLAY_RELAY_CERT` (path or base64 DER of the pinned relay QUIC leaf). Persisting the choice is the settings-UI slice's job.
- **Frontend surface:** Preferences → "Network privacy (relay)" (`PreferencesPage.tsx`) — an Off/Prefer/Strict `radiogroup`. `overlay_mode` lives in `PreferencesData` (`usePreferences.ts`, synced blob, default `off`); selecting a mode persists it (`save_preferences`) and applies it live (`set_overlay_mode`); on login/restart the saved mode is re-applied once, guarded by a `get_overlay_mode` diff so an unchanged mode never restarts the shim. An apply error (e.g. Strict with no relay) surfaces inline and the control reflects the real resulting mode from `get_overlay_mode` — never a silent direct.

## relay_serving (`commands/relay_serving.rs`)
The mirror image of `overlay` (design §10.2, #813): `overlay` decides whether *your* traffic goes through the overlay, this decides whether *other people's* traffic goes through *your* device. Neither implies the other, and this one is off by default behind explicit consent. Engine: `pollis-core/src/net/peer/`.
- `get_relay_serving_status()` → `RelayServingStatus` — the CURRENT live state, the same role `get_overlay_mode` plays for the mode control. Shape (serde snake_case, mirrored by `frontend/src/hooks/queries/useRelayServing.ts`): `{ config: { enabled, wifi_only, power_only }, state: "off" | "serving" | "waiting" | "unsupported", hold: "metered_network" | "on_battery" | "offline" | "no_inbound_path" | null, on_wifi: bool | null, on_power: bool | null, active_circuits, bytes_forwarded }`. `hold` is non-null exactly when `state == "waiting"`. `on_wifi`/`on_power` are `null` when the platform cannot report them (Linux reads sysfs/procfs, macOS shells out to `pmset`/`route`, Windows reports neither yet) — and **a null signal never satisfies a condition**: if `wifi_only` is set and we cannot tell whether the link is unmetered, the device does not relay. Reading the status re-probes and reconciles, which is how a change (charger unplugged) gets noticed without a poll (CLAUDE.md bans polling).
- `set_relay_serving(config)` → `RelayServingStatus` — apply live, idempotent, and it returns the status it **settled on**, not an echo of the request: consent given while on battery settles to `waiting` + `on_battery`. Consent plus the user's own conditions decide whether the relay node runs; reachability decides whether the state is `serving`, so a device nothing can reach honestly reports `waiting` + `no_inbound_path` while the node runs and tries. `unsupported` is the answer on platforms that cannot serve at all (mobile). Persisting the choice is the settings-UI slice's job.
- **Event `relay-serving-status`** — carries a `RelayServingStatus` whenever the state, a platform signal, or the live circuit count changes. Wired in `src-tauri/src/lib.rs` setup via `commands::relay_serving::install_status_sink`. Required rather than optional: without it the status line only refreshes on remount.
- **How a device becomes reachable (`net::peer::park`, #813 wave 3).** A peer never listens for inbound connections, so reachability is something it earns by dialling **out**: on consent it opens one QUIC connection to **every** first-party relay in the signed directory, runs the ordinary device handshake (`proto::verify_handshake` — no new auth), registers with a `Park` frame carrying its own relay leaf DER, and leaves the connection parked. Every stream a relay then opens on that connection is an `Extend` being spliced into this device: it becomes a `PeerLink` and goes straight to the loopback engine, so nothing about the onion wire changes — only who dialled whom. `no_inbound_path` therefore means what it says (nothing can reach this device *right now*) and clears the moment one parked connection is live. Losing a parked connection re-dials on exponential backoff driven by the drop itself, never by a sweep. `src-tauri/src/lib.rs` setup calls `commands::relay_serving::attach_app_state` to supply the directory + device identity; without it the node runs and parks nowhere.
- **A serving peer's own revocation store is directory-backed.** `PeerEngine` is handed a `RevocationStore` keyed on the same pinned `POLLIS_OVERLAY_DIRECTORY_KEY` the directory is verified with, kept current alongside the directory refresh. A device with no directory key gets `RevocationStore::unconfigured()`, which admits nothing — so it runs a node, parks nowhere, and forwards nothing. Being unable to evaluate revocation is a refusal to extend, never a default-open.
- **What a relaying device can do is bounded structurally**, not by policy: the node runs with an **empty destination allowlist** (so a `Connect` frame — the only frame naming a destination — is refused before any socket is opened; a peer can never be a circuit's exit) and listens on **loopback only** (no new externally-reachable port; traffic arrives over a link the device opened outbound). Path selection additionally refuses a peer at the guard or exit position (`net::peer::discovery`).
- **Frontend surface:** Preferences → "Run a relay for others" (`PreferencesPage.tsx`). Consent + both conditions persist as three flat booleans in the synced prefs blob (`relay_serving`, `relay_serving_wifi_only`, `relay_serving_power_only`; defaults off / true / true). Every toggle calls `set_relay_serving`, as does login/restart once, guarded by a config diff against the live status. A failed call does **not** revert the toggle (consent is the user's statement, not the backend's) — the status line is the single place that says what is actually true.

---

## Appendix A — full registered-command inventory (names only)

Mechanically extracted from `tauri::generate_handler![…]` in `src-tauri/src/lib.rs` at `d13c906` on **2026-08-03** (#714), plus the two `relay_serving` commands added by #813, the six `emoji` commands added by #848, the two `messages` receipt commands added by #857, the two `autolock` commands added by #851, and the four added by #874 (`messages::read_last_messages`, `r2::upload_public_file`, `r2::get_public_file_url`, `device_enrollment::await_enrollment_approval`). **200 commands.** Grouped by the `commands::<module>::` path used at the registration site; **names only — no descriptions are given here because they were not verified.** A name in this list that has no prose above is real and callable; read its implementation in `pollis-core/src/commands/` before using it.

Regenerate with:

```bash
sed -n '/generate_handler!\[/,/^\s*\]) *$/p' src-tauri/src/lib.rs
```

- **`(lib.rs, no module)`** — `hide_window`, `read_clipboard_files`, `read_clipboard_image_to_temp`
- **`autolock`** — `report_user_activity`, `set_auto_lock_timeout`
- **`auth`** — `delete_account`, `dev_login`, `get_identity`, `get_session`, `initialize_identity`, `is_current_device_registered`, `list_known_accounts`, `list_user_devices`, `logout`, `request_email_change_otp`, `request_otp`, `revoke_device`, `verify_email_change`, `verify_otp`, `wipe_local_data`
- **`blocks`** — `block_user`, `list_blocked_users`, `unblock_user`
- **`bookmarks`** — `list_saved_messages`, `resolve_message_permalink`, `save_message`, `toggle_saved_message`, `unsave_message`
- **`camera`** — `list_video_devices`, `start_camera`, `start_camera_preview`, `stop_camera`, `stop_camera_preview`, `subscribe_camera_events`
- **`device_enrollment`** — `approve_device_enrollment`, `await_enrollment_approval`, `finalize_device_enrollment`, `list_pending_enrollment_requests`, `list_security_events`, `poll_enrollment_status`, `recover_with_secret_key`, `reject_device_enrollment`, `reset_identity_and_recover`, `start_device_enrollment`
- **`dm`** — `accept_dm_request`, `add_user_to_dm_channel`, `create_dm_channel`, `get_dm_channel`, `leave_dm_channel`, `list_dm_channels`, `list_dm_requests`, `remove_user_from_dm_channel`
- **`emoji`** — `get_emoji_url`, `list_group_emoji`, `list_usable_emoji`, `prepare_emoji_text`, `remove_group_emoji`, `upload_group_emoji`
- **`groups`** — `accept_group_invite`, `approve_join_request`, `create_channel`, `create_group`, `create_group_invite_link`, `decline_group_invite`, `delete_channel`, `delete_group`, `get_group_join_requests`, `get_group_members`, `get_my_join_request`, `get_pending_invites`, `leave_group`, `list_group_channels`, `list_group_invite_links`, `list_user_groups`, `list_user_groups_with_channels`, `redeem_group_invite_link`, `reject_join_request`, `remove_member_from_group`, `request_group_access`, `revoke_group_invite_link`, `search_group_by_slug`, `send_group_invite`, `set_member_role`, `update_channel`, `update_group`
- **`install_kind`** — `detect_managed_install`
- **`livekit`** — `cancel_call`, `connect_rooms`, `get_livekit_token`, `get_livekit_url`, `get_livekit_view_token`, `list_voice_participants`, `list_voice_room_counts`, `publish_ping`, `publish_typing`, `publish_voice_presence`, `start_call`, `subscribe_realtime`
- **`media_permissions`** — `get_media_permission_status`, `open_privacy_settings`, `revoke_media_permissions`, `set_revoke_media_on_exit`
- **`messages`** — `add_reaction`, `delete_message`, `edit_message`, `get_channel_messages`, `get_dm_messages`, `get_message_retention`, `get_reactions`, `ingest_channel_envelopes`, `ingest_dm_envelopes`, `list_messages`, `list_thread_summaries`, `read_channel_messages`, `read_dm_messages`, `read_last_messages`, `read_thread_messages`, `remove_reaction`, `run_message_eviction`, `search_messages`, `send_message`, `set_message_retention`
- **`mls`** — `catch_up_all_mls_groups`, `poll_mls_welcomes`, `process_pending_commits`
- **`overlay`** — `get_overlay_mode`, `set_overlay_mode`
- **`pin`** — `get_unlock_state`, `lock`, `set_pin`, `unlock`
- **`r2`** — `download_file`, `download_media`, `get_media_url`, `get_public_file_url`, `upload_file`, `upload_media`, `upload_public_file`
- **`relay_serving`** — `get_relay_serving_status`, `set_relay_serving`
- **`safety`** — `get_safety_number`, `list_peer_verifications`, `set_contact_verified`
- **`screenshare`** — `cancel_screen_share_picker`, `enumerate_screen_sources`, `screenshare_ws_url`, `start_screen_share`, `stop_screen_share`, `subscribe_screen_share_events`, `subscribe_screen_share_frames`
- **`sfx`** — `play_sfx`, `start_ring`, `stop_ring`
- **`terminal`** — `terminal_ack`, `terminal_close`, `terminal_open`, `terminal_resize`, `terminal_write`
- **`transparency`** — `audit_peer_account_key`, `self_audit_account_key`, `verify_own_build`
- **`tray`** — `tray_set_close_to_tray`, `tray_set_enabled`, `tray_set_unread`, `tray_set_voice_state`
- **`update`** — `is_update_required`, `mark_update_required`
- **`user`** — `get_preferences`, `get_user_profile`, `save_preferences`, `search_user_by_username`, `update_user_profile`
- **`voice`** — `get_last_join_timings`, `get_voice_gate_state`, `join_voice_channel`, `leave_voice_channel`, `list_audio_devices`, `prepare_voice_connection`, `release_voice_ptt`, `set_remote_user_volume`, `set_voice_audio_processing`, `set_voice_input_device`, `set_voice_input_mode`, `set_voice_output_device`, `set_voice_ptt_held`, `subscribe_voice_events`, `toggle_voice_deafen`, `toggle_voice_mute`
- **`voice_test`** — `play_test_tone`, `record_and_play_back`, `set_mic_test_monitor`, `start_mic_test`, `stop_mic_test`, `stop_test_playback`, `subscribe_voice_test_events`

If this appendix and `src-tauri/src/lib.rs` disagree, `lib.rs` is right and this file is stale.

---
_Back to [index.md](./index.md)_

---

## Complete registered-command index

Generated from `src-tauri/src/lib.rs`'s `invoke_handler!` — **200 commands** in 28 shim modules.
Prose above covers roughly half of these; this index covers all of them, so a name that
appears here but not above is registered and real, just undocumented. Regenerate rather
than hand-edit.

**`(root)`** (4) — `hide_window`, `read_clipboard_files`, `read_clipboard_image_to_temp`, `write_clipboard_text`

**`tray`** (4) — `tray_set_close_to_tray`, `tray_set_enabled`, `tray_set_unread`, `tray_set_voice_state`

**`media_permissions`** (4) — `get_media_permission_status`, `open_privacy_settings`, `revoke_media_permissions`, `set_revoke_media_on_exit`

**`auth`** (15) — `delete_account`, `dev_login`, `get_identity`, `get_session`, `initialize_identity`, `is_current_device_registered`, `list_known_accounts`, `list_user_devices`, `logout`, `request_email_change_otp`, `request_otp`, `revoke_device`, `verify_email_change`, `verify_otp`, `wipe_local_data`

**`pin`** (4) — `get_unlock_state`, `lock`, `set_pin`, `unlock`

**`autolock`** (2) — `report_user_activity`, `set_auto_lock_timeout`

**`device_enrollment`** (10) — `approve_device_enrollment`, `await_enrollment_approval`, `finalize_device_enrollment`, `list_pending_enrollment_requests`, `list_security_events`, `poll_enrollment_status`, `recover_with_secret_key`, `reject_device_enrollment`, `reset_identity_and_recover`, `start_device_enrollment`

**`safety`** (3) — `get_safety_number`, `list_peer_verifications`, `set_contact_verified`

**`transparency`** (3) — `audit_peer_account_key`, `self_audit_account_key`, `verify_own_build`

**`overlay`** (2) — `get_overlay_mode`, `set_overlay_mode`

**`relay_serving`** (2) — `get_relay_serving_status`, `set_relay_serving`

**`user`** (5) — `get_preferences`, `get_user_profile`, `save_preferences`, `search_user_by_username`, `update_user_profile`

**`groups`** (27) — `accept_group_invite`, `approve_join_request`, `create_channel`, `create_group`, `create_group_invite_link`, `decline_group_invite`, `delete_channel`, `delete_group`, `get_group_join_requests`, `get_group_members`, `get_my_join_request`, `get_pending_invites`, `leave_group`, `list_group_channels`, `list_group_invite_links`, `list_user_groups`, `list_user_groups_with_channels`, `redeem_group_invite_link`, `reject_join_request`, `remove_member_from_group`, `request_group_access`, `revoke_group_invite_link`, `search_group_by_slug`, `send_group_invite`, `set_member_role`, `update_channel`, `update_group`

**`dm`** (8) — `accept_dm_request`, `add_user_to_dm_channel`, `create_dm_channel`, `get_dm_channel`, `leave_dm_channel`, `list_dm_channels`, `list_dm_requests`, `remove_user_from_dm_channel`

**`blocks`** (3) — `block_user`, `list_blocked_users`, `unblock_user`

**`bookmarks`** (5) — `list_saved_messages`, `resolve_message_permalink`, `save_message`, `toggle_saved_message`, `unsave_message`

**`messages`** (20) — `add_reaction`, `delete_message`, `edit_message`, `get_channel_messages`, `get_dm_messages`, `get_message_retention`, `get_reactions`, `ingest_channel_envelopes`, `ingest_dm_envelopes`, `list_messages`, `list_thread_summaries`, `read_channel_messages`, `read_dm_messages`, `read_last_messages`, `read_thread_messages`, `remove_reaction`, `run_message_eviction`, `search_messages`, `send_message`, `set_message_retention`

**`mls`** (3) — `catch_up_all_mls_groups`, `poll_mls_welcomes`, `process_pending_commits`

**`livekit`** (12) — `cancel_call`, `connect_rooms`, `get_livekit_token`, `get_livekit_url`, `get_livekit_view_token`, `list_voice_participants`, `list_voice_room_counts`, `publish_ping`, `publish_typing`, `publish_voice_presence`, `start_call`, `subscribe_realtime`

**`r2`** (7) — `download_file`, `download_media`, `get_media_url`, `get_public_file_url`, `upload_file`, `upload_media`, `upload_public_file`

**`update`** (2) — `is_update_required`, `mark_update_required`

**`install_kind`** (1) — `detect_managed_install`

**`voice`** (16) — `get_last_join_timings`, `get_voice_gate_state`, `join_voice_channel`, `leave_voice_channel`, `list_audio_devices`, `prepare_voice_connection`, `release_voice_ptt`, `set_remote_user_volume`, `set_voice_audio_processing`, `set_voice_input_device`, `set_voice_input_mode`, `set_voice_output_device`, `set_voice_ptt_held`, `subscribe_voice_events`, `toggle_voice_deafen`, `toggle_voice_mute`

**`voice_test`** (7) — `play_test_tone`, `record_and_play_back`, `set_mic_test_monitor`, `start_mic_test`, `stop_mic_test`, `stop_test_playback`, `subscribe_voice_test_events`

**`screenshare`** (7) — `cancel_screen_share_picker`, `enumerate_screen_sources`, `screenshare_ws_url`, `start_screen_share`, `stop_screen_share`, `subscribe_screen_share_events`, `subscribe_screen_share_frames`

**`camera`** (6) — `list_video_devices`, `start_camera`, `start_camera_preview`, `stop_camera`, `stop_camera_preview`, `subscribe_camera_events`

**`sfx`** (3) — `play_sfx`, `start_ring`, `stop_ring`

**`terminal`** (5) — `terminal_ack`, `terminal_close`, `terminal_open`, `terminal_resize`, `terminal_write`


_Back to [index.md](./index.md)_
