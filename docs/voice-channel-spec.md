# Pollis Voice Channel — Minimal Viable Spec

> **Status: historical.** This document is the pre-shipping design spec from
> when voice channels were first being scoped. The feature has since shipped
> and the implementation diverged from this spec in two material ways:
>
> 1. Section 6 below proposed shipping v1 *without* audio E2EE. We did not
>    do that. Voice is end-to-end encrypted in production: per-frame
>    AES-128-GCM via libwebrtc's `FrameCryptor`, keyed by
>    `MlsGroup::export_secret("pollis/voice/v1", epoch, 32)` on the
>    channel's MLS group. See `pollis-core/src/commands/voice_e2ee.rs` and
>    the "End-to-end encryption" section of
>    `.codesight/wiki/audio-processing.md`.
> 2. Group encryption is MLS (RFC 9420) via `openmls`, not the Signal
>    Protocol referenced throughout the doc.
>
> Kept for institutional context. For current behavior, read the wiki.

## 0. Context and starting point

LiveKit is already wired in. Every channel already has a LiveKit room named after its `channel_id`. The frontend connects to that room when a channel is selected (`useLiveKitRealtime`), using a JWT minted by `get_livekit_token`. The JWT grants `canPublish`, `canSubscribe`, and `canPublishData`. Audio tracks are not yet published; only data channel pings flow today.

The `Channel` type already carries a `channel_type` field (`'text' | 'voice'`). This is the hook the whole feature hangs on.

---

## 1. Minimal voice channel UX

### What the user sees

**In the channel list (group menu view)**

Voice channels are distinguished from text channels by a `[v]` prefix to stay consistent with the terminal aesthetic. Channels whose `channel_type` is `'voice'` render differently in the `renderGroupMenu` list.

**Joining**

Selecting a voice channel from the menu does not open a text composer. Instead it shows a `VoiceChannelView` — a simple participant list. Joining is implicit: selecting the channel joins the call. There is no separate "join" button for v1.

**While in a call**

A persistent `VoiceBar` appears at the bottom of the app (above the existing breadcrumb footer bar) whenever the user is in a voice channel. It shows:

- Channel name (e.g. `[v] general-voice`)
- Mute/unmute toggle button
- Leave button (disconnects audio, navigates back)
- Participant count

The `VoiceBar` stays visible even if the user navigates to a text channel in another group — they remain in the call until they explicitly leave or the app closes.

**Participant list**

Inside `VoiceChannelView`, each connected participant renders as a single row:

```
[username]   [speaking indicator]   [muted indicator]
```

Speaking indicator: a small animated dot or `*` in terminal style, lit when that participant is the active speaker. Muted indicator: a static dim `[m]`.

**Mute**

The mute button in `VoiceBar` toggles `localParticipant.setMicrophoneEnabled(false/true)`. The current mute state is local-only — no server round-trip needed.

### What is NOT in v1

- Video, screen share, push-to-talk, noise cancellation config
- Per-user volume controls, call history, incoming call notifications
- Voice DMs

---

## 2. LiveKit features needed

LiveKit handles all WebRTC transport, STUN/TURN, and codec negotiation. The app only uses its JS client API.

**Already available (no new work)**

- Room connection and JWT auth (`get_livekit_token` already grants `canPublish`)
- Data channel (used for message pings today)
- Participant join/leave events

**New LiveKit API surface**

| Feature | LiveKit API |
|---|---|
| Publish local mic | `localParticipant.setMicrophoneEnabled(true)` |
| Mute local mic | `localParticipant.setMicrophoneEnabled(false)` |
| Active speaker detection | `RoomEvent.ActiveSpeakersChanged` |
| Remote mute state | `TrackPublication.isMuted` on each remote participant's mic track |
| Participant list | `room.remoteParticipants` + `room.localParticipant` |
| Participant identity | `participant.identity` (user ID), `participant.name` (display name from JWT) |

No audio rendering element needed — `livekit-client` auto-plays remote audio without a `<audio>` DOM element.

---

## 3. Rust backend changes

### 3a. No new Tauri commands needed for v1

`get_livekit_token` already issues tokens with full publish permissions for any `room_name`. The existing 3600s TTL is fine.

### 3b. DB schema

Voice presence is ephemeral — LiveKit tracks connected participants, nothing to persist.

The only DB migration needed is adding `channel_type` to the `channels` table in Turso if it doesn't exist:

```sql
ALTER TABLE channels ADD COLUMN channel_type TEXT NOT NULL DEFAULT 'text';
```

### 3c. Summary of backend changes

| Change | File | Notes |
|---|---|---|
| Accept `channel_type` in `create_channel` | `src-tauri/src/commands/groups.rs` | Add field to args struct, pass to INSERT |
| Return `channel_type` in `list_user_groups_with_channels` | `src-tauri/src/commands/groups.rs` | Add column to SELECT, map into `Channel` struct |
| Turso migration | migration script | `ALTER TABLE channels ADD COLUMN channel_type TEXT DEFAULT 'text'` |

---

## 4. Frontend component structure

### New hook: `useVoiceChannel`

`frontend/src/hooks/useVoiceChannel.ts`

Manages the voice session lifecycle independently of `useLiveKitRealtime`. Key responsibilities:

- Holds a **separate** `Room` instance (distinct from the data-ping room in `useLiveKitRealtime`)
- Publishes local microphone track on join
- Subscribes to `ActiveSpeakersChanged`, `ParticipantConnected`, `ParticipantDisconnected`
- Exposes: `participants`, `activeSpeakerIds`, `isMuted`, `toggleMute()`, `leave()`
- Cleans up on unmount or `leave()`

Two separate `Room` instances are intentional — the data-ping room should never publish audio.

### New store slice

Add to `appStore.ts`:

```typescript
activeVoiceChannelId: string | null;
setActiveVoiceChannelId: (id: string | null) => void;
```

Participant data and speaking state live inside `useVoiceChannel` as local React state (high-frequency updates, no need to be global).

### New component: `VoiceChannelView`

`frontend/src/components/Voice/VoiceChannelView.tsx`

Rendered when `currentView.type === 'voice-channel'`. Shows participant list, delegates audio to `useVoiceChannel`.

```
[v] general-voice
─────────────────────────────────
  alice            ●   (speaking)
  bob             [m]  (muted)
  carol
─────────────────────────────────
```

### New component: `VoiceBar`

`frontend/src/components/Voice/VoiceBar.tsx`

Rendered in `TerminalApp.tsx` between main content and bottom breadcrumb bar when `activeVoiceChannelId` is set (~28px height).

```
[v] general-voice  |  [mic on]  [leave]  |  3 participants
```

### Changes to `TerminalApp.tsx`

1. Add `'voice-channel'` to the `View` union type
2. In `renderGroupMenu`, prefix voice channels with `[v]`; selecting one pushes `{ type: 'voice-channel' }` and sets `activeVoiceChannelId`
3. Render `<VoiceBar />` between main content and bottom bar, guarded by `activeVoiceChannelId !== null`
4. Add `case 'voice-channel': return <VoiceChannelView channelId={activeVoiceChannelId} />` to `renderContent`

### Changes to `CreateChannel` page/modal

Add a radio or toggle — "Text" / "Voice" — that sets `channel_type` in the form payload.

---

## 5. Voice + text in the same LiveKit room vs. separate rooms

**Decision: same room, two Room instances.**

Use the channel ID as the room name for both data pings and audio (already the pattern). When a voice channel is selected:

- `useLiveKitRealtime` keeps its data-ping connection to the room
- `useVoiceChannel` connects a second `Room` instance to the same room name, solely for audio

Same JWT works for both. Two connections to the same room is acceptable for v1 and avoids any naming scheme changes.

**Rejected alternative**: single `Room` instance for everything. Merging audio lifecycle with the data-ping singleton (`livekitRoomRef`) requires significant refactoring not justified for v1.

---

## 6. E2EE considerations for audio

> **Superseded.** The "v1 without E2EE" recommendation below was not what
> shipped. Production wires `livekit::e2ee::E2eeOptions { Gcm, KeyProvider }`
> into `RoomOptions` at `Room::connect` time, with the shared key derived
> from `MlsGroup::export_secret("pollis/voice/v1", epoch, 32)` on the
> channel's MLS group. The key rotates on every MLS epoch advance (both for
> committers and receivers). Source: `pollis-core/src/commands/voice_e2ee.rs`.

### Original recommendation (not shipped): v1 without audio E2EE

LiveKit supports frame-level E2EE via `E2EEManager` (WebCrypto), but:

- Requires a shared key between all participants — distribution is non-trivial in a Signal-based system
- Signal Protocol (SenderKey) is designed for async messages, not real-time streams; no direct mapping to LiveKit frame encryption
- Practical threat: Pollis's own LiveKit server would need to be actively recording streams — substantially different from the Turso text interception scenario
- Transport is TLS-encrypted to LiveKit in transit

**What actually shipped**: per-frame AES-128-GCM via libwebrtc's `FrameCryptor`, keyed by the MLS exporter secret on the channel's MLS group (the same group that protects the channel's text messages). For 1:1 calls, the key derives from the DM's MLS group between the two participants. There is no opt-out and no UI indicator — all communication is encrypted.

---

## 7. What to defer to v2

| Feature | Reason |
|---|---|
| ~~Audio E2EE~~ | Shipped in v1, not deferred. See §6 above. |
| Push-to-talk | Requires hotkey capture in Tauri |
| Incoming call ring | Needs notification system not yet built |
| Voice in DMs | DMs don't have `channel_type` today |
| Noise suppression config | Browser defaults are fine for v1 |
| Recording | Major privacy/legal scope |
| Per-user volume | AudioContext routing; UI complexity |
| Call history in Turso | Schema addition + privacy tradeoff |

---

## 8. Implementation sequence

1. Turso migration — add `channel_type` to `channels` table
2. Rust: update `create_channel` and `list_user_groups_with_channels` in `src-tauri/src/commands/groups.rs`
3. Verify `Channel.channel_type` TypeScript type matches Rust struct
4. `CreateChannel` page — add channel type selector
5. `useVoiceChannel` hook — join/leave/mute/participant/speaker logic
6. Zustand — add `activeVoiceChannelId` to `appStore.ts`
7. `VoiceChannelView` component — participant list
8. `VoiceBar` component — persistent bottom bar
9. `TerminalApp` wiring — `voice-channel` view type, `VoiceBar`, `[v]` prefix in group menu
10. Manual QA — two clients, verify mute indicator propagates, leave cleans up
