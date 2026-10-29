# Backend Commands

All backend calls from the frontend use `invoke("command_name", { args })` (imported from `frontend/src/bridge`, which routes to Tauri's `invoke`). Dispatch happens in `src-tauri/src/commands/`: a thin `#[tauri::command]` shim per command — registered in `src-tauri/src/lib.rs`'s `invoke_handler!` — forwards the JSON-shaped call into `pollis-core/src/commands/`. The implementations live in `pollis-core` so a CLI / TUI / mobile binding (uniffi) can call them without any shell-runtime dependency. Edit `pollis-core`, not the shims.

The path in each section header below points at the implementation in `pollis-core`. The `#[tauri::command]` shim under `src-tauri/src/commands/` with the same module name re-exports the types and forwards each command verbatim.

> ### ⚠️ This is a PARTIAL listing — read this before trusting it
>
> **Last verified: 2026-08-03 (#714), against `src-tauri/src/lib.rs` at `77abb2b`.**
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
exactly like the writes, and neither `pollis-core` nor the shipping `pollis`
binary links `libsql` at all — so a client-side query is not a policy violation
any more, it is a compile error. (`pollis` keeps it optional behind
`test-harness`, for the flows harness's process-local "remote"; both graphs are
walked by `pollis-core/tests/no_client_side_remote_reads.rs`.)

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
3. **Commit holes are reported, not refused.** `head`, `head_generation` and
   the commits are read inside that same transaction, and
   `pollis_api::reads::check_contiguous` reports a gap as
   `ConversationState::hole`. The client's answer to a real hole
   (`forget_local_mls_group_at`, then external-join onto the head) is the
   recovery, pinned by `epoch_gap_recovers_via_external_join` and
   `corrupt_commit_recovers_instead_of_wedging`; refusing to serve would leave a
   damaged conversation unreadable for good. What the one transaction fixed
   (#987) is the *phantom* hole: with `head` and `commits` from separate
   statements, a torn read could manufacture a gap and trigger that destructive
   recovery for nothing. A retention prune is a moved floor (`pruned_below`), not
   a hole.
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

**Responses are in the contract too (#922).** Each endpoint declares its success body
as `DsRequest::Response`. The DS returns it via `writes::ok_response::<B>`, which
accepts only `B::Response`, and `pollis-core` decodes it via `ds_post_json::<B>()`, so a
renamed response field breaks both ends at compile time. Endpoints with no body declare
`Empty` (accepted only by `ok_empty`); a plain `{"status":"ok"}` is the tagged
`StatusOk` enum, so `{"status":"borked"}` can neither typecheck nor decode.

Each table row also carries an `Audience` (#925): `Client` rows get the
`ClientRequest` marker every `ds_post*` requires, so an `Operator` endpoint (recovery
levers such as `/v1/welcomes/resubmit`, `/v1/envelopes/gc`) cannot gain a client call
site; operator rows must be listed in `docs/ds-operator-endpoints.md` (a test reads it).
A trailing `idempotent` marks endpoints that may be resent after an ambiguous
connection failure (#1497). The only GETs the DS serves are `/health`, `/version`,
`/v1/config` and `/v1/retention/metrics`: any read about user data must be a POST in
the table, because a GET's query string is outside the request signature (#681).

**What the contract does not cover:** field *values* (e.g. `actor_id: None` where
`Some(me)` was meant), version skew against an already-deployed DS (additive/optional/
defaulted wire rules still apply), and 4xx/5xx error bodies (callers branch on status).

The `pollis-api` crate is dependency-light (serde only) on purpose, mirroring
`pollis-schema` and `pollis-device-cert`: sharing it does not make the DS depend
on `pollis-core`.

## auth (`commands/auth.rs`)
- `initialize_identity(user_id)` — ensure MLS credentials + KPs, poll welcomes. Requires the local DB to be open (post-`set_pin` / `unlock`).
- `request_otp(email)` — send OTP code to email
- `verify_otp(email, code)` → `AuthResult` — verify OTP, register the device. Does **not** open the local DB; `set_pin` (signup) or `unlock` (resume) does.
  - **The device id it sends is the one the OTP session gets bound to**, and it has to be chosen before the DS has said anything, so it is picked locally: a fresh ULID normally, or — for a returning device that is not yet enrolled — its existing stable id, looked up from the typed address by `auth::stable_device_id_for_reenrollment` (an unenrolled device cannot restart its enrollment otherwise; the enrollment-request write is session-gated).
  - That lookup goes through the local accounts index, which can be stale about the address (an email change made on another device leaves the old one in the keystore). When it misses, the session is bound to a fresh candidate while the keystore still holds an older id — and a session-credentialed `rotate-identity` keeps exactly the session's device (`account::reset_recover_in_tx`, read from the server-side session record, never the request body), so the soft reset used to delete the signing-in device's own row and strand it (#1099).
  - So a **not-yet-enrolled** device with a mismatch **adopts the session's id** — persisting it to the keystore and registering it under the session. An already-enrolled device also sees a mismatch by design (its stable lookup returns `None`) and keeps its stored id; holding the account key is what separates the two cases.
- `get_session()` → `AuthResult | null` — rebuild profile from `accounts.json`. Does not open the local DB.
- `get_device_id()` → `string | null` — this device's stable `device_id` (or null pre-registration). Used to build the per-device voice identity (`voice-{user_id}:{device_id}`) so a client can tell which voice participant is itself (#140). **Not a registered Tauri command** as of 2026-08-03 (#714) — it lives in `pollis-core/src/commands/auth.rs` and is called internally; `invoke("get_device_id")` will fail.
- `logout(delete_data)` — clear session, optionally delete local data
- `delete_account(user_id)` — delete account from Turso + local (user DB, keystore, that user's media cache, media-server token, staged attachments)
- `wipe_local_data()` — delete all local databases and keystore entries

## diagnostics (`commands/diagnostics.rs`, #1305)
- `get_diagnostics(appVersion?)` → `string` — the on-device log (`pollis-core/src/diag.rs`, `<data dir>/logs/pollis.log` + the rotated `.1`) under a short header (app/core version, OS, time). Local only; the desktop shim passes the bundle version, mobile passes `appVersion` from `expo-constants`. Empty log when no host called `diag::init` (tests, CLI, TUI).

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
- `search_user_by_username(query)` → `User | null` — a username OR an email, resolved by the DS's `directory::user_by_identifier` (`POST /v1/directory/users`): an identifier containing `@` matches `email` only, anything else matches `username` only, exact. Never `username OR email` — that predicate's first row was won by an account whose username was the target's email address (see `database.md` → users).
- `get_preferences(user_id)` → JSON string
- `save_preferences(user_id, preferences_json)`
- ~~`upload_avatar(user_id, file_data, file_name, content_type)`~~ / ~~`get_avatar_url(user_id)`~~ — **neither exists** anywhere in `pollis-core` or `src-tauri` as of 2026-08-03 (#714). Struck out rather than replaced with a guess: `avatar_url` is a field on the user profile returned by `get_user_profile`; whatever writes it is not one of these two commands. Trace it from `frontend/src/hooks/queries/useUserProfile.ts` before documenting a replacement.

## groups (`commands/groups/`)
- `list_user_groups(user_id)` → `Group[]`
- `list_user_groups_with_channels(user_id)` → `GroupWithChannels[]`
- `list_muted_channels(user_id)` → `string[]` — **#1431.** The channel ids this account muted and is still a member of (`POST /v1/read/channel-mutes`). Account state on the DS, so every device reads the same list; the DS sends `channel_mutes_changed` to the owner's inbox after each change, and both clients refetch on it.
- `set_channel_muted(user_id, channel_id, muted)` — **#1431.** Mute or unmute one group channel for the whole account (`POST /v1/channel-mutes/set`). The DS refuses a channel the caller is not a member of, a DM and a group id; both directions are idempotent. The push fan-out skips a muted member unless the message names them by `@username`; desktop's `notify.ts` applies the same rule to its own sounds and banners. See `notifications.md` → "Per-channel mute".
- `list_group_channels(group_id, requester_id)` → `Channel[]` — **#917: members-only.** Took no caller until then, so any group's channel list was readable by anyone who named the group id. Channel names are content-adjacent metadata (`#general` says nothing, `#q3-layoffs` says a lot). Non-member → `you are not a member of this group`.
- `create_group(name, description?, owner_id)` → `Group`. Also wakes the creator's other devices with a `membership_changed` to their own inbox, after the reconcile that queued their Welcomes (`mls.md` → "A new group has no room yet"). Tests: flows `groups::a_group_created_on_one_device_wakes_the_users_other_devices`, Maestro `scripts/maestro-own-groups.sh`.
- `create_channel(group_id, name, description?, channel_type?)`
- `send_group_invite(group_id, inviter_id, invitee_identifier)`
- `get_pending_invites(user_id)` → `PendingInvite[]`
- `accept_group_invite(invite_id, user_id)`
- `decline_group_invite(invite_id, user_id)`
- `list_group_invites(group_id, user_id)` → `GroupInvite[]` — **#1506.** The member page's "Invited" section: every pending invite to the group, with invitee, inviter and `created_at` (newest first). Members only (an outsider gets the shared refusal); read as `POST /v1/directory/group` with `want_invites: true`, an opt-in flag so the roster and authz reads don't pay for it — an older DS ignores the flag and answers no invites.
- `revoke_group_invite(invite_id, group_id, user_id)` — **#1506.** Withdraw a pending invite (`POST /v1/invites/revoke`). The DS allows the member who sent it and the group's admins, both re-derived from the invite row, and deletes it — which takes the invitee out of the desired MLS roster (pending invitees count as present). The core then runs the same catch-up + reconcile a removal does, committing the invitee's devices (grafted in at invite time) out of the tree, and wakes the group room. The DS itself nudges the invitee's inbox: once the row is gone the revoker no longer shares a roster with them, so the broker would refuse the revoker's own publish.
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
- `send_message(conversation_id, sender_id, content, reply_to_id?, thread_id?, sender_username?, message_id?)` → `Message` — **durable since #1335**: the local `message` row is written FIRST, as `send_state = 'pending'` with an empty ciphertext, before any network work (resolve, MLS catch-up, seal, DS post); delivery fills the ciphertext and clears the state, a failed attempt sets `'failed'`. `message_id` (a ULID from `new_message_id`) lets the renderer name the message before it calls, so its optimistic stub and the stored row are one message; a repeat call with the same id is idempotent. `attachments?: OutgoingAttachment[]` (`{ path | stagedId, filename, contentType, blurhash?, width?, height? }`) hands the UPLOAD to the outbox too: the bytes are stored encrypted with the row before any network work and `content` is then the caption. Thread replies take the same path. A failed attempt is `failed`, or `rejected` when no retry can fix it. See [MLS → The durable outbox](mls.md#the-durable-outbox-1335).
- `new_message_id()` → `String` — a fresh ULID for the renderer's next send (#1335), minted in Rust so it carries the crate's randomness (it is the envelope id the DS dedupes on).
- `retry_message(message_id)` → `Message` — one more delivery attempt for an unsent message, under the same id; the renderer's Retry.
- `discard_unsent_message(message_id)` → `bool` — throw away an unsent message; waits out an attempt in flight. A row that was ever sealed may have reached the DS, so it is first withdrawn from everyone (`retract_for_everyone`: redaction + envelope removal, a no-op when nothing landed) and the discard fails rather than leave a delivered copy the sender could still have deleted — unless the withdrawal fails for good (no longer a member, conversation gone), when the row is removed anyway. `false` when there was nothing unsent under that id. `delete_message` on an unsent row routes here.
- `flush_outbox()` → `OutboxReport { delivered, failed }` — deliver every `pending` / `failed` message, oldest first; `rejected` ones (a refusal no retry fixes) are left to the user. Single-flight but never lossy: a call that arrives while a drain runs makes that drain go round once more (or drains itself if the last pass already ended) and returns the resulting report. Spawned by `unlock` (every cold start) and called by both renderers after launch, on a realtime reconnect, on the browser `online` event (desktop) and on returning to the foreground (mobile). Event-driven; nothing polls.
- `read_thread_messages(thread_id)` → `Vec<ChannelMessage>` — one thread's replies, oldest-first, local-only (#825)
- `list_thread_summaries(conversation_id)` → `Vec<ThreadSummary>` — reply count + last-reply time per thread root
- `get_channel_messages(user_id, channel_id, limit, cursor?)` → `MessagePage`
- `get_dm_messages(user_id, dm_channel_id, limit, cursor?)` → `MessagePage`
- `read_last_messages(conversation_ids)` → `Vec<ChannelMessage>` — the newest message in each of the given conversations, in ONE call (#874). Drives every sidebar preview row (channels, DMs, DM requests): the previous shape was one `read_channel_messages` / `read_dm_messages` per ROW, re-fired on mount, on window focus and on every `deleted_message` event. A single `ROW_NUMBER() OVER (PARTITION BY conversation_id ...)` over the local `message` table, chunked at 400 bound parameters, plus one batched username hydration. Conversations with no local messages are ABSENT from the result — the caller keys by `conversation_id`, so a null placeholder would be a second spelling of the same answer. Ordering key matches `read_local_channel_page` exactly, so a preview and the top of the opened conversation cannot disagree.
- `edit_message(message_id, conversation_id, sender_id, new_content)`
- `delete_message(message_id, user_id)` — hard-deletes the envelope on Turso + the sender's local row. Both branches (self and admin) build the `POST /v1/messages/delete` body through the one `delete_message_body(...)` helper so `actor_id` cannot be forgotten on either — see "Every DS write body must NAME ITS ACTOR" above; it was missing on both before #875, which made deletion 403 on any `require_auth = false` deployment. If the message had attachments (`_att` in the plaintext JSON payload), each `content_hash` is reference-counted against the sender's other non-deleted local messages; unreferenced ones have their `attachment_object` row + R2 object removed (best-effort, logged on failure). Cross-user references are invisible locally because attachment metadata lives inside the MLS-encrypted payload; the DS's derived reference count (`attachment_ref` ⋈ `message_envelope`) is what actually gates collection. Since #1382 every upload is its own object under a random file id, so there is no shared dedup row to re-register.
- `read_messages_around(conversation_id, message_id, limit?)` → `MessagePage` — a page CENTRED on one message (#850), `limit` before and `limit` after, anchored on `(sent_at, id)` because that is the order the log renders in. The jump-to-message read path: a search hit or permalink almost by definition points outside the loaded window, and the renderer's windowing can only reveal a row the query already holds. A message this device does not hold yields an empty page — the same honest answer `resolve_message_permalink` gives.
- `read_messages_after(conversation_id, cursor, limit?)` → `MessagePage` — the forward page read (#1039): the oldest `limit` (default 50, max 200) rows strictly NEWER than `cursor`, returned newest-first like every other page, with `next_cursor` = the newest row returned iff the page came back full (a short page is the tail this device holds, and an empty one never carries a continuation). Exclusive of the cursor row and tie-broken on `id` like the backward read, so walking forward from a `read_messages_around` page's head and backward from its `next_cursor` visits every row exactly once. This is what lets a jump-to-message window page toward the present until it rejoins the live tail instead of being merged with it. Local SQLite only — page reads never touch the DS.

### Search (#850, `commands/messages/search.rs`)
- `search_messages(query, conversation_id?, sort?, limit?, cursor?)` → `SearchPage` — FTS5 over the local index, NOT a `LIKE` scan. `sort` is `recent` | `relevant`; omitting it lets Rust pick (relevance globally, recency inside a conversation) and the page reports what it used. `query` is PARSED (`from:`, `in:`, `before:`/`after:`/`on:`, `has:attachment`, `has:link`) and re-emitted as a well-formed FTS5 expression — raw user text must never reach `MATCH`. Returns `total` (all hits, not the page), an offset `next_cursor`, and on the first page a `corpus` describing what this device actually holds. Full article: [Message Search](./search.md).
- `rebuild_search_index()` → `()` — drop the FTS index and rebuild it from `message`. The Preferences escape hatch for index drift; startup already repairs silently when `integrity-check` fails.

### Read state (#844, `commands/messages/read_state.rs`)
- `get_unread_counts(user_id)` → `UnreadCount[]` — one entry per conversation with something unread; a conversation with nothing unread is ABSENT rather than zero, so the result is the complete badge set. Derived by counting local `message` rows past the conversation's cursor (`(m.sent_at, m.id) > (c.last_read_at, c.last_read_message_id)` — a row-value compare matching the list's `ORDER BY sent_at DESC, id DESC`), excluding your own and deleted messages.
- `mark_conversation_read(conversation_id, user_id)` → `bool` — names the CONVERSATION, never a position: Rust resolves the newest message itself, so the renderer cannot assert a cursor pointing at a message that does not exist or is not the newest. Returns whether the cursor moved; a move schedules the debounced cross-device push, and (#1508) a debounced `POST /v1/push/dismiss` so the conversation's push notifications clear from the account's phones — from every client, desktop and TUI included (see `notifications.md` → "Clearing read push notifications").
- `dismissed_push_handles(user_id, handles)` → `string[]` — **mobile bridge only (#1508).** Which of the tray's notification handles were read since on any device (`POST /v1/push/dismissed`, at most 100). The phone asks on landing in the app and on every return to the foreground and clears those, covering a silent dismissal push that never arrived.
- `sync_read_cursors(user_id)` → `usize` — pull the account's encrypted cursor blob from the DS, merge it (`max()` per conversation — cursors never rewind), push the merged set back. Returns how many local cursors moved. Called ONCE at startup from `AppShell`; the ongoing direction is event-driven, never polled.
- **Mobile (#1422):** all three are bridge arms (same camelCase args), driven by `mobile/hooks/queries/useUnread.ts`. `get_unread_counts` is re-read on landing in the app (sign-in, unlock, cold start), on every return to the foreground and after every ingest (push receipt, resume / reconnect catch-up) and replaces `appStore.unreadCounts` whole; `mark_conversation_read` runs wherever a badge is cleared and from the chat screen itself (on focus, on blur, and as messages arrive while it is open); `sync_read_cursors` runs on unlock and on each return to the foreground — a phone is resumed far more often than launched.

**The cursor syncs as ciphertext, unlike preferences.** `save_preferences` puts plaintext JSON in Turso's `user_preferences`; a read cursor must not go the same way, because `(conversation_id, last_read_at)` rewritten on every open is a timestamped log of which conversations a human reads and when. So the payload is padded to a size bucket (`commands::messages::framing::pad`, reused rather than reinvented) and sealed with AES-256-GCM under a key derived by HKDF-SHA256 from the **account identity key** — the key that represents the human, that every device of the account already holds, and that the DS has never seen in the clear. Same construction as the `account_recovery` wrap, different `info` string (`pollis-read-cursor-sync-v1`) so the one key cannot be repurposed across the two uses. Transport is `POST /v1/read-cursors/save` + `POST /v1/read/read-cursors`; the DS validates the blob's envelope (base64, 12-byte nonce, ≤ 256 KiB) and nothing about its contents, because it holds no key for it. Residual leak, stated plainly: the server sees that the blob changed, its size bucket, and when.

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
- `report_user(reported_id, reason, conversation_id?, message_id?, also_block)` (#1213) — `POST /v1/reports` with the reporter (this device's user), the reported account, a reason (`spam` | `harassment` | `illegal` | `other`) and optionally the conversation + message ids. **Never message text**: the wire type and `user_report` have no field for it. `also_block` then runs `block_user`. The DS refuses a self-report, a forged reporter, a message id without its conversation, a conversation the reporter is not in, an unknown account, and more than 20 reports per reporter per day; it emails support@pollis.com (ids only) and stores the row. Desktop: message menu → Report, profile → Report, page `/report/$userId[/$conversationId/$messageId]`. Mobile: long-press → Report, user screen → Report, screen `app/report.tsx`.
- `list_blocked_users(user_id)` → `BlockedUser[]`
- Enforced in: `create_dm_channel`, `send_message` (DM only — group-channel sends are not gated), `send_group_invite`. All three return the identical string `"message request pending"` so the sender cannot infer which gate rejected them.

## bookmarks (`commands/bookmarks.rs`)

Saved messages and message permalinks (#854). **Every command here is device-local**
— pure rusqlite against the encrypted local DB (`bookmark` table in
`db/local_schema.sql`). None of them touch the DS or Turso, and no DS endpoint backs
any of them.

Why device-local: which messages a user saved is interest metadata, a ranked list of
the moments and conversations they care most about. The server already sees envelope
routing; a bookmark set would tell it strictly more, in its most sensitive shape. The
cost is that bookmarks don't follow a user to a new device, consistent with accepted
loss (2).

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

## export (`commands/export.rs`)

On-device plaintext export (#856): one conversation, or the whole account, written as
a JSON file to a path the user picked in the OS save dialog. **Strictly device-local**
— pure rusqlite against the encrypted local DB; no DS, no Turso, no R2. The module's
own test scans its source for the network helpers (`ds_client`, `ds_post`, `ds_reads`,
`reqwest`, `presign`, `r2::`) so that stays true.

Every write goes through `private_fs` (0600 files, 0700 directories), never the process
umask (#1093) — an export is the one artefact holding full plaintext history and every
attachment, and it lands wherever the picker pointed, often a shared-machine directory.
A second source scan bans `std::fs::write` / `File::create` / `create_dir_all` in
`export.rs` and `export_fetch.rs` so the next write site cannot regress it.

- `export_archive(path, conversation_id?)` → `ExportSummary` — `conversation_id`
  narrows to one conversation; `null` is the full account archive (which also carries
  the Vault). `path` must be absolute (a dialog result); the file is written to a
  sibling `.part` and renamed into place, so a failed write never leaves a truncated
  archive. Returns `{ path, conversations, messages, attachments, vault_entries, bytes }`.

**Shape** (`format: "pollis-archive"`, `version: 1`): `account { user_id, username }`,
`conversations[] { id, kind, name, group_id, group_name, peer_user_id, messages[] }`,
`vault[]`. Each message carries `text`, `attachments[] { name, content_type, size_bytes,
content_hash, storage_key, sha256, key_version }` (`content_hash` is the random file id,
`sha256` the plaintext digest, `key_version` the file-key-ring version, `0` = Vault; the
last two are `null` for a reference from before #1382), reply/thread ids, timestamps, `saved` (bookmark) and
`receipts[]`. Names come from `conversation_cache` / `user_cache` — a conversation the
cache never learned still exports, with `name: null`. A deleted message exports as a
tombstone (`text: null`, `deleted_at` set), never its last body. The `{"_att","_txt"}`
envelope is unpacked; a body that merely starts with `{` is kept as text.

**What it is not.** Not key backup, and not history sync: it reads plaintext this
device *already* decrypted, so accepted losses (1) and (2) in `CLAUDE.md` apply to the
archive exactly as they apply to the message list. Ciphertext, `mls_kv`, `identity_key`,
`pin_key_cache`, `contact_verification` and the keystore are never read
(`archive_never_carries_key_material` seeds each with a marker and asserts none reach
the file). **No import command exists and none may be added** — a reader for this
format would be the backup channel #856 forbids; `there_is_no_import_command` and
`frontend/tests/export-archive.test.ts` guard both halves.

**Attachments.** Every reference carries its metadata plus a deterministic `file`
name (`<hash[..16]>-<sanitised name>`, one safe path component). Bytes are written to
the sibling `<archive>-files/` directory for every distinct attachment whose decrypted
copy is **already in this device's media cache** — looked up for the unlocked user *by
name* (`r2::find_cached_file_for_user`, the #1000 lesson), decrypted under `db_key` and
re-verified against its `sha256` before being vouched for; anything that fails is reported
missing rather than aborting. Still zero network. The summary returns `files_dir`,
`attachments_written` and `attachments_missing[]` (the exact list the opt-in fetch
takes). The JSON never claims a file exists — the filesystem is the source of truth.
The archive holds **no** key: the `content_hash` is only the object's random file id
(#1382 — before that it was the convergent-encryption key, which made the archive alone
enough to decrypt every file; that scheme was broken and replaced). Fetching a missing
attachment later needs a device that can still open the conversation's file key ring
(or, for Vault files, the account key). A pre-#1382 reference (no `sha256`/`key_version`)
is not listed as missing — its object can no longer be opened.

## export_fetch (`commands/export_fetch.rs`)

The **opt-in second step** of the export, and the one place the feature touches the
network. A separate module and command on purpose: `export.rs` stays scannable as
strictly local, and the default path never comes here.

- `fetch_export_attachments(files_dir, attachments: MissingAttachment[])` (each
  `{ content_hash, storage_key, sha256, scope: AttachmentKeyScope, content_type, file }`) →
  `FetchSummary { fetched, failed[] { content_hash, file, error } }` — downloads the
  attachments an export reported missing into its files directory, via
  `r2::download_media` (DS-minted presigned GET, decrypt under the ring key named by
  `scope`, `sha256` check). Only
  ever reachable from the separately-worded "Download the missing attachments" button
  that appears *after* an archive is written; `frontend/tests/export-archive.test.ts`
  pins that to `ExportArchiveButton` alone. Sequential — one presigned GET at a time,
  so a large archive cannot fan out into a burst against the DS.
- The list comes from the renderer and is treated as untrusted: `files_dir` must be
  absolute and every `file` a single path component (no separators, no `..`), or the
  whole call is refused before any byte moves. A file already present with the right
  hash is counted and skipped (idempotent, no re-download); bytes that fail their hash
  are a per-file failure and never reach the disk.

UI: Security page → "Your data" (account), `DMSettings` and the channel header
(conversation) — all through `components/Security/ExportArchiveButton`, which also
carries the opt-in fetch offer when the summary reports missing attachments.
Playwright: `e2e/export-archive.spec.ts` (both skins; the picker is the mock's
`plugin:dialog|save`, driven by the `exportSavePath` preload — `null` = cancel).

**Mobile** (`bridge.rs` arms `export_archive`, `fetch_export_attachments`,
`bundle_export`; `mobile/components/ExportArchive.tsx` on Security, `dm/info` and
`conversation/info`). A sandboxed app has no "save as", so the archive is written
under the app's cache dir (`pollis-export/`), optionally completed with the fetch,
then `bundle_export(path)` → `bundle_archive` zips `archive.json` + `files/` into one
`<archive>.zip` for the OS share sheet (`expo-sharing`). Desktop keeps the loose
JSON-plus-folder layout because the user chose the destination there. Maestro:
`mobile/.maestro/flows/export.yaml`.

## pinned_messages (`commands/pinned_messages.rs`)

Pinned messages (#99). Not `pin` — that module is the device-unlock PIN. Pins are
per-conversation SHARED state, so unlike bookmarks they are DS-backed: the server
stores a content snapshot per pin, sealed under the conversation's 32-byte pin master
key (`Kpin`), which itself is stored server-side only WRAPPED under a KEK derived from
the current MLS epoch's exporter secret (`export_secret("pollis-pin-kek")`). The
unwrapped `Kpin` is cached in the local encrypted DB (`pin_key_cache`) because
`max_past_epochs = 0` destroys the previous exporter at every merge — a device that
holds the key re-wraps it under the new exporter after each epoch advance
(`maybe_rewrap_pin_key`, hooked at every `voice_e2ee::on_mls_epoch_changed` site plus
a per-catch-up backstop next to `ensure_group_info_published`); a device that does not
unwraps the stored wrap the moment its `(generation, epoch)` matches
(`try_adopt_pin_key` fires right after a Welcome lands, before the post-join
self-update moves the epoch). Minting is INSERT-ONLY on the DS so two racing minters
can never fork the key. See `.codesight/wiki/mls.md` § "Pinned-message key state".

- `pin_message(conversation_id, message_id, user_id)` — snapshot the message from the
  local `message` row (a message this device cannot read cannot be pinned), seal under
  `Kpin`, `POST /v1/pins/pin`. Mints the conversation's key on first use. Idempotent
  per `(conversation_id, message_id)`.
- `unpin_message(conversation_id, message_id, user_id)` — `POST /v1/pins/unpin`. Any
  current member may unpin (Slack's model); idempotent.
- `list_pinned_messages(conversation_id, user_id)` → `PinnedMessage[]` — newest first,
  decrypted locally; `sender_username` resolved from the local `user_cache` when known.
  Skips (with a log line) a pin that fails AEAD or whose decrypted snapshot names a
  different message/conversation than its row — a spliced row must not render as a
  relocated pin. Errors with "pin key not yet available" on a device that cannot
  unwrap the current wrap yet (a fresh external joiner); that heals when any
  key-holding member's next pass re-wraps.

All three are dispatched by the mobile bridge (`pollis-core/src/bridge.rs`) as
well. Desktop's UI is the message menu's Pin / Unpin plus `PinnedPanel` in the
right panel; mobile's (#1315) is the message sheet's `btn-pin` plus
`mobile/app/conversation/pins.tsx` behind the chat header's `btn-pins`, whose
rows jump back to the message (`mobile/stores/messageJumpStore.ts`). Proofs:
`src-tauri/tests/flows/pinning.rs`; on a device `mobile/.maestro/flows/pins.yaml`
and the two-member `mobile/scripts/maestro-pins.sh` (dev-DS peer
`pollis-tui/examples/pins_peer.rs`: each side sees the other's pin and unpin).

## file_keys (`commands/file_keys.rs`, #1382)

The per-conversation **attachment file key ring** — the key every channel/DM
attachment is sealed under. Same construction as the pin key above, with one
difference: the ring rotates. Each MLS conversation has a ring of random 32-byte
keys (each entry also records the `(generation, epoch)` it was added at); the DS
stores it only WRAPPED under `export_secret("pollis-file-kek")` in `file_keystate`
(`key_version` = ring length), and the unwrapped ring is cached locally in
`file_key_cache`. Every current member — late joiner, external joiner, newly
enrolled device — can unwrap it and open every file the conversation ever shared,
including files from before it joined (deliberate). Replaced convergent encryption,
under which the bucket plus the DS's `attachment_object` rows decrypted every file.

Internal hooks (not invokable commands):
- `mint_file_keys_for_new_group` — at conversation creation, while the creator is the
  sole member (best-effort; the first upload mints otherwise). DS mint is insert-only.
- `maybe_rewrap_file_keys` — at every epoch-advance site next to `maybe_rewrap_pin_key`
  (`group_state.rs`, `reconcile.rs`, `self_update.rs`, `migrate.rs`); re-wraps under the
  new exporter and settles any owed rotation.
- `try_adopt_file_keys` — right after a Welcome or external join.
- `on_member_removed` — called by the device that committed a removal: records a
  durable "rotation owed at `(g, e)`" mark (`file_key_cache.rotation_owed_*`), then
  appends a fresh key wrapped at the post-removal epoch. New uploads use it; old files
  are not re-encrypted. The mark clears only once the ring holds a key added at or after
  `(g, e)`, so two racing removals are safe.
- `upload_key` / `key_at_version` — the newest key for an upload; the key at a given
  version for a download (refetching from the DS when the cached ring is shorter).
- `test_file_key_version` — test harness only (`src-tauri/tests/flows/file_keys.rs`:
  ring mint, late join, rotation on removal).

DS endpoints: `POST /v1/file-keys/keystate` (mint / re-wrap / rotate; the DS refuses
every fork — mint only at `key_version = 1`, re-wrap at the same version and a newer
`(generation, epoch)`, rotate at exactly `+1`) and `POST /v1/read/file-keystate`.
Both membership-gated. See `.codesight/wiki/database.md` § `file_keystate`.

## vault (`commands/vault.rs`)

The Vault (#107): a personal encrypted space that presents as a chat with yourself
and syncs across the account's devices like cloud storage. Entries are sealed under
HKDF-SHA256(account identity key seed, salt = user id, info = `pollis-vault-v1`) →
AES-256-GCM over the padded entry JSON — the read-cursor construction (#844), per
entry. Because the account key transfers at enrollment, a brand-new device decrypts
the ENTIRE vault: the deliberate, cryptographic inversion of accepted loss (2)
("a new device starts empty"). The pinned flag lives INSIDE the ciphertext, so
pin/unpin is indistinguishable from an edit on the wire. Entry bodies are the same
content string chat uses (plain text or the `_att` attachment envelope), so file
drops ride the normal staged-upload/R2 pipeline; every save declares its attachment
file ids so the DS's `vault_attachment_ref` rows keep the objects alive against
attachment GC. Vault files are sealed under the account's Vault file root
(`vault_file_root_key`: HKDF over the account identity key seed, `info =
"pollis-vault-files-v1"`, #1382) instead of a conversation ring, so every enrolled
device opens them and the DS cannot; their references carry `kv: 0`. Remote-first writes, local `vault_message` cache for offline reads;
the remote is the source of truth (sync replaces the cache).

- `get_vault_messages(user_id)` → `VaultMessage[]` — oldest first (chat order). Pulls
  and replaces the local cache when the DS is reachable; serves the last synced state
  when not. An entry this build cannot decrypt is hidden, never deleted. A row the
  DS has only ever inserted carries `updated_at == created_at`; a rewrite (edit or
  pin) stamps the server clock, which is what both UIs' "(edited)" keys on.
- `send_vault_message(user_id, content)` → `VaultMessage`
- `edit_vault_message(id, new_content, user_id)` → `VaultMessage` — keeps `pinned` and
  `created_at`. A file the edit drops is collected like a deleted one (below).
- `set_vault_message_pinned(id, pinned, user_id)` → `VaultMessage`
- `delete_vault_message(id, user_id)` — hard delete, remote first; releases the
  entry's attachment references, then collects every file no other local entry
  holds (#1491): `/v1/attachments/delete` for the `attachment_object` row, a
  presigned DELETE for the object, and this device's media-cache entry. Both
  server steps are conditional on the server (no live message or vault reference,
  any user), so a file something else still holds survives. Until #1491 nothing
  ever collected a deleted Vault file: the object and its row stayed for the life
  of the account.
- `search_vault_messages(query, user_id)` → `VaultMessage[]` — case-insensitive
  substring over the local cache; the server cannot search what it cannot read.

Both apps use the same commands (mobile through the `bridge.rs` arms, #1316). Desktop
is `frontend/src/pages/Vault.tsx` (sidebar row at the top of the DM list, Cmd+K
"Vault"); mobile is `mobile/app/vault.tsx` (a row at the top of the Direct tab, and a
Search-tab quick-jump), with `mobile/hooks/queries/useVault.ts` and the pure rules in
`mobile/lib/vault.ts` (`tests/vault.test.ts`). Neither UI calls
`search_vault_messages`: both filter the loaded entries client-side on the caption
and attachment filenames. An edit of an entry with files rebuilds the same `_att`
envelope around the new caption, so files survive a text edit. Mobile reaches pin /
edit / delete through a long-press sheet, edit and delete replace the composer as on
desktop, and the media view's grid (virtualised) opens the shared full-screen viewer
(`app/media.tsx`, `source=vault`), which swipes through the grid's images and videos
in grid order, newest first (#1492, `vaultViewerItems`) — desktop's
`MediaGalleryView` walks the same roll with ←/→ and its on-screen previous/next.
Mobile attaches photos from the library only (the
chat composer's picker); desktop takes any file. Maestro: `mobile/.maestro/flows/vault.yaml`,
`vault-swipe.yaml` (twelve images, swipe, position, swipe-down dismiss).

**A failed save (#1497)** says why and is retryable on both apps: the draft (text and
the SAME attachment objects) goes back into the composer, a line above it names the
reason — `vaultSaveFailure` (`mobile/lib/vault.ts`, `frontend/src/utils/vaultSaveFailure.ts`)
sorts the core's error text into offline (a connection failure or a DS/R2 5xx),
too large, or other — and "Try again" sends the restored draft. Each app's
`buildMessageContent` lets every upload settle and remembers the ones that succeeded
against their attachment object, so a retry uploads only what failed. The save path's
DS endpoints (`/v1/r2/presign`, `/v1/attachments/register`, `/v1/vault/save`,
`/v1/vault/delete`) are declared `idempotent` in `pollis-api`'s endpoint table, so
`ds_post` resends them once after a connection failure (`send_ds_resending`); the DS
treats a byte-identical re-save as a no-op (it does not mark the entry edited).
Maestro: `mobile/.maestro/flows/vault-retry.yaml` (a twelve-image entry; an offline
save and its retry on Android); Playwright: `e2e/vault-save-failure.spec.ts`.

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
- `approve_device_enrollment(request_id, user_id, verification_code)` — `verification_code` is what the approver **TYPED**, read off the NEW device's screen, and it is checked against the code Rust derives from the ephemeral key it fetched (#793) — never against the DS's stored copy. A disagreement between the two is additionally reported as tampering rather than passed silently.
- `reject_device_enrollment(request_id, user_id)`
- `list_pending_enrollment_requests(user_id)` → `PendingEnrollmentRequest[]` — carries **no verification code** (#1096). It used to, and the approval UI displayed that value and submitted it straight back, so the SAS comparison was performed by nobody: a DS that swapped the new device's ephemeral key and its stored code to match satisfied `approve_device_enrollment`'s derived check too, because both sides of it came from the server. Only the human comparing two screens caught it, and one tap handed over the account private key. Leaving the field out of the type is what makes the pre-fill unrepresentable — there is nothing to pre-fill with. `RealtimeEvent::EnrollmentRequested` drops it for the same reason (the DS's packet still has one, for shipped clients; the dispatch does not read it).

  The approving UI is `EnrollmentApprovalPrompt` (desktop) and the pending card on `self/security` (mobile): an eight-character field, approve disabled until it is full, submitted verbatim, cleared on a mismatch. The input rule — upper-case, drop anything outside Rust's Crockford-style alphabet, never map a lookalike (`O`→`0` would let a mis-read code compare equal), truncate to eight — is `frontend/src/utils/enrollmentSas.ts` and its mobile twin `mobile/lib/enrollmentSas.ts`, each pinned against the Rust constants by a unit test. Proofs: `e2e/enrollment-approval.spec.ts` (desktop, five specs; four of them fail if the code is pre-filled) and `mobile/.maestro/flows/enrollment-approval.yaml` (two-client).
- `recover_with_secret_key(user_id, secret_key)` — same handoff pattern as the approval path.
- `reset_identity_and_recover(user_id, email)` — soft recovery; `reset_identity` populates `AppState.unlock` with the new keypair before this command's local-DB cleanup runs.
- `finalize_device_enrollment(user_id)` — call after `set_pin` completes. Publishes the device cert + a fresh MLS key package, then external-joins every existing group / DM the device isn't in yet. Idempotent for fresh signup.
- `list_user_devices(user_id)` → `DeviceInfo[]`
- `revoke_device(user_id, device_id)` — tombstones the device (`POST /v1/devices/revoke`), writes the `device_revoked` security event, sends the `device_revoked` inbox nudge, and **returns**. Removing its leaf from every group and DM tree (one reconcile commit per conversation) runs detached afterwards (`spawn_revoked_leaf_removal`), four conversations at a time, and revokes coalesce: one made while a pass runs only queues one more pass, and each pass removes every device revoked by then, so clearing many stale devices costs about two passes. The tombstone is what cuts the device off, since the DS refuses everything it signs from then on, and every member's MLS sweep evicts a revoked leaf anyway. It used to wait for all of those commits, which held "Revoking…" for minutes on a slow link and made clearing several stale devices impractical.
- `is_current_device_registered(user_id)` → `bool`. The authoritative re-check behind the `device_revoked` inbox nudge: the nudge is per-user so it reaches every device, and only the one that answers `false` signs out (a forged nudge can't evict a valid device). `false` requires an ACTIVE row — revocation is a tombstone, so this filters `revoked_at IS NULL` (#685). Returns `true` when the device id isn't known yet, so early boot never self-logs-out. A revoked device's own signed read is answered `device_revoked`, which maps to `false` here, not an error (#1338).
- **Revoked device (#1338).** The DS answers a request signed by a revoked device's own key with 401 `{"error":"device_revoked"}` plus `x-pollis-auth-error: device_revoked` (an unknown device or a bad signature keeps the bare `unauthorized`, so only the key holder learns it). `ds_client` turns that into `Error::DeviceRevoked` and records `AppState.revoked_device` — bound to the exact `(user_id, device_id)` that signed the refused request, and only while that device is still this process's device (`note_revoked_response`), so a response landing after a sign-out or another account's sign-in marks nobody. Only that device's later signed calls short-circuit. `logout` wipes (keys, device id, local DB) whatever `delete_data` says only when the account being signed out is the revoked one, and always clears the record, so the next sign-in is a new device. The DS reads the revoked key in the same single lookup as a live one and verifies either way, so the refusal's timing reveals nothing. The hosts notice the error (exact `device_revoked`, or `…: device_revoked`) in one place each — desktop `frontend/src/bridge/invoke.ts`, mobile `mobile/lib/deviceRevoked.ts` via `lib/native/bridge.ts` — and run one handler that first asks `is_current_device_revoked` (a stray or late rejection never ends a healthy session), then signs out with `deleteData: false` (the core decides the wipe) and shows `auth:login.deviceRemoved` on sign-in; the live nudge and desktop's startup check call the same handler (`deviceWasRevoked()`).
- `is_current_device_revoked()` → `bool`. Whether this process's current device is the one the DS answered `device_revoked` for (#1338). Tests: `pollis-delivery/tests/auth.rs`, flows `auth::a_revoked_device_that_missed_the_nudge_is_told_and_wiped`, `e2e/device-revoked.spec.ts`, `mobile/tests/device-revoked.test.ts`, Maestro `scripts/maestro-device-revoked.sh`.
- `reset_identity(user_id)` → new secret key. **Not a registered Tauri command** as of 2026-08-03 (#714): it is `pollis_core::commands::account_identity::reset_identity`, reached from the registered `reset_identity_and_recover`.
- Also registered in this module but undocumented here: `list_security_events`. See Appendix A.

## device_link (`commands/device_link.rs`) — QR device link (#1207, #1361)

Sign a new device in by QR instead of the email OTP plus a typed SAS, in either direction: v1, an enrolled device shows the QR and the new device scans it (desktop pastes its code, having no scanner), or v2, the NEW device shows the QR and a signed-in phone scans it. Protocol, threat model and guardrails: `docs/qr-device-link-design.md`. Mobile no longer shows a v1 code; its Security screen is the v2 scanner.

- `create_device_link(user_id, pin)` → `DeviceLinkHandle { link_id, qr_payload, expires_at }`. **Requires the PIN** (`pin::verify_pin`, same attempt counter and lockout as unlock). Holds the link token in memory only; the DS gets `SHA-256(HKDF(t, "pollis-link-claim-v1"))` and a server-set 60 s expiry.
- `await_device_link(user_id, link_id, since)` → `DeviceLinkStatus` (`open | claimed | ready_to_approve | tampered | expired`). One awaited call with Rust-side backoff — the renderer never polls. `ready_to_approve` means the request's **link tag verified** against the QR-derived MAC key the DS never sees; `tampered` means it did not, and is never approvable. `poll_device_link` is the one-shot read.
- `approve_device_link(user_id, link_id)` — re-verifies the tag, then runs the same wrap as typed-code approval (`device_enrollment::wrap_and_approve`); security event `device_enrolled` with `via=qr_link`.
- `cancel_device_link(link_id)` — forget the token locally.
- `claim_device_link(payload, device_name)` → `UserProfile` (`enrollment_required = true`). Signs in with a **`DeviceLink`-scoped session**: the DS's OTP-only gates (soft reset, reset-and-recover, establish-identity) refuse it; it can only register the device and file its enrollment request, which `start_device_enrollment` then tags. Clears on finalize and on any email sign-in. Refuses a v2 code with "scan it with your signed-in phone".

**v2, the NEW device shows the QR (#1361)** — `docs/qr-device-link-design.md` § v2:

- `create_device_offer(device_name)` → `DeviceOfferHandle { link_id, qr_payload, expires_at }`. New device, no credential. `qr_payload` = `pollis-link:v2:<link_id>:<base64url(ephemeral X25519 pub)>`, **no secret**. Plain text, **never a link**: no app registers `pollis-link`, so the system camera cannot open Pollis with it (`mobile/tests/link-codes.test.ts` pins this). The poll secret and the private key stay in memory (`state.device_offer`); the DS gets `SHA-256(poll_secret)` and sets a 120 s expiry. Replaces any earlier offer.
- `await_device_offer(link_id)` → `DeviceOfferWait` (`signed_in { profile } | expired`). One awaited call with Rust-side backoff, bounded by the offer's lifetime. On `signed_in` the device holds a **`DeviceOffer`-scoped session** (enrollment-only, like `DeviceLink`), and `start_device_enrollment` files its request with the QR's key and no tag.
- `cancel_device_offer()` — forget the offer locally.
- `accept_device_offer(user_id, pin, payload)` → `AcceptedDeviceOffer { link_id, new_device_id, device_name }`. Signed-in scanner. **Requires the PIN** (`pin::verify_pin`) before anything is sent; binds the offer to this account, once. Refuses a v1 code with "enter it on the device you're signing in".
- `finish_device_offer(user_id, link_id)` → `DeviceOfferOutcome` (`linked { device_name } | tampered | expired`). Waits for the request and wraps the account key only if the request's ephemeral key is **byte for byte the scanned one**; otherwise it reports `tampered` and wraps nothing. Security event `device_enrolled` with `via=qr_scan`.

## livekit (`commands/livekit/`)
- Tokens are minted by the DS now (#393) — no on-device signer. `get_livekit_token` and friends call `ds_livekit_token` (`POST /v1/livekit/token`); server-side fan-out/roster go through `ds_livekit_send_data` / `ds_livekit_participants`. The client holds no LiveKit API secret.
- `get_livekit_token(room_id, user_id, username)` → token string (identity/name derived server-side; the args are ignored)
- `subscribe_realtime(on_event: Channel)`
- `connect_rooms(room_ids, user_id, username)`

## voice (`commands/voice/`)
**Participant identity** is per-device: `voice-{user_id}:{device_id}` (device_id from `AppState.device_id`; falls back to legacy `voice-{user_id}` pre-login). This lets two devices of the same user coexist in one room instead of colliding on the SFU (#140) — mirrors the realtime/inbox scheme in `livekit/realtime.rs`. Parse the user back out with `voice::user_id_from_voice_identity` (Rust) / `voice/identity.ts` (frontend). The voice event loop **does not play back** audio tracks whose parsed user_id is the local user's (self-hear mute) but still shows them as participants. `RoomEvent::Disconnected` now fully tears down the room + mic stream via `release_voice_resources` (previously leaked).
- `prepare_voice_connection(channel_id, user_id, display_name)` — best-effort warmup fired on user "intent" (hover, route entry). Pre-fetches + caches the DS-minted voice token (identity derived server-side, so it matches the join) and runs a one-shot HTTPS probe to warm DNS / TLS / connection pool. Idempotent; safe to call eagerly. Consumed by the next `join_voice_channel` for the same channel + identity.
- `join_voice_channel(channel_id, user_id, display_name, input_device, output_device, audio_processing)` — connect to LiveKit and publish the local mic. `audio_processing` is the `ApmConfig` struct (AGC + NS + AEC settings) — see [Audio Processing](./audio-processing.md). Consumes a fresh warmup if present and runs `Room::connect` + cpal mic init concurrently to minimise cold-start latency.

  Join latency is dominated by **serialized DS round trips**, not by LiveKit. In order: the token mint (0 when a warmup is consumed) → `derive_voice_key` (MLS group resolve, then the catch-up and the published-GroupInfo read *concurrently*) → `Room::connect` ‖ mic init → publish → the seed roster (pseudonym resolve + **one** batched avatar lookup for the local user and every existing participant together). The avatar lookup used to be one request *per person already in the channel*, which made the busiest rooms the slowest to enter. Note that `prepare_voice_connection` warms only the token and DNS/TLS — it does **not** pre-pay the MLS phase, so `e2ee_ms` is unaffected by hovering.
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
- `get_last_join_timings()` — debug: most recent `JoinTimings` record (jwt mint, **e2ee**, room connect, mic init, first publish, **roster**, total). `e2ee_ms` is the MLS resolve + catch-up + exporter-secret derivation, which runs *before* the LiveKit connect because the key has to be in `RoomOptions`; `roster_ms` is the pseudonym resolution + the one batched avatar lookup that seeds the participant list. Those two phases are where a slow join usually lives — `room_connect_ms` and `mic_init_ms` overlap each other, so `total` minus them is mostly DS round trips.

## r2 (`commands/r2.rs`)
- `upload_file(data, key, content_type)` → URL — caller-chosen key. Legacy; new public objects should use `upload_public_file`.
- `download_file(key)` → bytes — returned over the JSON IPC as an array of integers (~3x inflation). Now only the FALLBACK path for legacy non-content-addressed keys; see `get_public_file_url`.
- `upload_public_file(prefix, data, content_type)` → `UploadResult` — uploads an avatar / group icon under a CONTENT-ADDRESSED key `{prefix}/{sha256}.{ext}` (#874). Avatars used to live at a stable `avatars/{user_id}` overwritten in place, and a mutable key is what made a local cache impossible: nothing short of re-fetching could tell you whether a cached copy was current, so every viewer re-downloaded every avatar on every launch. A new avatar is now a new key, which the profile row already publishes.
- `get_public_file_url(key)` → URL — resolves a public object to a loopback media-server URL, caching the bytes on disk encrypted at rest under the same content-addressed scheme attachments and custom emoji use. The key IS the expected digest, so the integrity check is free and mandatory (these objects are stored unencrypted). Returns `""` — the same sentinel `get_media_url` uses — for a LEGACY key that carries no digest, or when the media server isn't up; the frontend falls back to `download_file` for those.
- ~~`presign_upload(key, content_type)`~~ — **does not exist** under that name as of 2026-08-03 (#714). Presigning is server-side: the DS secrets broker mints URLs via `POST /v1/r2/presign` (see `delete_r2_object` below and `docs/secrets-broker.md`).
- `get_media_url(r2_key, content_hash, content_type, sha256, key_scope)` → URL — builds the loopback media-server URL (`http://127.0.0.1:<port>/<token>/<hash>`) for a cached item; errors if the server isn't started or there is no active unlock. See `pollis-core/src/media_server.rs`.
- `upload_media(path, filename, content_type)` / `download_media(r2_key, content_hash, sha256, key_scope)` — the sealed media path (#1382; see `file_keys` below). An upload picks a random 32-byte file id, seals the file (AES-256-GCM, 4 MiB chunks) under `HKDF(salt = file id, ikm = newest ring key of the conversation's file key ring)` — or the Vault file root for a Vault upload — PUTs it to `media/{hex file id}.enc` and registers the object with the DS (`attachment_object`, for reference counting only; nothing there is key material). `MediaUploadResult` carries `content_hash` (the file id — named for the `_att` `hash` wire field, NOT derived from content), `key_version` and `sha256`; the `_att` reference carries them as `hash`, `kv`, `sha`. `key_scope` is a tagged enum: `{ kind: "conversation", conversation_id, key_version }` or `{ kind: "vault" }`. A download refuses a reference without a key version (pre-#1382 convergent objects; no fallback) and refuses bytes whose SHA-256 differs from `sha256`. No cross-user dedup: every upload is a new object. A file over `R2_MEDIA_MAX_BYTES` (104 MiB) is refused from its metadata before it is read (#1491) — reading first held a 2 GB pick in memory just to say no. Hashing, image metadata (the blurhash is computed from a ≤64 px thumbnail; width/height stay the original's) and encryption run on the blocking pool, never on the bridge's four async workers, and at most three uploads hold their bytes at once (`MAX_CONCURRENT_UPLOADS`) — a twelve-image Vault entry used to park every worker in image decoding and starve its own HTTP requests (#1497). `path` is a file the **user** already has (picker selection, OS drag-and-drop); nothing in the app ever writes a file in order to have a path to pass here. **The shim enforces that** — see "Path-taking commands are scoped to what the user picked" below.
- `save_media_to_path(r2_key, content_hash, sha256, key_scope, target_path)` → `"marked"` | `"unsupported"` | `"failed: …"` — "save attachment as…". Downloads, decrypts, verifies `sha256` and writes the file in Rust, then applies this platform's download marker (macOS `com.apple.quarantine`, Windows `Zone.Identifier`). The renderer used to `fetch` the loopback URL and `writeFile` the bytes, which left the file with no provenance marker at all — Gatekeeper and SmartScreen both treat an unmarked file as locally authored. A marker that cannot be written (FAT32, a network share) does NOT fail the save; the return value says what happened. `target_path` is gated by `PathScope`. See `.codesight/wiki/media-server.md`.
- `upload_media_staged(staged_id, filename, content_type)` — the same upload for bytes with no path: a paste, or a drop the webview surfaced as a `File`. Reads them from the in-memory staging registry (`commands/staging.rs`) and releases them **only on success**, so a failed upload is a retry rather than a lost paste.
- Internal: `delete_r2_object(state, r2_key)` — DS-presigned DELETE (via `presign_r2`) used by `delete_message` to purge orphaned attachments. Treats 404 as success. The client holds no R2 credentials — every get/put/delete is presigned by the DS secrets broker (`POST /v1/r2/presign`, #393).

### Path-taking commands are scoped to what the user picked

Four commands take a filesystem path as an IPC argument — `upload_media`,
`upload_group_emoji`, `export_archive`, `fetch_export_attachments` — and act on
it with the app process's full authority. They are the only four, and each one's
`#[tauri::command]` shim in `src-tauri/src/commands/` calls
`PathScope::require` before forwarding to `pollis-core`. A path that is not in
the scope is refused with *"that location was not chosen in a file dialog on
this device"*, and nothing is read or written.

`src-tauri/src/pathscope.rs` owns the registry. It is **written only from inside
the process**, which is the whole point — a "register this path for me" command
would be one a compromised renderer calls with `~/.ssh/id_rsa`:

| writer | when |
| --- | --- |
| `commands::pathscope::pick_open_paths` | the OS open/folder picker, driven from Rust |
| `commands::pathscope::pick_save_path` | the OS save panel, driven from Rust |
| `pathscope::remember_dropped` | the `DragDrop` window event in `run()`, with the paths the OS put in it |

The renderer's `bridge/dialog.ts` calls the two `pick_*` commands; `dialog:*` is
no longer in `src-tauri/capabilities/default.json`, so `plugin:dialog|open` is
unreachable from the webview and there is no way to obtain a path Rust did not
witness. Directories are stored as prefixes (an export's sidecar `files/` lives
under the folder the user chose) and `..` is collapsed lexically before the
prefix test, so a picked directory cannot be walked out of. A save target is
stored as given rather than canonicalized, because it does not exist yet —
which is why Tauri's own `fs::Scope` could not be used for this.

Mobile is exempt: it reaches the same `pollis-core` implementations through the
uniffi bridge, where the OS app sandbox already bounds what a path can name.

`tests/flows/security.rs::path_taking_commands_refuse_a_path_the_user_never_chose`
drives all four through the real IPC pipeline with an empty scope.

## staging (`commands/staging.rs`)
In-memory custody of attachment bytes that arrive through the webview with no filesystem path (#1000). The renderer used to manufacture one by writing the raw file into the OS temp directory as `pollis-<ts>-<original filename>`; nothing ever deleted it, so the plaintext of every file a user had ever pasted stayed there under its own name. There is no file now — bytes go into a process-global registry keyed by an opaque id, bounded by `STAGED_MAX_TOTAL_BYTES` (512 MiB, which refuses rather than evicting), and released on upload success, on removal, on composer unmount, and wholesale by `lock` / `logout` / `wipe_local_data`.
- `stage_attachment(<raw IPC body>)` → `{ id, size_bytes }` — the payload is the `Uint8Array` itself (`tauri::ipc::Request`, same shape as `terminal_write`), not a JSON number array.
- `discard_staged_attachment(staged_id)` → `bool` — release bytes the composer will not send.

## emoji (`commands/emoji.rs`)
Custom per-group emoji (#848). Remote metadata lives in `custom_emoji_object` (content-addressed, one row per stored image) + `group_emoji` (one row per group that uses it); writes go through the DS (`POST /v1/emoji/{create,remove,gc}`). The objects are **unencrypted** in R2, deliberately — see the module docs and migration `000015_custom_emoji.sql`. `create` names its actor as `created_by` and `remove` as `actor_id` (both filled from `current_user_id`) — neither did before #875, so both 403'd on an unsigned deployment; see "Every DS write body must NAME ITS ACTOR" at the top of this page.
- `list_usable_emoji(user_id)` → `CustomEmoji[]` — every custom emoji the user may SEND, i.e. every emoji registered to any group they are a current member of. Discord's rule: usable in ANY conversation, not just the registering group. This is both the picker's source and the permission predicate — deliberately one query, so the set offered and the set permitted cannot drift.
- `list_group_emoji(group_id, requester_id)` → `CustomEmoji[]` — one group's emoji (the management page). **#917: members-only**, and this does *not* contradict #848. #848 requires that an emoji from a group you are in none of still **renders**; that is discharged entirely by `get_emoji_url`, which takes a content hash, no group, and no membership check. *Resolving one hash you were handed* and *enumerating a group's emoji set* are different acts — the second is group metadata (shortcodes name things, `created_by` names members). So the hash stays open and the list closes, the narrowest rule that keeps cross-group rendering intact. The picker was never affected: `list_usable_emoji` has always joined through `group_member`.
- `upload_group_emoji(group_id, shortcode, path)` → `CustomEmoji` — reads the file from disk (no bytes over IPC), **re-encodes** it to WebP/GIF under 48 KiB (`encode_emoji`), content-hashes the *re-encoded* bytes, skips the R2 PUT on a dedup hit, uploads under a length-bound presign, then registers via the DS. `shortcode` must be `[a-z0-9_]{2,32}`.
- `remove_group_emoji(group_id, shortcode)` — DS remove (group admins only — #1280; any member may upload), then an event-driven GC sweep that deletes any now-unreferenced R2 blob.
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

Mechanically extracted from `tauri::generate_handler![…]` in `src-tauri/src/lib.rs` at `77abb2b` on **2026-08-03** (#714), plus the two `relay_serving` commands added by #813, the six `emoji` commands added by #848, the two `messages` receipt commands added by #857, the two `autolock` commands added by #851, and the four added by #874 (`messages::read_last_messages`, `r2::upload_public_file`, `r2::get_public_file_url`, `device_enrollment::await_enrollment_approval`), and the three added by #1000 (`r2::upload_media_staged`, `staging::stage_attachment`, `staging::discard_staged_attachment`), and the nine added by #99/#107 (`pinned_messages::{pin_message, unpin_message, list_pinned_messages}`, `vault::{get_vault_messages, send_vault_message, edit_vault_message, set_vault_message_pinned, delete_vault_message, search_vault_messages}`). **212 commands.** Grouped by the `commands::<module>::` path used at the registration site; **names only — no descriptions are given here because they were not verified.** A name in this list that has no prose above is real and callable; read its implementation in `pollis-core/src/commands/` before using it.

Regenerate with:

```bash
sed -n '/generate_handler!\[/,/^\s*\]) *$/p' src-tauri/src/lib.rs
```

- **`(lib.rs, no module)`** — `hide_window`, `read_clipboard_files`, `read_clipboard_image`
- **`autolock`** — `report_user_activity`, `set_auto_lock_timeout`
- **`auth`** — `delete_account`, `dev_login`, `get_session`, `initialize_identity`, `is_current_device_registered`, `list_known_accounts`, `list_user_devices`, `logout`, `request_email_change_otp`, `request_otp`, `revoke_device`, `verify_email_change`, `verify_otp`, `wipe_local_data`
- **`blocks`** — `block_user`, `list_blocked_users`, `unblock_user`, `report_user`
- **`bookmarks`** — `list_saved_messages`, `resolve_message_permalink`, `save_message`, `toggle_saved_message`, `unsave_message`
- **`camera`** — `list_video_devices`, `start_camera`, `start_camera_preview`, `stop_camera`, `stop_camera_preview`, `subscribe_camera_events`
- **`device_enrollment`** — `approve_device_enrollment`, `await_enrollment_approval`, `finalize_device_enrollment`, `list_pending_enrollment_requests`, `list_security_events`, `poll_enrollment_status`, `recover_with_secret_key`, `reject_device_enrollment`, `reset_identity_and_recover`, `start_device_enrollment`
- **`dm`** — `accept_dm_request`, `add_user_to_dm_channel`, `create_dm_channel`, `get_dm_channel`, `leave_dm_channel`, `list_dm_channels`, `list_dm_requests`, `remove_user_from_dm_channel`
- **`emoji`** — `get_emoji_url`, `list_group_emoji`, `list_usable_emoji`, `prepare_emoji_text`, `remove_group_emoji`, `upload_group_emoji`
- **`groups`** — `accept_group_invite`, `approve_join_request`, `create_channel`, `create_group`, `create_group_invite_link`, `decline_group_invite`, `delete_channel`, `delete_group`, `get_group_join_requests`, `get_group_members`, `get_my_join_request`, `get_pending_invites`, `leave_group`, `list_group_channels`, `list_group_invite_links`, `list_group_invites`, `list_muted_channels`, `list_user_groups`, `list_user_groups_with_channels`, `redeem_group_invite_link`, `reject_join_request`, `remove_member_from_group`, `request_group_access`, `revoke_group_invite`, `revoke_group_invite_link`, `search_group_by_slug`, `send_group_invite`, `set_channel_muted`, `set_member_role`, `update_channel`, `update_group`
- **`install_kind`** — `detect_managed_install`, `detect_read_only_location` (macOS: running from the DMG or translocated, so the updater would hit EROFS; a translocation caused by an older release's own quarantine stamp is repaired in place and reported as `translocated_healed`)
- **`livekit`** — `cancel_call`, `connect_rooms`, `get_livekit_token`, `get_livekit_url`, `get_livekit_view_token`, `list_voice_participants`, `list_voice_room_counts`, `publish_ping`, `publish_typing`, `publish_voice_presence`, `start_call`, `subscribe_realtime`
- **`media_permissions`** — `get_media_permission_status`, `open_privacy_settings`, `revoke_media_permissions`, `set_revoke_media_on_exit`
- **`messages`** — `add_reaction`, `delete_message`, `edit_message`, `get_channel_messages`, `get_dm_messages`, `get_message_retention`, `get_reactions`, `get_unread_counts`, `ingest_channel_envelopes`, `ingest_dm_envelopes`, `list_messages`, `list_thread_summaries`, `mark_conversation_read`, `read_channel_messages`, `read_dm_messages`, `read_last_messages`, `read_messages_after`, `read_messages_around`, `read_thread_messages`, `rebuild_search_index`, `remove_reaction`, `run_message_eviction`, `search_messages`, `send_message`, `set_message_retention`, `sync_read_cursors`, plus the outbox (#1335): `discard_unsent_message`, `flush_outbox`, `new_message_id`, `retry_message`
- **`mls`** — `catch_up_all_mls_groups`, `poll_mls_welcomes`, `process_pending_commits`
- **`overlay`** — `get_overlay_mode`, `set_overlay_mode`
- **`pathscope`** — `pick_open_paths`, `pick_save_path`
- **`pin`** — `get_unlock_state`, `lock`, `set_pin`, `unlock`
- **`pinned_messages`** — `list_pinned_messages`, `pin_message`, `unpin_message`
- **`r2`** — `download_file`, `download_media`, `get_media_url`, `get_public_file_url`, `save_media_to_path`, `upload_file`, `upload_media`, `upload_media_staged`, `upload_public_file`
- **`staging`** — `discard_staged_attachment`, `stage_attachment`
- **`relay_serving`** — `get_relay_serving_status`, `set_relay_serving`
- **`safety`** — `get_safety_number`, `list_peer_verifications`, `set_contact_verified`
- **`screenshare`** — `cancel_screen_share_picker`, `enumerate_screen_sources`, `screenshare_ws_url`, `start_screen_share`, `stop_screen_share`, `subscribe_screen_share_events`, `subscribe_screen_share_frames`
- **`sfx`** — `play_sfx`, `start_ring`, `stop_ring`
- **`terminal`** — `get_terminal_enabled`, `set_terminal_enabled`, `terminal_ack`, `terminal_close`, `terminal_open`, `terminal_resize`, `terminal_write`
- **`transparency`** — `audit_peer_account_key`, `self_audit_account_key`, `verify_own_build`
- **`tray`** — `tray_set_close_to_tray`, `tray_set_enabled`, `tray_set_unread`, `tray_set_voice_state`
- **`update`** — `get_update_check_plan`, `is_update_required`, `mark_update_required`
- **`user`** — `get_preferences`, `get_user_profile`, `save_preferences`, `search_user_by_username`, `update_user_profile`
- **`vault`** — `delete_vault_message`, `edit_vault_message`, `get_vault_messages`, `search_vault_messages`, `send_vault_message`, `set_vault_message_pinned`
- **`voice`** — `get_last_join_timings`, `get_voice_gate_state`, `join_voice_channel`, `leave_voice_channel`, `list_audio_devices`, `prepare_voice_connection`, `release_voice_ptt`, `set_remote_user_volume`, `set_voice_audio_processing`, `set_voice_input_device`, `set_voice_input_mode`, `set_voice_output_device`, `set_voice_ptt_held`, `subscribe_voice_events`, `toggle_voice_deafen`, `toggle_voice_mute`
- **`voice_test`** — `play_test_tone`, `record_and_play_back`, `set_mic_test_monitor`, `start_mic_test`, `stop_mic_test`, `stop_test_playback`, `subscribe_voice_test_events`

If this appendix and `src-tauri/src/lib.rs` disagree, `lib.rs` is right and this file is stale.

---
_Back to [index.md](./index.md)_

---

## Complete registered-command index

Generated from `src-tauri/src/lib.rs`'s `invoke_handler!` — **226 commands** in 34 shim modules.
Prose above covers roughly half of these; this index covers all of them, so a name that
appears here but not above is registered and real, just undocumented. Regenerate rather
than hand-edit.

**`(root)`** (4) — `hide_window`, `read_clipboard_files`, `read_clipboard_image`, `write_clipboard_text`

**`media_permissions`** (4) — `get_media_permission_status`, `open_privacy_settings`, `revoke_media_permissions`, `set_revoke_media_on_exit`

**`auth`** (14) — `delete_account`, `dev_login`, `get_session`, `initialize_identity`, `is_current_device_registered`, `list_known_accounts`, `list_user_devices`, `logout`, `request_email_change_otp`, `request_otp`, `revoke_device`, `verify_email_change`, `verify_otp`, `wipe_local_data`

**`pin`** (4) — `get_unlock_state`, `lock`, `set_pin`, `unlock`

**`autolock`** (2) — `report_user_activity`, `set_auto_lock_timeout`

**`device_enrollment`** (10) — `approve_device_enrollment`, `await_enrollment_approval`, `finalize_device_enrollment`, `list_pending_enrollment_requests`, `list_security_events`, `poll_enrollment_status`, `recover_with_secret_key`, `reject_device_enrollment`, `reset_identity_and_recover`, `start_device_enrollment`

**`device_link`** (11) — `accept_device_offer`, `approve_device_link`, `await_device_link`, `await_device_offer`, `cancel_device_link`, `cancel_device_offer`, `claim_device_link`, `create_device_link`, `create_device_offer`, `finish_device_offer`, `poll_device_link`

**`safety`** (3) — `get_safety_number`, `list_peer_verifications`, `set_contact_verified`

**`transparency`** (3) — `audit_peer_account_key`, `self_audit_account_key`, `verify_own_build`

**`overlay`** (2) — `get_overlay_mode`, `set_overlay_mode`

**`relay_serving`** (2) — `get_relay_serving_status`, `set_relay_serving`

**`user`** (5) — `get_preferences`, `get_user_profile`, `save_preferences`, `search_user_by_username`, `update_user_profile`

**`groups`** (31) — `accept_group_invite`, `approve_join_request`, `create_channel`, `create_group`, `create_group_invite_link`, `decline_group_invite`, `delete_channel`, `delete_group`, `get_group_join_requests`, `get_group_members`, `get_my_join_request`, `get_pending_invites`, `leave_group`, `list_group_channels`, `list_group_invite_links`, `list_group_invites`, `list_muted_channels`, `list_user_groups`, `list_user_groups_with_channels`, `redeem_group_invite_link`, `reject_join_request`, `remove_member_from_group`, `request_group_access`, `revoke_group_invite`, `revoke_group_invite_link`, `search_group_by_slug`, `send_group_invite`, `set_channel_muted`, `set_member_role`, `update_channel`, `update_group`

**`dm`** (8) — `accept_dm_request`, `add_user_to_dm_channel`, `create_dm_channel`, `get_dm_channel`, `leave_dm_channel`, `list_dm_channels`, `list_dm_requests`, `remove_user_from_dm_channel`

**`blocks`** (3) — `block_user`, `list_blocked_users`, `unblock_user`

**`bookmarks`** (5) — `list_saved_messages`, `resolve_message_permalink`, `save_message`, `toggle_saved_message`, `unsave_message`

**`export`** (2) — `export_archive`, `fetch_export_attachments`

**`pinned_messages`** (3) — `list_pinned_messages`, `pin_message`, `unpin_message`

**`vault`** (6) — `delete_vault_message`, `edit_vault_message`, `get_vault_messages`, `search_vault_messages`, `send_vault_message`, `set_vault_message_pinned`

**`messages`** (32) — `add_reaction`, `delete_message`, `discard_unsent_message`, `edit_message`, `flush_outbox`, `get_channel_messages`, `get_conversation_receipts`, `get_dm_messages`, `get_message_retention`, `get_reactions`, `get_unread_counts`, `ingest_channel_envelopes`, `ingest_dm_envelopes`, `list_messages`, `list_thread_summaries`, `mark_conversation_read`, `mark_messages_read`, `new_message_id`, `read_channel_messages`, `read_dm_messages`, `read_last_messages`, `read_messages_after`, `read_messages_around`, `read_thread_messages`, `rebuild_search_index`, `remove_reaction`, `retry_message`, `run_message_eviction`, `search_messages`, `send_message`, `set_message_retention`, `sync_read_cursors`

**`mls`** (3) — `catch_up_all_mls_groups`, `poll_mls_welcomes`, `process_pending_commits`

**`livekit`** (12) — `cancel_call`, `connect_rooms`, `get_livekit_token`, `get_livekit_url`, `get_livekit_view_token`, `list_voice_participants`, `list_voice_room_counts`, `publish_ping`, `publish_typing`, `publish_voice_presence`, `start_call`, `subscribe_realtime`

**`emoji`** (6) — `get_emoji_url`, `list_group_emoji`, `list_usable_emoji`, `prepare_emoji_text`, `remove_group_emoji`, `upload_group_emoji`

**`r2`** (9) — `download_file`, `download_media`, `get_media_url`, `get_public_file_url`, `save_media_to_path`, `upload_file`, `upload_media`, `upload_media_staged`, `upload_public_file`

**`staging`** (2) — `discard_staged_attachment`, `stage_attachment`

**`update`** (3) — `get_update_check_plan`, `is_update_required`, `mark_update_required`

**`install_kind`** (2) — `detect_managed_install`, `detect_read_only_location`

**`voice`** (16) — `get_last_join_timings`, `get_voice_gate_state`, `join_voice_channel`, `leave_voice_channel`, `list_audio_devices`, `prepare_voice_connection`, `release_voice_ptt`, `set_remote_user_volume`, `set_voice_audio_processing`, `set_voice_input_device`, `set_voice_input_mode`, `set_voice_output_device`, `set_voice_ptt_held`, `subscribe_voice_events`, `toggle_voice_deafen`, `toggle_voice_mute`

**`voice_test`** (7) — `play_test_tone`, `record_and_play_back`, `set_mic_test_monitor`, `start_mic_test`, `stop_mic_test`, `stop_test_playback`, `subscribe_voice_test_events`

**`screenshare`** (7) — `cancel_screen_share_picker`, `enumerate_screen_sources`, `screenshare_ws_url`, `start_screen_share`, `stop_screen_share`, `subscribe_screen_share_events`, `subscribe_screen_share_frames`

**`camera`** (6) — `list_video_devices`, `start_camera`, `start_camera_preview`, `stop_camera`, `stop_camera_preview`, `subscribe_camera_events`

**`sfx`** (3) — `play_sfx`, `start_ring`, `stop_ring`

**`pathscope`** (2) — `pick_open_paths`, `pick_save_path`

**`terminal`** (7) — `get_terminal_enabled`, `set_terminal_enabled`, `terminal_ack`, `terminal_close`, `terminal_open`, `terminal_resize`, `terminal_write`


_Back to [index.md](./index.md)_
