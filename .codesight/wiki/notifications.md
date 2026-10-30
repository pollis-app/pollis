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

There is **no parallel dispatcher in Rust**. All decisions happen in JS. Rust is just the transport for LiveKit events (pushed through the `EventSink`/`ChannelSink` over a `tauri::ipc::Channel` to the renderer's `channelOn` subscriber) and the executor for sound (`play_sfx` rodio command). OS notifications fire through the bridge — the renderer calls `sendNotification({ title, body, icon })`, which routes to `tauri-plugin-notification`. That bridge call **escapes `&`, `<` and `>` in both the title and the body** (`utils/notificationText.ts`, #1095): a notification body is not plain text on Linux — the freedesktop spec allows a small HTML subset and `notify-rust` passes it verbatim — and every name interpolated into one (sender username, group name, inviter preferred name) is remote-controlled, so a group called `<img src=…>` would otherwise turn message receipt into a request from the victim's machine. Escaping at the bridge rather than at each i18n site is deliberate: it covers the next caller too. The DS also refuses to store such a name (`profile::is_valid_display_name` — bounded length, no control characters, no bidi overrides, no `<`/`>`, but `&` and non-Latin scripts allowed), so the escaping is defence in depth.

## The category table

The single source of truth (`frontend/src/utils/notify.ts`):

```ts
const CATEGORIES: Record<Category, CategoryConfig> = {
  direct_message:    { sound: 'ping',  osNotif: true,  badge: true, alert: true, cooldownMs: 2500 },
  channel_message:   { sound: 'ping',  osNotif: true,  badge: true,              cooldownMs: 2500, mutable: true, cooldownGroup: 'channel' },
  voice_other_join:  { sound: 'join'                                                              },
  voice_other_leave: { sound: 'leave'                                                             },
  voice_self_join:   { sound: 'join'                                                              },
  voice_self_leave:  { sound: 'leave'                                                             },
  dm_request:        { sound: 'ping',  osNotif: true,               alert: true                   },
  group_invite:      { sound: 'ping',  osNotif: true,               alert: true                   },
  enrollment:        { sound: 'ping',  osNotif: true,                            overlay: true    },
  incoming_call:     {                  osNotif: true,                            honorsRingtonePref: true },
  // @all in a group. Silenced by a mute like chatter (it is addressed to the
  // room, not to you). Badge is left to the accompanying new_message event so
  // a connected client doesn't double-count unread.
  all_mention:       { sound: 'ping',  osNotif: true,                            cooldownMs: 2500, mutable: true, cooldownGroup: 'channel' },
  // @username in a group (#843) — someone spoke to YOU by name, which is a
  // personal event, so it is treated like a DM rather than like channel
  // chatter: ping, OS notification, AND the status-bar alert that names the
  // sender. Deliberately louder than `all_mention` (addressed to a room, not
  // to you) — that alert is the difference. NOT mutable: a direct mention
  // reaches you in a muted channel (#1431).
  user_mention:      { sound: 'ping',  osNotif: true,               alert: true, cooldownMs: 2500, cooldownGroup: 'channel' },
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
| `cooldownMs` | Suppresses repeat sound + OS-notif within the window | — | Keyed by `(cooldownGroup ?? category, roomId)` |
| `mutable` | Silenced (sound + OS notif) while `payload.roomId` is a muted channel (#1431); the badge still counts | `setMutedChannels` | `channel_message`, `all_mention` |
| `cooldownGroup` | Categories sharing a group share one cooldown per room | — | `channel`: the `new_message` and the mention ping one @mention raises make one sound and one banner |

### Conventions

1. **Anything with `osNotif: true` should also have `sound: 'ping'`.** Users always hear every system notification. Don't ship a silent OS banner. The one exception is `incoming_call`, which has no `sound` because it plays a *ringtone* on its own device-local gate (`honorsRingtonePref`) rather than a one-shot sfx — muting ringing on one device must not silence every other alert on it.
2. **Channel chatter pings by default; a noisy room is muted, not ignored (#1431).** Every channel message also pushes every other member's phones, so desktop matches: `channel_message` pings and raises an OS notification. A user who finds a channel too loud mutes it (per channel, per account, stored on the DS — see "Per-channel mute" below); a muted channel's chatter and `@all` only update the badge, while a direct `@username` still gets through. The account-wide `allow_desktop_notifications` preference still silences every OS banner on the device.
3. **`badge` and `alert` are never cooldown-gated.** The unread count must stay accurate; only sound and OS notifications dedupe.

## Categorization (the call site)

The "which category does this event belong to" decision lives at the call site, not in the table. This keeps the table flat and event-shaped predicates (`isSelected`, `isOwnVoiceChannel`, own-message) out of the config.

In `useLiveKitRealtime.ts`, the `new_message` handler categorizes. The wake-up names no sender (§5), so "is this my own message, sent from my other device?" is answered AFTER ingest: `read_last_messages` reads the newest local row back, and `shouldAlertForWake` (`frontend/src/utils/messageWake.ts`, tested in `frontend/tests/message-wake.test.ts`) suppresses the alert and the badge when its MLS-attributed `sender_id` is ours. The message itself is ingested and shown either way. (The old `isOwnMessage` read `event.sender_id`, which the payload stopped carrying, so it was always false: a message from your other device raised "New message from Someone" and bumped the badge.) Mobile applies the same rule to its unread count (`mobile/lib/messageWake.ts` `shouldCountUnread`). On mobile that bump is only the instant path: the badges come from the persisted read cursors (`get_unread_counts`, #1422, `mobile/hooks/queries/useUnread.ts`), re-read after the bump and after every ingest, so a message that arrived while the app was backgrounded or closed — the push case, which no live wake-up saw — still badges.

```ts
if (isSelected || !incomingId) return;
await ingested;
const latest = (await invoke('read_last_messages', { conversationIds: [incomingId] }))[0];
if (!shouldAlertForWake({ latest, selfId, isSelected: false })) return;
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

**Who the identity comes from — never the sending client.** Every inbox ping goes through `POST /v1/livekit/send-data`, where the DS strips every identity key the client sent (`sender_id`, `sender_username`, `caller_id`, `caller_username`, `inviter_username`, `group_name`, …) and, for the inbox kinds that name their actor (`dm_created`, `membership_changed`, `all_mention`, `user_mention`, `call_invite`), stamps the **verified signer** back in as `sender_id` + `sender_username` (from `users`; `group_name` from `groups`), plus the same values under the legacy per-kind keys. The DS also refuses a target the signer may not reach — another user's inbox needs a shared DM / group / pending invite and no block either way; `call_invite` needs an accepted DM or a shared group — and refuses `enrollment_requested` from any client (it is DS-originated, `bootstrap::enrollment_request`). `dispatch_data` then attributes `call_invite` / `dm_created` / the `invite` kind from that stamp when the packet has no participant, or from the publishing participant when it has one, and drops a `call_invite` with neither; it drops `enrollment_requested` from any participant, and passes on its `link_id` (set on a QR-link request, which the UI must not answer with the typed-code takeover) but never its `verification_code` (#1096). See `secrets-broker.md`.

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

Cooldown is keyed by `${cooldownGroup ?? category}:${roomId ?? '_global'}`; `channel_message`, `all_mention` and `user_mention` share the `channel` group, so one @mention (which arrives as both a `new_message` and a mention ping) sounds once. It applies only to sound and OS notification — never to badge, alert, or overlay. The cooldown timestamp is recorded only when *something actually fired* (i.e., pref + permission allowed it). This means:

- A 10-message burst from one DM pings once, OS-notifies once, but badges 10 times.
- A burst across two DM rooms pings twice (different cooldown buckets).
- If sound is disabled but OS notif is enabled, the OS notification still fires and starts the cooldown.

## Mentions (#843) and who gets pushed (#1431)

Mentions are decided **in Rust**, in `pollis-core/src/commands/messages/send.rs`:

- `mention_audience(is_channel, content, members)` returns `Everyone` (a DM, or
  a channel `@all`), `Only(user_ids)` (a channel naming specific members), or
  `Nobody` (ordinary channel chatter). It drives the inbox ping: `all_mention`
  to every member, or `user_mention` to just the named ones. A member named
  directly inside an `@all` gets the `user_mention` instead of the
  `all_mention`, because that is the ping that gets through a mute.
- `direct_mentions(content, members)` is the list of members named by
  `@username` (whether or not the message also says `@all`).

**Every channel message pushes every other member (#1431).** The send carries
`push_to: Some([])` (everyone) and `push_mentions: Some(direct_mentions)`; the
DS (`pollis_delivery::push::PushAudience::from_send` → `push_recipients`)
takes every member but the sender, intersects with any `push_to` subset (so it
can only ever remove recipients), then drops each member who muted the channel
unless `push_mentions` names them. A DM sends no mention list; nobody can mute
one.

**Backward compatibility.** A client from before #1431 sends `None` for
ordinary chatter (still: no push), `Some([])` for `@all` (the mute applies), and
`Some([ids])` with no `push_mentions` for `@username` — read as "only these, and
they are the mention list", so they get through a mute exactly as before.

## Per-channel mute (#1431)

A row in `channel_mute (user_id, channel_id)` (migration 000038) — the row's
existence is the mute. Per account, not per device: `set_channel_muted` writes
it through `POST /v1/channel-mutes/set` (only for a group channel the caller is
a member of; a DM or group id is refused), `list_muted_channels` reads it back
through `POST /v1/read/channel-mutes`, and after every write the DS sends
`channel_mutes_changed` to the owner's `inbox-<user>` room (DS-originated; a
participant-published copy is dropped) so the account's other devices refetch.
Rows go when the member leaves or is removed, with the channel, and with the
account (`teardown.rs`).

- **Desktop:** the bell in the channel header (`channel-mute-toggle`) toggles
  it; the sidebar and the group's channel list draw a muted channel in the
  muted tier with a bell-off mark (`sidebar-channel-muted-<id>`).
  `useLiveKitRealtime` mirrors `useMutedChannels` into `notify.ts`
  (`setMutedChannels`), which silences `mutable` categories for that room.
- **Mobile:** the channel's "More" sheet carries the switch
  (`toggle-channel-mute`); the group panel dims the row and marks it with a
  bell-off glyph (its id becomes `row-channel-<id>-muted`). Pushes — the only
  OS notifications mobile raises, backgrounded or foreground — are already
  filtered by the DS.

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

## Clearing read push notifications (#1508)

A phone's message notification used to stay in the tray after the message was
read, unless it was the very notification the user tapped: opening the
conversation from the list, Search or a link, or reading it on desktop or
another phone, left it there. Slack/Discord behaviour is now the rule — once a
conversation is read on **any** client, its notifications leave **every**
phone's tray. Three paths, all matching on the notification's opaque handle `h`
(#1122), never on a conversation id:

1. **This phone read it.** `markConversationRead` (`mobile/hooks/queries/useUnread.ts`,
   every route: tray tap, list, Search, link, the open chat as messages arrive)
   calls `dismissConversationNotifications` (`mobile/lib/push/index.ts`): each
   tray notification's handle is resolved with the same `resolve_push_handle` a
   tap uses (cached in core) and the ones for that conversation are dismissed.
2. **Any client read it.** `pollis-core`'s `mark_conversation_read`, when the
   cursor moves, queues the conversation (`commands::push::schedule_push_dismiss`,
   2 s debounce) and reports `{ conversation_id, read_secs_ago }` to
   `POST /v1/push/dismiss`. The DS stamps the user's handles minted before the
   read and sends every token of theirs one **silent, data-only** push
   `{ d: [handles] }` (no title or body, `priority: normal`, iOS
   `contentAvailable`), coalesced per user (`push::DISMISS_COALESCE`, 2 s).
   On the phone, the expo-notifications background task
   (`mobile/lib/push/dismissTask.ts`, registered from the app entry
   `mobile/index.ts` before expo-router loads) clears those handles from the
   tray — in the foreground, in the background and, on Android, with the app
   killed (headless JS); no bridge, no unlock, no network needed.
3. **The silent push never arrived** (iOS does not wake a force-quit app;
   Android may hold a normal-priority push in Doze). On landing in the app and
   every return to the foreground, `reconcileReadNotifications` sends the tray's
   handles to `dismissed_push_handles` → `POST /v1/push/dismissed` and clears
   the ones the DS recorded as read. It never guesses from local unread counts:
   a message not yet ingested on this device would look read.

**Finding a notification's handle in the tray.** iOS keeps the payload on a
delivered notification, so `data.h` is there. On Android a notification the
app presented itself (foreground) keeps it too, but one the system posted while
the app was in the background keeps none of the payload — only its tag — so
the DS tags every Android message push with its handle, and
`mobile/lib/push/dismiss.ts` reads it back from expo-notifications' foreign
identifier (`expo-notifications://foreign_notifications?tag=<h>&id=0`). A
per-conversation tag (or `collapseId`) would have been simpler and is exactly
what #1122 removed: a stable value the providers could count.

**Privacy.** The silent push carries only handles the providers already saw on
the original notifications — they learn which notifications were read, never a
conversation and never content. The DS learns that the user read a
conversation at about that time, which the read-receipt envelope (#892, on by
default) already tells it; the encrypted read-cursor blob stays opaque.

**Compatibility.** A build without the task ignores the silent push: no
title/body means expo-notifications never presents it (on every SDK we have
shipped, 55 and 57), and nothing listens for data-only messages. A client from
before this change never calls the new endpoints; a new client against an old
DS gets a 404, which is best-effort (kept and retried with the next read). The
mobile half needs a store build (pollis-core change + the expo-task-manager
native module + the iOS `remote-notification` background mode + an R8 keep
rule for expo's headless app loader, without which a minified Android build
crashed on the first silent push to a killed app).

**Desktop** raises no push and registers no token; its part is the core report
above, so reading on desktop clears the phone.

## Android rich push (#1599)

A capable Android build registers its token with `caps: ["rich"]`
(`mobile/lib/push/caps.ts`, only when the binary's embedded config has
`extra.richPush.android`, which `mobile/plugins/withRichPush.js` sets). The DS
then sends it a **data-only**, high-priority `{h}` instead of the generic alert,
so the app, not the system, posts the notification.

- **One FCM service.** `com.pollisnative.push.PollisMessagingService`
  (pollis-native module) *subclasses* expo-notifications'
  `ExpoFirebaseMessagingService`, and `withRichPush.js` removes expo's own
  `<service>` from the app manifest (`tools:node="remove"`), so FCM has exactly
  one `MESSAGING_EVENT` target instead of two services ranked by intent-filter
  priority (expo's is `-1`). `RichPush.richHandle` claims a message only when it
  has no notification block, no Expo `title`/`message`, and an `h` (top level,
  or inside the Expo push service's JSON `body`). Everything else — today's
  alert pushes, the `{d}` dismissals, `onNewToken`, `onDeletedMessages` — goes to
  `super`, i.e. expo, unchanged. In the foreground a rich push also goes to
  `super` so JS's received listener still ingests; the JS handler presents no
  title-less push (`mobile/lib/push/presentation.ts`), so nothing doubles.
- **Fetch.** `RichPushHandler` calls `pollis_notify_fetch` through the JNI shim
  in `pollis-native/android/cpp-adapter.cpp` (`PollisNotify.nativeFetch`,
  UTF-8 bytes back, not a modified-UTF-8 jstring), with data dir =
  `filesDir + "/"` — exactly what `lib/native/bridge.ts` gives `init_pollis`.
  The same `.so` serves the JSI bridge, so a warm process shares the app's core
  and a cold one (FCM started it, no React) initialises it headless; the core's
  `state.lock` keeps the two from racing. Budget: the whole handler finishes in
  8.5 s (FCM allows ~10 s for a high-priority data message), the core gets that
  minus 0.7 s and minus any time queued behind the previous push (one fetch at a
  time). Not WorkManager: an expedited job adds a dependency and quota, and
  before Android 12 needs a foreground service, which the app blocks. If the
  fetch overruns, the generic text is posted at the deadline and a late result
  replaces it in place (same tag, `setOnlyAlertOnce`).
- **Post.** `RichPushNotifier`: `MessagingStyle` on channel `default`, **tag =
  `h`, id 0** (what FCM used), so all three #1508 paths keep finding it through
  the foreign identifier. One notification per conversation: a new one folds the
  messages of the one already showing for that conversation and replaces it
  (the newest handle is the one a read clears); a message already in the stack
  (core's one-time fallback re-returns the newest one) is not added twice, by
  `message_id`. Muted (never set for an @mention) → nothing; "Name only" → the
  sender with `push.nameOnlyBody`; failure, no items, or mode "none" (the
  user's "Nothing", or no escrowed key yet) → the generic "New message / You
  have a new message". The tap opens the launcher activity with the push's data plus
  `google.message_id` as extras, exactly what FCM's system notification does,
  so expo's lifecycle listener and `lib/push` route it by `h` as before.
- **Copy** is Android string resources (`pollis-native/android/src/main/res/values*/pollis_push_strings.xml`),
  generated from the shared `mobile.push.*` keys by
  `node scripts/mobile-android-push-strings.mjs` (`{{sender}}`-style
  placeholders become fixed positional `%1$s` args per key, `ARGS`; `--check`;
  `mobile/tests/rich-push.test.ts` fails when stale). They follow the device
  locale, not the in-app language choice, which native code cannot read.
- **Test hook (debug builds only).** `RichPushTestReceiver`:
  `adb shell am broadcast -a com.pollis.mobile.debug.RICH_PUSH -p com.pollis.mobile --es body '{"h":"<22 chars>"}' [--es fake '<fetch JSON>']`
  runs the same path as an FCM delivery; `fake` substitutes the core's reply.

## Rich push, decrypted on the device (#1599) — the core half

The push payload is still only `{ h }`. A capable phone wakes on it, fetches
the ciphertext itself, decrypts and stores it, and rewrites the notification
with the sender and a preview. The iOS Notification Service Extension and the
Android FCM handler both call one plain C function in `pollis-core`
(`pollis-core/include/pollis_notify.h`, `src/notify/`):

- `pollis_notify_fetch(data_dir, h, budget_ms)` → JSON. It resolves `h`
  (`resolve_push_handle`), then runs the app's own
  `ingest_dm_envelopes_inner` / `ingest_channel_envelopes_inner` on a headless
  `AppState`: no realtime, no LiveKit, no media server, the database opened
  with `LocalDb::open_existing_for_user_in`, which never creates, migrates or
  wipes. It returns only the inbound messages that call stored; if it stored
  nothing for the handle's conversation (the app already had it), the newest
  message there from the last 10 minutes, once (its id is kept in `ui_state`
  `notify_fallback_shown`). Setting "Nothing" returns before any fetch. At most 5, oldest first, body ≤ 200 chars
  with markdown markers removed (`notify/preview.rs`). Attachments become a
  placeholder plus `body_key` (`image|video|voice|attachment|sensitive`).
  `muted` is a muted channel with no direct `@username` of this user
  (`mention`), the same rule `push_recipients` uses. Failures are reasons
  (`not_logged_in|lock_busy|timeout|network|decrypt|internal`); the host then
  shows the generic text.
- **Setting.** `get_notification_content` / `set_notification_content {mode}`
  (`full|name|none`, `ui_state` key `notification_content`, default `full`),
  bridge arms only (mobile-only).
- **Key escrow (security-relevant).** The DB key is PIN-wrapped, so the
  extension could not open the database at all. On mobile, while the setting
  is not `none`, the core keeps the unwrapped key in keystore slot
  `notify_db_key`. Whitepaper §3.8 has the trade-off. Desktop never escrows.
- **Cross-process lock.** Every MLS-mutating step (all `mls_group_lock`
  sections, welcome drains, KeyPackage top-ups, receipt seals) also holds
  `flock(<data_dir>/state.lock)` (`notify/lock.rs`). The app shares one hold
  across its sections; the extension holds it for its whole run with a
  deadline of 40% of its budget, and only for the ingest (it lets go before
  the name and mute lookups). On iOS the host takes
  `pollis_app_suspend_ticket` on entering the background, then calls
  `pollis_app_will_suspend(ticket, ms)` (no new holds, drain the open ones,
  checkpoint and close the SQLCipher connection, whose WAL holds `-shm` locks)
  and `pollis_app_did_resume` (reopen), from
  `mobile/modules/pollis-native/ios/PollisStateLockLifecycle.mm`, so the app
  never sits suspended holding a lock in the App Group (`0xdead10cc`). A
  ticket older than the last resume does nothing. The extension closes its
  connection before returning.
- **iOS data dir + keychain.** `pollis_set_data_dir_override` (App Group
  container) relocates the data dir without changing the keystore namespace,
  and `init_pollis` moves the old `Application Support/pollis` there once
  (`notify/host.rs::migrate_dir`: only when the old dir holds the app's data and
  the new one does not; a copy across volumes parks the old tree as
  `.migrated` and deletes it, retried on later launches; a failed move keeps
  this launch on the old dir). The keystore KEK is COPIED into the access group
  from `pollis_set_keychain_access_group` / Info.plist
  `PollisKeychainAccessGroup`, and the app-only item is kept, so a build
  without the entitlement still finds it. Any error reading the shared group
  falls back to the app-only item. No process mints a KEK while a sealed
  `keystore.pks` exists, and the extension never mints one
  (`keystore.rs::kek_plan`, host-tested). Files are kept at Data Protection class C
  (readable after first unlock).
- **Connection config.** `init_pollis` writes `<data_dir>/notify-config.json`
  (DS URL, R2 origins; the dev gate key in dev builds only) for the extension.

Tests: `src-tauri/tests/flows/rich_push.rs` (app/extension race with no loss
or rollback, deterministic exclusion while the app holds a staged commit,
fallbacks, modes including "Nothing" with a stale escrow, mute + mention, no
message previewed twice, group create/forget waiting on the lock, escrow
deletion by logout and PIN lockout);
unit tests in `notify::*` and `db::local`.

## Files

| File | Role |
|---|---|
| `frontend/src/utils/notify.ts` | Dispatcher + category table |
| `frontend/src/utils/mentions.ts` | Mention parsing + ranking; mirrors the Rust matcher |
| `frontend/src/hooks/queries/useMentionCandidates.ts` | Roster-derived mention pool (the no-leak boundary) |
| `frontend/src/components/ui/RichTextInput.tsx` | The composer field: contentEditable projection of the wire text, and the terminal skin's inline ghost (shared with `:shortcode:` completion) |
| `frontend/src/components/ui/MentionSuggestList.tsx` | Refined skin's Slack-style suggestion list |
| `frontend/src/components/Message/MessageBody.tsx` | Splits mention tokens out, delegates the rest to `LinkifiedText` |
| `pollis-core/src/commands/messages/send.rs` | `mention_audience()` / `direct_mentions()` — the inbox fanout and the push audience |
| `pollis-delivery/src/push.rs` | `PushAudience` + `push_recipients` — membership intersection and the per-channel mute; `dismiss_push` / `read_dismissed_push_handles` — clearing read notifications (#1508) |
| `pollis-core/src/commands/push.rs` | `schedule_push_dismiss` (debounced report from `mark_conversation_read`), `dismissed_push_handles` (#1508) |
| `mobile/lib/push/dismiss.ts` · `dismissTask.ts` · `index.ts` | Tray matching by handle, the background task for the silent push, same-device dismissal and the foreground catch-up (#1508) |
| `mobile/modules/pollis-native/android/src/main/java/com/pollisnative/push/` · `PollisNotify.kt` · `cpp-adapter.cpp` | Android rich push: FCM service, fetch with deadline, MessagingStyle posting, JNI shim (#1599) |
| `mobile/plugins/withRichPush.js` | Replaces expo's FCM service with ours; marks the binary rich-capable (#1599) |
| `pollis-delivery/src/profile.rs` | `set_channel_mute` / `read_channel_mutes` (#1431) |
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
- #1431 — channel messages push by default; per-channel, per-account mute.
- #1508 — a read on any client clears that conversation's push notifications from every phone.
- #1599 — rich push: the device decrypts and shows sender + preview (Android half above).
- #186 — original audit + dispatcher refactor (PR #202).
