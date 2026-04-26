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
   useLiveKitRealtime.ts                     useVoiceChannel.ts
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
                          (Rust)   (Rust)   (Zustand) (Zustand) (Zustand)
```

There is **no parallel dispatcher in Rust**. All decisions happen in JS. Rust is just the transport for LiveKit events (via `tauri::ipc::Channel`) and the executor for sound (`play_sfx` rodio command) and OS notifications (`tauri-plugin-notification`).

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
  group_invite:      { sound: 'ping',  osNotif: true                                              },
  enrollment:        { sound: 'ping',  osNotif: true,                            overlay: true    },
};
```

### Output fields

| Field | What it does | Pref gate | Notes |
|---|---|---|---|
| `sound` | Plays a sfx (`'ping'`/`'join'`/`'leave'`) via the `play_sfx` Rust command | `allow_sound_effects` | Cooldownable |
| `osNotif` | Fires an OS notification banner via `plugin:notification\|notify` | `allow_desktop_notifications` + OS permission | Cooldownable |
| `badge` | Increments the per-room unread count (`useAppStore.incrementUnread`) | none | Drives dock/taskbar badge via `useBadge` |
| `alert` | Sets the blinking status-bar alert (`useAppStore.setStatusBarAlert`) | none | Cleared on navigation |
| `overlay` | Sets `pendingEnrollmentApproval` so the UI takes over | none | Used only by enrollment |
| `cooldownMs` | Suppresses repeat sound + OS-notif within the window | — | Keyed by `(category, roomId)` |

### Conventions

1. **Anything with `osNotif: true` should also have `sound: 'ping'`.** Users always hear every system notification. Don't ship a silent OS banner.
2. **Pings are reserved for personal events.** Channel chatter only updates the badge — noisy rooms must not become a constant ping.
3. **`badge` and `alert` are never cooldown-gated.** The unread count must stay accurate; only sound and OS notifications dedupe.

## Categorization (the call site)

The "which category does this event belong to" decision lives at the call site, not in the table. This keeps the table flat and event-shaped predicates (`isOwnMessage`, `isSelected`, `isOwnVoiceChannel`) out of the config.

In `useLiveKitRealtime.ts`, the `new_message` handler categorizes:

```ts
if (isOwnMessage || isSelected || !incomingId) return;
notify(conversationId ? 'direct_message' : 'channel_message', { roomId, title, body, senderUsername });
```

The voice handler categorizes:

```ts
if (event.user_id === currentUserIdRef.current) return;          // own join → handled in useVoiceChannel
if (event.channel_id !== activeVoiceChannelIdRef.current) return; // different room → noise
notify(event.type === 'voice_joined' ? 'voice_other_join' : 'voice_other_leave');
```

The membership handler reads the `kind` discriminator (see below):

```ts
if (event.kind === 'invite') {
  notify('group_invite', { roomId: event.conversation_id, title: 'New group invite', body: '...' });
}
```

## Pref + permission flow

`useLiveKitRealtime.ts` owns the React-side state and pushes it into `notify.ts` via `setNotifyPrefs(...)`. The effect re-runs whenever `allow_sound_effects` or `allow_desktop_notifications` changes:

1. Read current prefs from `usePreferences()`.
2. Call `plugin:notification|is_permission_granted` — if not granted *and* the user has notifications enabled, request permission.
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

The Rust enum (`src-tauri/src/realtime.rs`):

```rust
MembershipChanged {
    conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
},
```

Wire dispatch (`src-tauri/src/commands/livekit.rs`) reads the `kind` field from the JSON payload. Publishers set it via `serde_json::json!({"type": "membership_changed", ..., "kind": "invite"})`.

The frontend type narrows it:

```ts
| { type: 'membership_changed'; conversation_id?: string | null; kind?: 'invite' | 'approval' | null }
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

## Files

| File | Role |
|---|---|
| `frontend/src/utils/notify.ts` | Dispatcher + category table |
| `frontend/src/utils/sfx.ts` | `playSfx()` wrapper around `play_sfx` Rust command |
| `frontend/src/hooks/useLiveKitRealtime.ts` | Categorizes incoming Rust events, calls `notify(...)`, owns pref + permission sync |
| `frontend/src/hooks/useVoiceChannel.ts` | Calls `notify('voice_self_join'/'voice_self_leave')` for local actions |
| `frontend/src/hooks/useBadge.ts` | Reads `unreadCounts` from Zustand, applies dock/taskbar badge |
| `src-tauri/src/realtime.rs` | `RealtimeEvent` enum (Rust → JS wire format) |
| `src-tauri/src/commands/livekit.rs` | `dispatch_data()` parses payloads, sends typed events to JS |
| `src-tauri/src/commands/sfx.rs` | `play_sfx` rodio implementation |
| `src-tauri/src/commands/groups.rs` | Membership-change publishers (set `kind` here) |

## Related issues

- #186 — original audit + dispatcher refactor (PR #202).
