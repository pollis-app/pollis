# Frontend Audit — Proposed Changes

> Generated 2026-03-27. No code has been changed. These are proposals only.

---

## 1. Zustand Store — Duplication & Redundancy

### 1.1 Duplicate `username` / `userAvatarUrl` fields — **HIGH**

**Files:** `stores/appStore.ts` lines 6–7, 62–63, 77–94

`username` and `userAvatarUrl` exist as top-level store fields, but they duplicate data already on `currentUser`. `setUsername` (lines 77–94) manually keeps `currentUser.username` in sync as a side effect, which is fragile.

**Proposal:** Delete the separate `username` and `userAvatarUrl` fields. Read `currentUser.username` and `currentUser.avatar_url` everywhere instead. Remove `setUsername`/`setUserAvatarUrl` actions and all callers.

---

### 1.2 Voice state doesn't belong in the store — **MEDIUM**

**Files:** `stores/appStore.ts` lines 47–57, `hooks/useVoiceChannel.ts` lines 40–47

`voiceParticipants`, `voiceActiveSpeakerIds`, `voiceIsMuted`, and `isLocalSpeaking` are written to Zustand by `useVoiceChannel` and immediately read back by `VoiceBar` and `VoiceChannelView`. The store is acting as a pass-through for live call state that doesn't need to survive navigation or re-mount.

**Proposal:** Keep only `activeVoiceChannelId` in Zustand (it drives routing/visibility). Move the four live-call fields into a `useVoiceState` hook that returns them directly from the room ref. This eliminates ~8 setter calls per LiveKit event and removes 4 store fields.

---

### 1.3 Selection state duplicates router params — **LOW**

**Files:** `stores/appStore.ts` lines 64–66, `pages/Channel.tsx` line 14, `pages/DM.tsx`

`selectedGroupId`, `selectedChannelId`, `selectedConversationId` are set in Zustand on navigation, but the router params already encode this information. The only things that read them from Zustand are breadcrumb computation (already done from pathname in `AppShell.tsx`) and `markRead` side effects.

**Proposal:** Keep them for now but move to a separate `UIStore` to make the intent clear. Longer-term: derive from `useParams()` in each page and drop them from the store entirely.

---

### 1.4 `messageQueue` is wired to nothing — **MEDIUM**

**Files:** `stores/appStore.ts` lines 72, 122–133, `components/Message/MessageQueue.tsx`

`messageQueue` and its three actions (`addToMessageQueue`, `updateMessageQueueItem`, `removeFromMessageQueue`) exist in the store, but nothing populates it. `useSendMessage` sends directly without touching the queue. `MessageQueue.tsx` renders an empty list.

**Proposal:** Either delete `messageQueue` from the store and remove `MessageQueue.tsx`, or wire `useSendMessage` to populate it (add on mutation start, update on success/failure, remove on completion). Don't leave it half-implemented.

---

## 2. Unnecessary `useEffect` Hooks

### 2.1 Groups/channels sync into Zustand — DELETE IT — **HIGH**

**File:** `components/Layout/AppShell.tsx` lines 46–54

```typescript
useEffect(() => {
  if (!groupsWithChannels) return;
  setGroups(groupsWithChannels);
  for (const g of groupsWithChannels) {
    setChannels(g.id, g.channels);
  }
}, [groupsWithChannels, setGroups, setChannels]);
```

This runs on every query refetch and syncs React Query data into Zustand. But nothing reads `groups` or `channels` from Zustand for rendering — all consumers use React Query directly. The `setGroups`/`setChannels` actions are called only here.

**Proposal:** Delete this effect. Delete `setGroups` and `setChannels` from the store. Verify nothing reads `state.groups` or `state.channels` for rendering (they don't — it's all from React Query).

---

### 2.2 Seven stale-closure ref updates in `useLiveKitRealtime` — **HIGH**

**File:** `hooks/useLiveKitRealtime.ts` lines 81–102

```typescript
const selectedChannelIdRef = useRef(selectedChannelId);
useEffect(() => { selectedChannelIdRef.current = selectedChannelId; }, [selectedChannelId]);
// ... 4 more identical patterns
```

Five separate effects just to keep refs current for a message handler. This is a sign the message handler is structured wrong — it was set once at mount but needs current values.

**Proposal:** Rebuild the channel message handler with `useCallback` and include its real dependencies. Re-register the `channel.onmessage` listener when those dependencies change (add it to the subscription effect's cleanup). This removes 5 effects and makes the data flow explicit.

---

### 2.3 Three separate keyboard shortcut effects — **LOW**

**File:** `components/Layout/AppShell.tsx` lines 59–92

Three `useEffect` hooks each add/remove a `window` keydown listener for Cmd+K, Esc, and Cmd+R respectively. Each one adds overhead and they share no state.

**Proposal:** Combine into one `useEffect` with a single listener that dispatches on `e.key`. Or extract a `useKeyboardShortcuts` hook. Either way: one listener, one cleanup.

---

### 2.4 Room name map built in a separate effect — **LOW**

**File:** `hooks/useLiveKitRealtime.ts` lines 61–76

A dedicated effect builds `roomNameMapRef` from `groupsWithChannels` and `dmConversations`. The map is only ever read inside the channel message handler (line 186), which already has access to the same data.

**Proposal:** Merge into the `allRoomIds` computation (lines 41–56) — build the map inline in the same `useMemo`/effect block, avoiding a separate pass over the same data.

---

## 3. Unnecessary Re-renders

### 3.1 `MainContent` subscribes to 5 store fields, only renders 2 — **HIGH**

**File:** `components/Layout/MainContent.tsx` lines 11–17

```typescript
const { selectedChannelId, selectedConversationId, replyToMessageId, setReplyToMessageId, currentUser } = useAppStore();
```

Any change to any of these five fields re-renders `MainContent`. `selectedChannelId` and `selectedConversationId` are passed to query hooks (which have their own subscriptions). Only `replyToMessageId` is used in the render tree.

**Proposal:** Use per-field selectors:
```typescript
const replyToMessageId = useAppStore((s) => s.replyToMessageId);
const setReplyToMessageId = useAppStore((s) => s.setReplyToMessageId);
const currentUser = useAppStore((s) => s.currentUser);
```
Move `selectedChannelId`/`selectedConversationId` to `useParams()` (they're already in the route).

---

### 3.2 `getAuthorUsername` function prop not memoized — **LOW**

**File:** `components/Layout/MainContent.tsx` lines 76–78

```typescript
getAuthorUsername={(authorId, message) =>
  message?.sender_username || (authorId === currentUser?.id ? "you" : authorId)
}
```

This creates a new function reference on every render of `MainContent`, breaking any memoization in `MessageList` or `MessageItem`.

**Proposal:**
```typescript
const getAuthorUsername = useCallback(
  (authorId: string, message?: Message) =>
    message?.sender_username || (authorId === currentUser?.id ? "you" : authorId),
  [currentUser?.id],
);
```

---

### 3.3 `syncParticipants` event handlers re-registered on every render — **LOW**

**File:** `hooks/useVoiceChannel.ts` lines 53–85, 124–149

`syncParticipants` is a `useCallback` but its dependencies include Zustand setters. LiveKit event listeners are registered inside the `connect()` async function, so they close over the initial `syncParticipants` reference and don't re-register — this is actually safe. But if `syncParticipants` ever changes identity (it won't with stable Zustand setters), the listeners would be stale.

**Proposal:** No immediate change needed, but add a comment noting that `syncParticipants` stability depends on Zustand setters being stable (they are). If voice state is moved out of Zustand (Issue 1.2), revisit this.

---

## 4. React Query Issues

### 4.1 `invalidateQueries` immediately after `setQueryData` — **MEDIUM**

**File:** `hooks/queries/useMessages.ts` lines 163–176

After sending a message, the code does an optimistic `setQueryData` (good) and then immediately calls `invalidateQueries` on the same key. The invalidation marks the data stale and triggers a refetch, defeating the optimistic update.

**Proposal:** Remove the `invalidateQueries` call. Trust the LiveKit `publish_ping` to notify other clients. The sender's own cache is already correct after `setQueryData`.

---

### 4.2 Double invalidation on channel create — **MEDIUM**

**File:** `hooks/queries/useGroups.ts` lines 209–215

Creating a channel invalidates both `channels(groupId)` and `userGroupsWithChannels(userId)`. Since `userGroupsWithChannels` already contains the channels, both invalidations cause separate fetches for overlapping data.

**Proposal:** Invalidate only `userGroupsWithChannels`. Remove the standalone `channels` invalidation. The `useGroupChannels` hook is rarely used independently; it should draw from the same data.

---

### 4.3 Inconsistent `staleTime` values — **LOW**

Across query hooks:
- Groups: `1000 * 60` (1 min)
- Messages: `1000 * 30` (30 sec)
- Reactions: `1000 * 15` (15 sec)
- Preferences: `1000 * 300` (5 min)
- Profile: `1000 * 30` (30 sec)

No comments explain the reasoning.

**Proposal:** Create `frontend/src/hooks/queries/queryConfig.ts` with named constants:
```typescript
export const STALE_TIMES = {
  STATIC: 1000 * 60 * 5,   // preferences, profile — rarely changes
  NORMAL: 1000 * 60,        // groups, channels
  LIVE: 1000 * 30,          // messages, invites — kept fresh by realtime
  FAST: 1000 * 15,          // reactions
} as const;
```
Then reference by name in each hook.

---

### 4.4 `useReactions` is dead code — **LOW**

**File:** `hooks/queries/useReactions.ts`

The hook is defined and exported but never imported or called anywhere. Reactions are rendered inline in `MessageItem.tsx` but the query hook isn't used.

**Proposal:** Delete `useReactions.ts` or add a `// TODO: wire up` comment with a tracking issue.

---

## 5. Architectural / Structural Issues

### 5.1 `useLiveKitRealtime` is 237 lines doing 6 separate jobs — **HIGH**

**File:** `hooks/useLiveKitRealtime.ts`

One hook manages: computing room IDs, building name lookups, tracking window focus, checking notification permission, handling the Tauri channel subscription, and connecting/disconnecting rooms. The 7 stale-closure refs (Issue 2.2) are a symptom of this.

**Proposal:** Split into focused hooks:
- `useAllRoomIds(userId)` — computes `[channelIds..., dmIds...]`
- `useWindowFocus()` — tracks `document.hasFocus()` state
- `useNotificationPermission()` — returns current permission status
- `useRealtimeMessages(roomIds)` — Tauri channel subscription + notification dispatch
- `useRoomConnector(roomIds)` — calls `connect_rooms` / `subscribe_realtime`

Keep `useLiveKitRealtime` as a thin orchestrator that calls these.

---

### 5.2 `AppShell.tsx` is 323 lines doing layout + logic — **MEDIUM**

**File:** `components/Layout/AppShell.tsx`

Manages keyboard shortcuts, route/breadcrumb computation, LiveKit subscription, badge sync, window focus, search panel, voice bar, and all layout chrome. Hard to trace bugs.

**Proposal:** Extract:
- `useKeyboardShortcuts()` — the three keyboard effects (see Issue 2.3)
- `useBreadcrumbFromRoute(pathname, groupsWithChannels)` — already close to extracted in the `useMemo` at line 138
- Delete the groups-to-store sync entirely (Issue 2.1)
- Keep `AppShell` as layout-only: title bar, sidebar, main area, voice bar placement

---

### 5.3 `MainContent` does message query + send mutation + render — **MEDIUM**

**File:** `components/Layout/MainContent.tsx`

One component owns: the `useMessages` query, the `useSendMessage` mutation, reply-to state, `getAuthorUsername` logic, and renders `MessageList`, `ReplyPreview`, `MessageQueue`, and `ChatInput`.

**Proposal:** Extract:
- `<MessagesContainer channelId/conversationId>` — owns the query and renders `<MessageList>`
- `<ChatInputContainer>` — owns the mutation and reply state
- `<MainContent>` becomes a layout wrapper with `<MessagesContainer>` + `<ChatInputContainer>`

---

### 5.4 `usePreferences` returns `{ query, mutation }` — **LOW**

**File:** `hooks/queries/usePreferences.ts` lines 34–73

Callers must write `const { query: prefsQuery } = usePreferences()` which is non-obvious.

**Proposal:** Return the same shape as other query hooks:
```typescript
return {
  data: query.data,
  isLoading: query.isLoading,
  isError: query.isError,
  save: mutation.mutate,
};
```

---

## 6. Other Quality Issues

### 6.1 Multiple raw message types with unclear usage — **LOW**

**File:** `hooks/queries/useMessages.ts` lines 13–32

`RawMessage` and `RawChannelMessage` have overlapping fields but different `sender_username` availability. Two separate transform functions exist. Easy to call the wrong one.

**Proposal:** Merge into one `RawMessage` type with optional `sender_username`. Use one `transformMessage` function everywhere.

---

### 6.2 `console.log` in production code — **LOW**

**Files:** `hooks/useLiveKitRealtime.ts` lines 132–142, `hooks/useVoiceChannel.ts` lines 160, 175, 179

Debug logs reach production builds.

**Proposal:** Wrap in `if (import.meta.env.DEV)`, or create a minimal logger:
```typescript
const log = import.meta.env.DEV ? console.log.bind(console, '[tag]') : () => {};
```

---

### 6.3 `AttachmentDisplay` has two code paths for the same URL — **LOW**

**File:** `components/Message/MessageItem.tsx` lines 143–215

`downloadUrl` is cached in state, but the download handler also calls `getFileDownloadUrl` as a fallback. Two places to maintain.

**Proposal:** Use a single `useQuery` keyed on `attachment.object_key` to cache the URL. The query layer handles deduplication and caching automatically.

---

## Priority Summary

| Priority | Issue | File | One-liner |
|----------|-------|------|-----------|
| HIGH | Delete groups-to-store effect | `AppShell.tsx` L46–54 | Never read from Zustand; delete entirely |
| HIGH | Split `useLiveKitRealtime` | `useLiveKitRealtime.ts` | 237 lines, 7 ref effects; split into 5 hooks |
| HIGH | `MainContent` store over-subscription | `MainContent.tsx` L11–17 | Subscribes to 5 fields, needs 2; use selectors |
| MEDIUM | `messageQueue` is disconnected | `appStore.ts` L72, `MessageQueue.tsx` | Wire up or delete |
| MEDIUM | Voice state in Zustand | `appStore.ts` L47–57 | Move to custom hook; keep only `activeVoiceChannelId` |
| MEDIUM | Optimistic update + invalidation | `useMessages.ts` L163–176 | Remove the invalidation; trust LiveKit ping |
| MEDIUM | Double invalidation on channel create | `useGroups.ts` L209–215 | Invalidate only `userGroupsWithChannels` |
| MEDIUM | `AppShell` doing too much | `AppShell.tsx` | Extract keyboard, breadcrumb, groups-sync |
| MEDIUM | `MainContent` doing too much | `MainContent.tsx` | Extract query/mutation containers |
| LOW | Duplicate `username`/`userAvatarUrl` | `appStore.ts` L6–7 | Read from `currentUser` directly |
| LOW | `getAuthorUsername` not memoized | `MainContent.tsx` L76–78 | `useCallback` with `[currentUser?.id]` |
| LOW | Inconsistent `staleTime` | Multiple | Create `queryConfig.ts` with named constants |
| LOW | `useReactions` dead code | `useReactions.ts` | Delete or document |
| LOW | `usePreferences` weird shape | `usePreferences.ts` | Return `{ data, isLoading, save }` |
| LOW | `console.log` in production | Multiple | `if (import.meta.env.DEV)` guard |
| LOW | Three keyboard effects | `AppShell.tsx` L59–92 | Merge into one |
| LOW | Two raw message types | `useMessages.ts` L13–32 | Merge into one |
| LOW | Attachment URL two code paths | `MessageItem.tsx` L143–215 | Use `useQuery` for URL caching |
