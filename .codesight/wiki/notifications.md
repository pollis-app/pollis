# Notifications & Sound

How sound effects, OS notifications, unread badges, status-bar alerts, and overlay prompts are routed in Pollis.

## TL;DR

Every user-facing alert goes through one function: `notify(category, payload)` in `frontend/src/utils/notify.ts`. The function looks up a flat config table to decide which outputs to fire (sound, OS notification, badge, status-bar alert, overlay), applies cooldown + user prefs + OS permission, and dispatches.

To change behavior, edit one row in `CATEGORIES`. To add a new alert type, add one row.

## Architecture

```
RealtimeEvent (Rust → JS Channel)         Local action (e.g. self voice join)
            │                                          │
            ▼                                          ▼
   useLiveKitRealtime.ts                     voice/voiceBridge.ts
   classify event → call notify()       call notify('voice_self_join')
                              │                │
                              ▼                ▼
                      ┌──────────────────────────────┐
                      │      notify(category, payload)│  ← frontend/src/utils/notify.ts
                      │  • lookup CATEGORIES[cat]    │
                      │  • check pref + OS permission│
                      │  • check cooldown            │
                      │  • fire each output          │
                      └──────────────────────────────┘
                              │       │       │       │       │
                              ▼       ▼       ▼       ▼       ▼
                          play_sfx  notify  badge   alert   overlay
                          (Rust)   (Rust)   (MobX)    (MobX)    (MobX)
```

There is **no parallel dispatcher in Rust**. All decisions happen in JS. Rust is just the transport for LiveKit events (pushed through the `EventSink`/`ChannelSink` over a `tauri::ipc::Channel` to the renderer's `channelOn` subscriber) and the executor for sound (`play_sfx` rodio command). OS notifications fire through the bridge — the renderer calls `sendNotification({ title, body, icon })`, which routes to `tauri-plugin-notification`.

## The category table

The single source of truth (`frontend/src/utils/notify.ts`):

```ts
const CATEGORIES: Record<Category, CategoryConfig> = {
  direct_message:    { sound: 'ping',  osNotif: true,  badge: true, alert: true, cooldownMs: 2500 },
  channel_message:   {                                  badge: true,              cooldownMs: 2500 },
  voice_other_join:  { sound: 'join'                                                              },
  voice_other_leave: { sound: 'leave'                                                             },
  voice_self_join:   { sound: 'join'                                                              },
  voice_self_leave:  { sound: 'leave'                                                             },
  dm_request:        { sound: 'ping',  osNotif: true,               alert: true                   },
  group_invite:      { sound: 'ping',  osNotif: true,               alert: true                   },
  enrollment:        { sound: 'ping',  osNotif: true,                            overlay: true    },
  incoming_call:     {                  osNotif: true,                            honorsRingtonePref: true },
  // @all in a group: the one channel event that DOES raise an OS ping (unlike
  // channel_message). Badge is left to the accompanying new_message event so a
  // connected client doesn't double-count unread.
  all_mention:       { sound: 'ping',  osNotif: true,                            cooldownMs: 2500 },
  // @username in a group (#843) — someone spoke to YOU by name, which is a
  // personal event, so it is treated like a DM rather than like channel
  // chatter: ping, OS notification, AND the status-bar alert that names the
  // sender. Deliberately louder than `all_mention` (addressed to a room, not
  // to you) — that alert is the difference.
  user_mention:      { sound: 'ping',  osNotif: true,               alert: true, cooldownMs: 2500 },
};
```

### Output fields

| Field | What it does | Pref gate | Notes |
|---|---|---|---|
| `sound` | Plays a sfx (`'ping'`/`'join'`/`'leave'`) via the `play_sfx` Rust command | `allow_sound_effects` | Cooldownable |
| `osNotif` | Fires an OS notification banner via the bridge's `sendNotification` (→ `tauri-plugin-notification`) | `allow_desktop_notifications` + OS permission | Cooldownable |
| `badge` | Increments the per-room unread count (`appStore.incrementUnread`) | none | Drives dock/taskbar badge via `useBadge` |
| `alert` | Sets the blinking status-bar alert (`appStore.setStatusBarAlert`) | none | Cleared on navigation |
| `overlay` | Sets `pendingEnrollmentApproval` so the UI takes over | none | Used only by enrollment |
| `honorsRingtonePref` | Gates the ringtone on the **device-local** call-ringtone toggle, independent of the account-level `allow_sound_effects` | `allowCallRingtone` | Used only by `incoming_call` |
| `cooldownMs` | Suppresses repeat sound + OS-notif within the window | — | Keyed by `(category, roomId)` |

### Conventions

1. **Anything with `osNotif: true` should also have `sound: 'ping'`.** Users always hear every system notification. Don't ship a silent OS banner. The one exception is `incoming_call`, which has no `sound` because it plays a *ringtone* on its own device-local gate (`honorsRingtonePref`) rather than a one-shot sfx — muting ringing on one device must not silence every other alert on it.
2. **Pings are reserved for personal events.** Channel chatter only updates the badge — noisy rooms must not become a constant ping. Mentions are the sanctioned exception, and they earn it by being addressed: `@all` to the room, `@username` to you specifically.
3. **`badge` and `alert` are never cooldown-gated.** The unread count must stay accurate; only sound and OS notifications dedupe.

## Categorization (the call site)

The "which category does this event belong to" decision lives at the call site, not in the table. This keeps the table flat and event-shaped predicates (`isOwnMessage`, `isSelected`, `isOwnVoiceChannel`) out of the config.

In `useLiveKitRealtime.ts`, the `new_message` handler categorizes:

```ts
if (isOwnMessage || isSelected || !incomingId) return;
// `title` / `body` fall back to `chat:notify.newMessageTitle` / `.newMessageBody`
// when the room name or preview is unavailable — see the localization note below.
notify(conversationId ? 'direct_message' : 'channel_message', { roomId, title, body, senderUsername });
```

The voice handler categorizes:

```ts
if (event.user_id === currentUserIdRef.current) return;          // own join → handled in voiceBridge
if (event.channel_id !== activeVoiceChannelIdRef.current) return; // different room → noise
notify(event.type === 'voice_joined' ? 'voice_other_join' : 'voice_other_leave');
```

The membership handler reads the `kind` discriminator (see below):

```ts
if (event.kind === 'invite') {
  notify('group_invite', {
    roomId: event.conversation_id,
    title: i18n.t('channels:notify.inviteTitle'),
    body: inviteBody,
    senderUsername: event.inviter_username ?? i18n.t('nav:statusBar.someone'),
  });
}
```

**Notification copy is localized (#855).** Titles and bodies come from the
translation catalogues via `i18n.t()` rather than string literals — the direct
`i18n` instance, not `useTranslation`, because a notification is produced once
and never re-rendered (see `frontend/src/i18n/README.md`). The invite body in
particular is no longer a template literal: `${inviter} invited you${groupPart}`
became three complete-sentence keys chosen by an explicit branch
(`channels:notify.inviteBody` / `inviteBodyToGroup` / `inviteBodyUnknown`),
because a sentence assembled from fragments cannot be translated into a
language that orders those fragments differently.

`group_invite`'s `alert` needs a `senderUsername` to fire the status-bar alert (the gate is `alert && roomId && senderUsername`). Both the invite and DM-request pings carry the counterparty's public username on the wire — see the payload note below. Before #396 the DM-request alert showed a `'New DM'` placeholder and group invites raised no status-bar alert at all.

### Startup / offline reconcile (#396)

`notify()` is the *only* thing that sets the status-bar alert, and it fires exclusively off a live realtime event. So a pending DM-request / group-invite that landed while the app was closed — or before `connect_rooms` subscribed the inbox room — shows up in the bottom-bar badges (`StatusBarSummary` mounts `useDMRequests` / `usePendingInvites`, both `refetchOnWindowFocus`) but never lights the alert.

`StatusBarSummary` closes this gap with a small reconcile effect that mirrors "has a pending DM-request / group-invite" into `setStatusBarAlert(...)`, naming the real requester / inviter. It reuses the already-mounted, focus/reconnect-refetching queries — no extra focus/reconnect plumbing — so it re-evaluates at cold launch and whenever a refetch surfaces a new item. It only seeds when no alert is already showing, so a fresher event-driven alert is never clobbered and a dismissed alert isn't re-raised until the next refetch brings genuinely new pending data.

## Pref + permission flow

`useLiveKitRealtime.ts` owns the React-side state and pushes it into `notify.ts` via `setNotifyPrefs(...)`. The effect re-runs whenever `allow_sound_effects` or `allow_desktop_notifications` changes:

1. Read current prefs from `usePreferences()`.
2. Call the bridge's `isPermissionGranted` (→ `tauri-plugin-notification`) — if not granted *and* the user has notifications enabled, request permission via `requestPermission`.
3. Push `{ allowSound, allowOsNotif, osPermissionGranted }` into the dispatcher.

Result: toggling notifications "on" in Preferences → granting the OS prompt → next event uses the new state, no restart needed.

## Membership-changed `kind` discriminator

`MembershipChanged` is overloaded — it covers four wire-equivalent cases:

| Publisher | Audience | `kind` value | Notification |
|---|---|---|---|
| `send_group_invite` (`groups.rs:876`) | Invitee's personal inbox | `"invite"` | ping + OS notif |
| `approve_join_request` (`groups.rs:1177`) | Requester's personal inbox | `"approval"` | silent |
| `approve_join_request` (`groups.rs:1183`) | Group room | none | silent (refetch) |
| `accept_group_invite` (`groups.rs:951`) | Group room | none | silent (refetch) |
| `remove_member_from_group` (`groups.rs:444`) | Group room | none | silent (refetch) |
| `leave_group` (`groups.rs:515`) | Group room | none | silent (refetch) |

The Rust enum (`pollis-core/src/realtime.rs`):

```rust
MembershipChanged {
    conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    // Present on the `invite` kind (issue #396): who invited you, to where.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inviter_username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group_name: Option<String>,
},
```

Wire dispatch (`pollis-core/src/commands/livekit/mod.rs`) reads `kind` from the JSON payload. The two **private-inbox** pings are built by dedicated helpers in `livekit_signalling.rs` — `group_invite_inbox_payload` and `dm_created_inbox_payload` — rather than inline JSON, so their identity-bearing shape is unit-tested next to the `assert_no_identity` §5 tests.

**§5 exception — private inbox may carry identity.** The metadata-minimization rule (no sender/actor in a realtime ping) governs *shared-room* broadcasts. `dm_created` and the `invite` kind are sent to the recipient's single-subscriber inbox room (`inbox-{userId}`), so they deliberately carry the counterparty's **public** username (`sender_username` / `inviter_username`) + `group_name` — the same directory metadata `get_pending_invites` and the DM-request query already return. `membership_changed_payload` (the group-room broadcast) stays routing-only.

The frontend type narrows it:

```ts
| {
    type: 'membership_changed';
    conversation_id?: string | null;
    kind?: 'invite' | 'approval' | null;
    inviter_username?: string | null;
    group_name?: string | null;
  }
```

When adding a new use of `MembershipChanged`, decide whether the receiver wants a notification. If yes, define a new `kind` value, set it on the publisher, and add the corresponding `notify(...)` call in the membership handler. If no, omit `kind` and the receiver will silently invalidate queries.

## Adding a new alert

1. **Add a row to `CATEGORIES`** in `frontend/src/utils/notify.ts`:
   ```ts
   friend_online: { sound: 'ping', osNotif: true, cooldownMs: 60000 },
   ```
2. **Add the literal to the `Category` union** in the same file.
3. **Call `notify('friend_online', { ... })`** at the appropriate event handler.

That's it. No new branches, no new refs, no new pref handling. If you need a new output type (e.g., a system-tray flash), add a column to `CategoryConfig`, handle it in the dispatcher, then opt categories in.

## Cooldown semantics

Cooldown is keyed by `${category}:${roomId ?? '_global'}`. It applies only to sound and OS notification — never to badge, alert, or overlay. The cooldown timestamp is recorded only when *something actually fired* (i.e., pref + permission allowed it). This means:

- A 10-message burst from one DM pings once, OS-notifies once, but badges 10 times.
- A burst across two DM rooms pings twice (different cooldown buckets).
- If sound is disabled but OS notif is enabled, the OS notification still fires and starts the cooldown.

## Mentions (#843)

`@all` and `@username` are the only channel events that wake anyone. Both are
decided **in Rust**, in `pollis-core/src/commands/messages/send.rs`:

- `mention_audience(is_channel, content, members)` returns `Everyone` (a DM, or
  a channel `@all`), `Only(user_ids)` (a channel naming specific members), or
  `Nobody` (ordinary channel chatter). Its unit tests are the invariant: a
  per-user mention wakes exactly the people named, and a plain channel message
  wakes nobody.
- That one value drives BOTH fanouts, so they cannot drift: the inbox ping
  (`all_mention` to every member, or `user_mention` to just the named ones) and
  the background push (`notify_new_message(..., only, ...)`, which intersects
  the subset with real membership so it can only ever remove recipients).

**Mentions cannot leak.** `members` is the roster of the conversation the
sender already belongs to, so an `@name` matching nobody in the room resolves
to nothing on both the server and the client. The composer's suggestion list
is derived from the same roster (`useMentionCandidates`) — there is
deliberately no username lookup behind it, because a directory search would let
anyone enumerate users outside their own groups and DMs just by typing.

Client side, `MessageBody` tokenizes only mentions that resolve, so a
highlighted token always means a real person was actually notified. Your own
mention (and `@all`, which addresses you too) renders as a filled accent block;
someone else's renders as a quiet tinted chip.

Composer UX branches per skin (`useSkin()`): terminal gets a ghosted inline
autosuggest accepted with Tab — the fish / zsh-autosuggestions idiom, offered
only when the caret is at the end of the line — and refined gets the
Slack-style list above the composer (arrows / Enter / Tab / Esc). Neither is a
modal, a portal, or a fixed overlay.

## Files

| File | Role |
|---|---|
| `frontend/src/utils/notify.ts` | Dispatcher + category table |
| `frontend/src/utils/mentions.ts` | Mention parsing + ranking; mirrors the Rust matcher |
| `frontend/src/hooks/queries/useMentionCandidates.ts` | Roster-derived mention pool (the no-leak boundary) |
| `frontend/src/components/ui/RichTextInput.tsx` | The composer field: contentEditable projection of the wire text, and the terminal skin's inline ghost (shared with `:shortcode:` completion) |
| `frontend/src/components/ui/MentionSuggestList.tsx` | Refined skin's Slack-style suggestion list |
| `frontend/src/components/Message/MessageBody.tsx` | Splits mention tokens out, delegates the rest to `LinkifiedText` |
| `pollis-core/src/commands/messages/send.rs` | `mention_audience()` — who gets woken, + the inbox fanout |
| `pollis-core/src/commands/push.rs` | `notify_new_message(..., only, ...)` recipient filter |
| `frontend/src/utils/sfx.ts` | `playSfx()` wrapper around `play_sfx` Rust command |
| `frontend/src/hooks/useLiveKitRealtime.ts` | Categorizes incoming Rust events, calls `notify(...)`, owns pref + permission sync |
| `frontend/src/voice/voiceBridge.ts` | Calls `notify('voice_self_join'/'voice_self_leave')` for local actions (was `hooks/useVoiceChannel.ts`, deleted) |
| `frontend/src/hooks/useBadge.ts` | Reads `unreadCounts` from the MobX store, applies dock/taskbar badge |
| `pollis-core/src/realtime.rs` | `RealtimeEvent` enum (Rust → JS wire format) |
| `pollis-core/src/commands/livekit/` | `dispatch_data()` parses payloads, sends typed events to JS |
| `pollis-core/src/commands/sfx.rs` | `play_sfx` rodio implementation |
| `pollis-core/src/commands/groups/` | Membership-change publishers (set `kind` here) |

## Related issues

- #843 — per-user `@username` mentions that notify the mentioned user.
- #186 — original audit + dispatcher refactor (PR #202).
