# UI

> **Navigation aid, regenerated — not hand-maintained.** Component inventory and prop
> signatures extracted from the source. Read the source files before adding props or
> modifying component logic.
>
> This inventory had drifted badly (it listed 14 components that no longer existed and
> omitted roughly 50 that did — #804), so treat the filesystem as the authority and
> **regenerate rather than patch** when it disagrees. Prop lists come from each file's
> `*Props` type and are omitted where a component takes none or builds its props inline.

**147 `.tsx` files** under `frontend/src`

## Conventions

- Reusable components live in **their own files**, never inlined in a parent.
- Use the shared primitives in `frontend/src/components/ui/` (`Button`, `Switch`,
  `TextInput`, …) — never a hand-rolled styled equivalent.
- **No modals.** Confirmation and input flows replace the chat input bar (the
  edit/delete bar pattern in `MainContent`) or navigate to a page. The sole exception
  is the Cmd+K search menu.
- Components read MobX stores inside `observer()` wrappers; remote data comes from
  React Query hooks in `frontend/src/hooks/queries/`.

### Design tokens: utilities, not `var()` (#874)

Tokens are CSS custom properties in `frontend/src/index.css` and are surfaced as
semantic Tailwind utilities in `frontend/tailwind.config.js`. **Call sites use the
utilities.** An inline `style={{ color: "var(--c-text-muted)" }}` and a
`text-[var(--c-text-muted)]` arbitrary class are both the wrong spelling of
`text-muted`, and the codebase carried ~650 of the first and ~90 of the second before
this sweep.

| token family | utilities |
|---|---|
| `--c-bg` / `--c-surface[-raised|-high]` | `bg-bg`, `bg-surface`, `bg-surface-raised`, `bg-surface-high` |
| `--c-text` / `-dim` / `-muted` | `text-fg`, `text-dim`, `text-muted` |
| `--c-accent[-bright|-dim|-muted]` | `text-accent…`, `bg-accent…`, `border-accent…`, `ring-accent` |
| `--c-border` / `--c-border-active` | `border-line`, `border-line-strong` |
| `--c-hover` / `--c-active` | `hover:bg-hover`, `bg-active` |
| `--c-danger` | `text-danger`, `border-danger` |
| `--c-voice-connected` | `text-connected` |
| `--bar-h` / `--side-w` | `h-bar`, `min-h-bar`, `w-side` |
| `--radius-chip` / `--radius-control` | `rounded-chip`, `rounded-control` |
| `--msg-header-gap` / `-group-gap` / `-divider-gap` / `--msg-row-pad-y` | `pt-msg-header`, `pt-msg-group`, `mt-msg-divider`, `pb-msg-row` |
| `--lh` | `leading-msg` |

**If a utility is missing, add it to the theme** — that is what the bottom seven rows
above are: tokens that had no utility, so every call site spelled them inline.

Inline `style` is still correct for a value computed at runtime: a per-author
username colour, a percentage width, a `transform`, a component-local custom property
(`--pill-accent`), or a colour handed to a third-party component as a prop rather than
a class (`PollisLogo`, the QR renderer).

Two token references in the codebase are **broken** and deliberately left alone:
`--c-text-accent` (7 sites) is defined nowhere, so those elements inherit their colour
— repainting them with `text-accent` is a design decision, not a rename. And
`--c-focus-ring` is defined but referenced by nothing; focus rings all use
`ring-accent`.

### Render cost (#874)

`observer()` from `mobx-react-lite` **already applies `React.memo`**. There is no
bare `React.memo` on any observer component in this codebase and there must not be
— mobx-react-lite throws if you nest the two. Consequences worth knowing before
touching a list:

- A component wrapped in `observer()` re-renders when (a) a prop fails shallow
  comparison, or (b) an observable it read during render changed. (b) is what makes
  the memo safe to rely on: an observable that changes under a component with equal
  props still re-renders it.
- That safety does **not** extend to non-MobX reactive reads. A React Query
  subscription inside a memoised row re-renders that row itself, which is correct but
  costs one observer per row. List-wide values belong in the list.
- The usual reason a memo does nothing is an **unstable prop from the parent**: an
  inline arrow, a `useMutation` result (new object every render), or an array/Map
  rebuilt per render. Before adding memoisation, check whether the existing memo is
  simply being defeated.

Concretely, in the message log:

- `MessageList` subscribes once to skin, background lightness, viewer and mention
  roster, and passes them to every row as one `useMemo`-stable
  `MessageRenderContext` (`components/Message/messageRenderContext.ts`). Rows and
  `MessageBody` must not re-acquire these per row.
- Callbacks handed to rows (`onScrollToReply`, `onToggleSave`, `onCopyLink`) and to
  `MessageList` from `MainContent` (`onReply`, `onEdit`, `onDelete`, `onLoadMore`,
  `getAuthorUsername`, `focusComposer`) are `useCallback`-pinned, several via refs
  so they stay stable across message arrivals. `MainContent` re-renders on every
  keystroke in the edit bar; unstable identities there re-render the entire log.
- Reply targets are resolved once into a `Map` by `MessageList`, not with a
  `find()` per row.
### The windowed log (#874)

**The log is virtualised.** `MessageList` keeps only the visible slice of the
timeline in the DOM, so layout and paint stopped scaling with history length the
way render cost already had. Measured on a 400-message channel in the terminal
skin: **7,415 elements inside the scroll container before, 538 after** — 400
rendered rows down to 27.

`@tanstack/react-virtual` does the windowing. Three things decided it over
`react-virtuoso` and `react-window`:

- It is **headless**. Day dividers, roster banners, sender grouping and both
  skins' row shapes stay in our own markup, unchanged, inside the window; the
  library only supplies a range and a set of offsets. A library that owns the
  row wrapper would have had to be taught all of that.
- 3.14 has **first-class chat anchoring**: `anchorTo: "end"` re-pins the row the
  reader is looking at across any count change, and `itemSizeCache` is keyed by
  item key rather than index, so prepending 50 older messages does not throw
  away 50 real measurements. That deleted the hand-rolled
  save-scrollTop/restore-by-scrollHeight-delta pair that used to bracket a
  load-more — which windowing would have broken anyway, since the prepended
  rows are off-window and only estimated.
- Same maintainers as `@tanstack/react-query` and `@tanstack/react-router`,
  both already load-bearing here.

Things worth knowing before touching it:

- **Rows are `position: absolute` inside `[data-testid="message-window"]`**,
  which is what makes each row's position independent of its neighbours and
  contains the refined day divider's margins (an escaping margin would leave
  every measurement short and the window drifting). It changes nothing about
  how the hover action bar and its `z-40` menu paint: both are still positioned
  against the row's own `relative` box, and nothing clips.
- **Grouping and day dividers are computed from the full timeline by index**,
  never from the rendered slice. Computing from the slice would put a spurious
  divider and a spurious sender header on the top row of every window.
- **The initial "open at the newest message" anchor is an effect that re-runs**,
  not a one-shot latched on a ref. `followOnAppend` only reacts to the message
  count changing while a scroll container exists, so a conversation whose first
  page arrives in one go has nothing for it to react to; and React's dev-mode
  double mount re-attaches the scroll element, which rewinds the virtualizer to
  the offset it still believes in. A one-shot anchor loses that race silently.
- `followOnAppend: "smooth"` follows new messages down **only from the bottom**.
  Scrolled back through history, an arriving message no longer yanks the
  viewport away — the Slack/Discord behaviour, and a deliberate change from the
  unconditional scroll-to-bottom that came before.
- While a load-more is in flight the "loading" line sits in the scroll container
  above the window, so the virtualizer's offsets are out by that line's height
  for the duration. It is under one row and the overscan absorbs it; do not
  "fix" it by moving the line inside the window container without also setting
  `paddingStart`.

**The three DOM-locating subsystems now ask before they look.** Permalink jumps
(`messageJumpStore`), arrow-key focus projection (`messageNavStore`) and
reply-quote scrolling (`scrollToMessage`) all used to `querySelector` a row that
was guaranteed to exist. `components/Message/messageWindow.ts` owns the
replacement: `findRow` for the synchronous common case (the overscan means an
ordinary one-step move finds its target already rendered) and `revealRow` to
move the window to a target's index and await the commit, with a cancellation
hook so an effect that re-runs abandons an in-flight reveal. Nothing else in the
list may reach into the DOM for a row.

**`olderMessages` is deliberately still uncapped.** Truncating it would be the
wrong trade — "history is bounded, not flaky" bounds what the *server* retains,
not what a scrolled-back user can see — and windowing removes the reason to
want to: an off-window message costs one array entry and one cached height, no
DOM node, no layout, no paint.

**Reading back through history (#927).** `MainContent` owns the paging state:
the older pages it has fetched, and the cursor the next one continues from,
both stamped with the conversation they belong to and read during render. Two
rules follow from what went wrong:

- **Never seed the cursor from an effect.** The pair that used to do it reset
  the pages on a conversation change and then declined to seed a cursor while
  pages were held — but read the *previous* conversation's pages to decide, and
  had no dependency left to re-run on. Reopening a conversation whose first page
  was still cached therefore left it with no cursor at all, pinned to its newest
  50 messages for as long as the cache held it. Deriving both from state keyed by
  the conversation makes that state unrepresentable.
- **A scroll event is not the only thing that means "give me more".** At scroll
  offset 0, and in a viewport the first page does not fill, the browser has no
  scroll left to emit — so `MessageList` re-checks whenever the timeline
  changes, not only on `scroll`. The check itself (still at the top, or not
  scrollable at all) is what stops it running away: the ordinary prepend
  re-anchors the reader onto the row they were reading, well below the
  threshold, so it fetches one page and stops. Anchoring happens in a layout
  effect, i.e. before that re-check, so `scrollTop` there is already the
  post-prepend truth.

**`content-visibility: auto` on the row does not work** — tried and reverted in
#874, and not worth retrying. It implies *paint containment*, which clips the
row's `absolute` hover action bar (`end-4 top-0 -translate-y-1/2`) and its
dropdown menu, both of which deliberately overflow the row box. It failed 8
`bookmarks.spec.ts` cases. It could only work if the action bar were hoisted out
of the row into a single shared overlay tracking the hovered row (the way Slack
does it).

Guards live in `e2e/render-cost.spec.ts` (render cost, backed by
`utils/renderProbe.ts`) and `e2e/message-window.spec.ts` (the window itself, and
every DOM-locating path aimed at a target far outside it). Both run in both
skins. Seeding a paginated conversation in the specs needs `paginate: true` in
the preload — `frontend/src/__mocks__/tauri-core.ts` otherwise hands back the
whole seeded conversation in one page, which is what every other spec wants.
### Query layer (#874)

Four rules, each of which had been broken somewhere and cost real round trips:

- **A list fetches for the whole list, never per row.** The list owner calls one
  batched hook and hands each row its slice as a prop (`useLastMessages` →
  `LastMessagePreview`). A row that fetches for itself multiplies by N and does it
  again on every invalidation.
- **`refetchOnWindowFocus` stays off.** The global default in `main.tsx` is `false`
  for a documented reason; freshness comes from realtime events and explicit
  invalidation. Exactly one hook overrides it — `useMediaPermissions`, because OS
  permission state changes outside the app entirely and no event exists for it. If
  you find yourself wanting the override, the missing piece is usually a realtime
  event: #874 added `membership_changed` publishes to `create_channel`,
  `update_channel` and `update_group`, which is what let the sidebar drop it.
- **A cache key names its INPUTS, in full.** Not their count
  (`useAllPendingJoinRequests` was keyed on `adminGroupIds.length`, so two different
  sets of three groups shared one entry), and not a key already meaning something
  else (`useUserProfile` and `useOtherUserProfile` shared one key holding two
  different response shapes).
- **Invalidate the narrowest prefix that actually changed.** `['groups']` also
  matches every group's channel list; `membership_changed` used to invalidate all of
  it. Where a key puts the id in the middle (`["groups", <id>, "members"]`) and no
  prefix selects the right set, use a `predicate` rather than widening.

Counts are asserted, not assumed: `e2e/ipc-efficiency.spec.ts` reads the per-command
tally the Tauri mock keeps (`window.__tauriInvokeCounts`) and pins how many calls each
interaction may make.

### Presence

`presenceStore` infers online-ness from LiveKit room participation, keeping a
`user_id → Set<room_id>` map so leaving one shared room doesn't flip someone
offline while they're still in another. Two properties are load-bearing:

- **`isOnline` is deliberately NOT a MobX action.** Actions run untracked, so an
  `action` here would leave its `byUser` reads invisible to the calling
  `observer()` and presence would stop updating live. It stays a plain method,
  excluded via `makeAutoObservable(this, { isOnline: false })`.
- **The local user is tracked separately.** Room events only ever describe
  *other* participants — nothing tells us we are online — so `byUser` never
  contains us and every surface would report the signed-in user as offline.
  `appStore.setCurrentUser` (and `logout`) pushes the id into
  `presenceStore.setSelfUserId`, and `isOnline` short-circuits true for it. The
  dependency is one-way (`appStore → presenceStore`; presence imports nothing of
  ours) so it cannot cycle. Don't paper over this at a call site with a
  hardcoded `presence="online"`.

## Components

### `(root)` (3)

- **App** — `frontend/src/App.tsx`
- **main** — `frontend/src/main.tsx`
- **router** — `frontend/src/router.tsx`

### `components` (6)

- **ErrorBoundary** — `frontend/src/components/ErrorBoundary.tsx`
- **SearchPanel** — props: isOpen, onClose — `frontend/src/components/SearchPanel.tsx`
- **TerminalApp** — props: onLogout, onLock, onDeleteAccount — `frontend/src/components/TerminalApp.tsx`
- **TerminalView** — props: visible — `frontend/src/components/TerminalView.tsx`
- **TypingIndicator** — props: channelId, conversationId — `frontend/src/components/TypingIndicator.tsx`
- **UpdateScreen** — `frontend/src/components/UpdateScreen.tsx`

### `components/Auth` (7)

- **EmailOTPAuth** — props: onSuccess, prefillEmail, prefillNonce, onStepChange — `frontend/src/components/Auth/EmailOTPAuth.tsx`
- **EnrollmentApprovalPrompt** — props: requestId, newDeviceId, verificationCode, onResolved — `frontend/src/components/Auth/EnrollmentApprovalPrompt.tsx`
- **EnrollmentGateScreen** — props: userId, userEmail, onEnrolled, onCancel, onResetComplete — `frontend/src/components/Auth/EnrollmentGateScreen.tsx`
- **LoginScreen** — props: knownAccounts, onAuthSuccess, onWipeComplete — `frontend/src/components/Auth/LoginScreen.tsx`
- **PinCreateScreen** — props: oldPin, onCreated, onCancel, headline, subline — `frontend/src/components/Auth/PinCreateScreen.tsx`
- **PinEntryScreen** — props: userId, username, onUnlocked, onForgotPin, onSwitchAccount — `frontend/src/components/Auth/PinEntryScreen.tsx`
- **SaveSecretKeyScreen** — props: secretKey, email, onConfirmed — `frontend/src/components/Auth/SaveSecretKeyScreen.tsx`

### `components/Layout` (14)

- **AppShell** — `frontend/src/components/Layout/AppShell.tsx`
- **BreadcrumbNav** — `frontend/src/components/Layout/BreadcrumbNav.tsx`
- **MainContent** — props: pendingDmRequest — `frontend/src/components/Layout/MainContent.tsx`
- **PageShell** — props: title, children, scrollable — `frontend/src/components/Layout/PageShell.tsx`
- **Sidebar** — props: isOpen, onToggle — `frontend/src/components/Layout/Sidebar.tsx`
- **SidebarProfilePanel** — refined only; identity row + the persistent voice strip (channel, mic, deafen, screenshare, disconnect) that replaces terminal's `VoiceBar` — `frontend/src/components/Layout/SidebarProfilePanel.tsx`
- **StatusBarSummary** — props: icon, count, to, label, color, testId — `frontend/src/components/Layout/StatusBarSummary.tsx`
- **TitleBar** — `frontend/src/components/Layout/TitleBar.tsx`
- **WindowResizeEdges** — `frontend/src/components/Layout/WindowResizeEdges.tsx`

#### `components/Layout/RightPanel` — right-hand context panel (#824)

- **RightPanel** — the generic slot — `frontend/src/components/Layout/RightPanel/RightPanel.tsx`
- **MembersPanel** — props: groupId, channelId, conversationId — `frontend/src/components/Layout/RightPanel/MembersPanel.tsx`
- **ThreadPanel** — props: threadId, channelId, conversationId — `frontend/src/components/Layout/RightPanel/ThreadPanel.tsx`
- **ThreadMessageRow** — props: message, isRoot — `frontend/src/components/Layout/RightPanel/ThreadMessageRow.tsx`
- **MemberRow** — props: userId, label, avatarKey, isAdmin — `frontend/src/components/Layout/RightPanel/MemberRow.tsx`
- **MediaGrid** — props: attachments — `frontend/src/components/Layout/RightPanel/MediaGrid.tsx`
- **MediaTile** — props: attachment — `frontend/src/components/Layout/RightPanel/MediaTile.tsx`

The panel is a **flex sibling of `<Outlet />`** in `AppShell`, never an overlay — opening it reflows the message list.

**Open/closed and shape are two different pieces of state (#904).**

| State | Where it lives | Reacts to the route? |
| --- | --- | --- |
| Is the panel open? | `stores/rightPanelStore` → `localStorage` key `pollis-right-panel-open:<userId>` | **No.** Only the user toggling it moves it. |
| Which shape, and whose data? | `?thread=<ULID>` for a thread; otherwise the members roster, scoped by `appStore` selection | **Yes.** That is what the panel is for. |

Open/closed used to be a `?panel=` search param, which is what broke it: nothing in the app carries search params across a navigation (every `router.navigate({ to })` produces a bare URL), so the param was dropped on every sidebar click and the panel fell back to a SKIN default — shut in `terminal`, open in `refined`. One bug, two faces: a terminal panel that would not stay open, a refined one that would not stay shut. It is now device-local and **keyed by user id**, the same scheme as the device-local language (`i18n/storage.ts`) and font size (`loadDeviceFontSize`), so a shared OS account never leaks one person's layout to the next.

**Seeding.** A device with nothing remembered falls back to the synced `right_panel_open_by_default` preference, else the skin's historical default — and then **writes that answer down**, once the preferences query has settled (the skin is read from that same query, so an earlier seed would record the loading placeholder). The write is what keeps the skin out of it: after seeding, the skin is never consulted again, so switching between `terminal` and `refined` cannot open or close the panel. The Preferences switch writes BOTH halves — device-local (so it actually moves the panel you are looking at) and synced (so the next new device seeds from it).

`validatePanelSearch` (`frontend/src/types/panel.ts`) is declared on the **root** route, because the panel is AppShell's chrome rather than any one page's. It drops unrecognised values instead of throwing, so a stale or hand-edited link falls back to a plain conversation view rather than blanking the app. `useRightPanel` owns the only write path for both halves.

**`useRightPanel` navigates with an explicit `to: pathname`** — a relative `to: "."` would resolve against `/` (AppShell renders at the root route) and throw the user out of their conversation, and omitting `to` leaves the search type unresolved. It navigates *only* when a thread id actually needs clearing; plain open/close touches the URL not at all.

Coverage: `e2e/right-panel-persistence.spec.ts`, both skins.

**Chrome mirrors the left sidebar.** The panel's close affordance is a **footer**, not a header: a full-width `min-h-bar border-t border-line` button with the label on the left and a `<kbd>` on the right carrying the live `useShortcutLabel("app.toggleRightPanel")` combo (default `mod+shift+b`, the shifted twin of the sidebar's `mod+b`). AppShell binds the same command id, so the chip and the key can't drift. Width tracks `--side-w` and the aside carries `font-mono`, which is the entire skin switch — terminal renders DM Mono, refined re-points `.font-mono:not(.font-machine)` at the sans face. Terminal additionally borrows the sidebar's `SectionHeader` chrome (sticky `h-bar` strip, hairline under, a second hairline above every section after the first) and its one-text-line rows with a bare `PresenceDot`; refined keeps the airier avatar rows.

**Content is contextual, the column is not.** The panel no longer collapses on routes with no conversation (Preferences, Search, root). `MembersPanel` degrades there to the plain online roster — self plus everyone visible in `presenceStore.byUser`, named from the DM list — and drops the media grid, since "who is online" still answers a question off-conversation and "media shared in this conversation" does not. A stale `?thread=` on such a route falls back to the roster rather than rendering an empty thread.

### `components/Message` (10)

- **AttachmentDisplay** — `frontend/src/components/Message/AttachmentDisplay.tsx`
- **LastMessagePreview** — props: message, isLoading — `frontend/src/components/Message/LastMessagePreview.tsx`
- **MediaLinkUnfurl** — props: text — `frontend/src/components/Message/MediaLinkUnfurl.tsx`
- **MessageActions** — props: messageId, variant, isOwn, canModerate, isSaved, copyLinkState, onReply, onOpenThread, onToggleSave, onCopyLink, onEdit, onDelete — `frontend/src/components/Message/MessageActions.tsx`. The per-message hover toolbar, shared by both skins: Reply, Edit (own messages), and a "more" trigger whose anchored menu (icon + label rows, Delete last) carries thread/save/copy-link/delete. Non-modal — `absolute` inside its own `relative` wrapper, same shape as `EmojiPickerButton`. Its Escape claim uses `stopImmediatePropagation` so closing the menu never also fires the window-level `nav.back` Escape shortcut.
- **MessageAvatar** — props: userId, username, size — `frontend/src/components/Message/MessageAvatar.tsx`
- **MessageBody** — props: text, ctx — `frontend/src/components/Message/MessageBody.tsx`. Renders resolving `@mentions` as tokens and delegates the rest to `LinkifiedText`. Plain `React.memo`, not `observer()` — it reads no observables, taking skin and the mention roster from `ctx`.
- **MessageItem** — props: message, ctx, replyToMessage, authorUsername, isAuthorAdmin, canModerate, isGroupStart, onReply, onOpenThread, threadReplyCount, onEdit, onDelete, onToggleSave, onCopyLink, copyLinkState, isSaved, onScrollToReply, receipt, peerCount, isDm — `frontend/src/components/Message/MessageItem.tsx`. Memoised via `observer()`. `ctx` is the list-wide `MessageRenderContext`; `replyToMessage` and `receipt` are resolved per row BY THE LIST, replacing an `allMessages.find()` (O(N^2)) and a whole-`Map` receipts prop that re-rendered every row whenever any one receipt landed.
- **MessageList** — props: messages, conversationId, groupIdForNames, adminUserIds, viewerIsAdmin, onReply, onOpenThread, threadReplyCounts, onEdit, onDelete, onScrollToMessage, getAuthorUsername, hasMore, isFetchingMore, onLoadMore, focusComposer — `frontend/src/components/Message/MessageList.tsx`. Passing `focusComposer` opts the list into arrow-key log navigation (bash-history style): ArrowUp from an empty/first-line composer walks the log, Left/Right walk the focused row's action bar, ArrowDown past the newest (or Tab/Escape) returns to the composer. The pure state machine lives in `utils/messageNav.ts` (unit-pinned by `frontend/tests/message-nav.test.ts`), the live state in `stores/messageNavStore.ts`, and rows style keyboard focus purely via CSS `focus-within` so keystrokes re-render nothing; browser-level coverage is `e2e/message-nav.spec.ts`. Windowed via `@tanstack/react-virtual` — see "The windowed log" above; `messageWindow.ts` is the only door from a message id to a live row.
- **messageWindow** (not a component) — `frontend/src/components/Message/messageWindow.ts`. Row-height estimates in rem, the overscan, and `findRow`/`revealRow`, the only sanctioned way to get from a message id to a rendered row now that the log is windowed.
- **MessageQueue** — `frontend/src/components/Message/MessageQueue.tsx`
- **ReplyPreview** — props: messageId, allMessages, onDismiss, onScrollToMessage — `frontend/src/components/Message/ReplyPreview.tsx`

### `components/Search` (1)

- **SearchView** — props: onNavigateToConversation — `frontend/src/components/Search/SearchView.tsx`

### `components/Security` (3)

- **AccountKeyAuditLine** — props: status, detail, testId — `frontend/src/components/Security/AccountKeyAuditLine.tsx`
- **BuildVerifyLine** — props: status, detail, testId — `frontend/src/components/Security/BuildVerifyLine.tsx`
- **KeyChangeBanner** — props: peerUserId, peerLabel — `frontend/src/components/Security/KeyChangeBanner.tsx`

### `components/Voice` (9)

- **CameraPicker** — `frontend/src/components/Voice/CameraPicker.tsx`
- **SidebarVoiceControls** — mic (four-state) + deafen for refined's sidebar voice strip; shares `micIndicatorOf` and `voiceControlLabels` with `VoiceBar` / `VoiceStage` — `frontend/src/components/Voice/SidebarVoiceControls.tsx`
- **RemoteUserVolumeSlider** — props: identity, participantName — `frontend/src/components/Voice/RemoteUserVolumeSlider.tsx`
- **RemoteVideoTile** — props: trackKey, className, initialWidth, initialHeight, preview, mirror — `frontend/src/components/Voice/RemoteVideoTile.tsx`
- **ScreenSharePicker** — props: disabled, onPick, title, subtitle, thumbnail, icon — `frontend/src/components/Voice/ScreenSharePicker.tsx`
- **ScreenShareViewer** — `frontend/src/components/Voice/ScreenShareViewer.tsx`
- **VoiceBar** — props: channelId, channelName — `frontend/src/components/Voice/VoiceBar.tsx`
- **StageTile** — props: participant, mode, focused, onFocus, onView — `frontend/src/components/Voice/stage/StageTile.tsx`
- **VoiceStage** — props: channelName, isInCall, onJoin, onLeave, onBack, onOpenSettings, observerParticipants, headerActions, footer, callMode — `frontend/src/components/Voice/stage/VoiceStage.tsx`

### `components/ui` (22)

- **AudioPlayer** — props: src, title, className, autoPlay, loop, preload — `frontend/src/components/ui/AudioPlayer.tsx`
- **Avatar** — props: avatarKey, size, alt, testId, variant, presence — `frontend/src/components/ui/Avatar.tsx`
- **Button** — props: children, onClick, disabled, isLoading, loadingText, className, variant, size, type, onKeyDown, autoFocus — `frontend/src/components/ui/Button.tsx`
- **Card** — props: children, className, style, padding — `frontend/src/components/ui/Card.tsx`
- **ChatInput** — props: onSend, placeholder, disabled, autoFocus, className, maxAttachments, onValueChange, draftKey, canNotifyAll — `frontend/src/components/ui/ChatInput.tsx`
- **Checkbox** — props: label, checked, onChange, disabled, className — `frontend/src/components/ui/Checkbox.tsx`
- **ALL_ALGORITHMS** — props: algorithm, dotSize, spacing, speed, className, style — `frontend/src/components/ui/DotMatrix.tsx`
- **InlineAudioPlayer** — props: src, title, className, autoPlay, onClick — `frontend/src/components/ui/InlineAudioPlayer.tsx`
- **InputOtp** — props: length, value, onChange, disabled, autoFocus, mask — `frontend/src/components/ui/InputOtp.tsx`
- **EmptyState** — props: children, testId, messageTestId, tone, background, actions — `frontend/src/components/ui/EmptyState.tsx`. The centred "nothing here" line; hand-rolled a dozen times before it existed (#874). Not a loading state.
- **LinkifiedText** — props: text — `frontend/src/components/ui/LinkifiedText.tsx`. URL detection and `ensureProtocol` live in `frontend/src/utils/links.ts`, shared with `MediaLinkUnfurl`; the `/g` regex is reset inside the single shared scanner, which is what makes it safe to share (#874).
- **LoadingSpinner** — props: size, className — `frontend/src/components/ui/LoaderSpinner.tsx`
- **NavigableGrid** — `frontend/src/components/ui/NavigableGrid.tsx`
- **NavigableList** — `frontend/src/components/ui/NavigableList.tsx`
- **PillButton** — props: accent, onClick, title, square, children — `frontend/src/components/ui/PillButton.tsx`
- **PollisLogo** — props: size, color — `frontend/src/components/ui/PollisLogo.tsx`
- **PresenceAvatar** — props: userId, avatarKey, size, alt, testId, variant — `frontend/src/components/ui/PresenceAvatar.tsx`
- **PresenceDot** — props: userId, testId — bare online/offline dot for rows with no avatar to anchor it (terminal sidebar DMs, right-panel members) — `frontend/src/components/ui/PresenceDot.tsx`
- **RangeSlider** — props: label, value, onChange, min, max, step, disabled, className, id, sublabel, description — `frontend/src/components/ui/RangeSlider.tsx`
- **ScrambleText** — props: text, placeholderLength, typeSpeed, scrambleInterval, className — `frontend/src/components/ui/ScrambleText.tsx`
- **Switch** — props: label, checked, onChange, disabled, className, id, description — `frontend/src/components/ui/Switch.tsx`
- **TerminalMenu** — props: items, onEsc, className, autoFocus — `frontend/src/components/ui/TerminalMenu.tsx`
- **TextArea** — props: label, value, onChange, placeholder, description, error, disabled, rows, className, id — `frontend/src/components/ui/TextArea.tsx`
- **TextInput** — props: label, value, onChange, placeholder, description, error, disabled, type, className, id, required, autoFocus, autoComplete — `frontend/src/components/ui/TextInput.tsx`

### `pages` (43)

- **AllJoinRequestsPage** — `frontend/src/pages/AllJoinRequestsPage.tsx`
- **ArcadePage** — `frontend/src/pages/ArcadePage.tsx`
- **BlockedPage** — `frontend/src/pages/BlockedPage.tsx`
- **CallPage** — `frontend/src/pages/Call.tsx`
- **ChangePinPage** — `frontend/src/pages/ChangePinPage.tsx`
- **ChannelPage** — `frontend/src/pages/Channel.tsx`
- **CreateChannel** — props: onSuccess — `frontend/src/pages/CreateChannel.tsx`
- **CreateChannelPage** — `frontend/src/pages/CreateChannelPage.tsx`
- **CreateGroup** — props: onSuccess — `frontend/src/pages/CreateGroup.tsx`
- **CreateGroupPage** — `frontend/src/pages/CreateGroupPage.tsx`
- **DMPage** — `frontend/src/pages/DM.tsx`
- **DMSettingsPage** — `frontend/src/pages/DMSettings.tsx`
- **DMsPage** — `frontend/src/pages/DMs.tsx`
- **GroupPage** — `frontend/src/pages/Group.tsx`
- **GroupsPage** — `frontend/src/pages/Groups.tsx`
- **InviteMember** — props: groupId, groupName — `frontend/src/pages/InviteMember.tsx`
- **InviteMemberPage** — `frontend/src/pages/InviteMemberPage.tsx`
- **InvitesPage** — `frontend/src/pages/InvitesPage.tsx`
- **JoinRequests** — props: groupId, groupName — `frontend/src/pages/JoinRequests.tsx`
- **JoinRequestsPage** — `frontend/src/pages/JoinRequestsPage.tsx`
- **KeyboardShortcutsPage** — `frontend/src/pages/KeyboardShortcutsPage.tsx`
- **KickMemberPage** — `frontend/src/pages/KickMemberPage.tsx`
- **LeaveGroupPage** — `frontend/src/pages/LeaveGroup.tsx`
- **Members** — props: groupId, isAdmin — `frontend/src/pages/Members.tsx`
- **MembersPage** — `frontend/src/pages/MembersPage.tsx`
- **PreferencesPage** — `frontend/src/pages/PreferencesPage.tsx`
- **RenameChannelPage** — `frontend/src/pages/RenameChannelPage.tsx`
- **RenameEntity** — props: kind, groupId, channelId, onSuccess — `frontend/src/pages/RenameEntity.tsx`. One form for both routes; `RenameGroup`/`RenameChannel` were eleven normalised lines apart and are gone (#874).
- **RenameGroupPage** — `frontend/src/pages/RenameGroupPage.tsx`
- **RequestsPage** — `frontend/src/pages/RequestsPage.tsx`
- **RootPage** — `frontend/src/pages/Root.tsx`
- **SearchPage** — `frontend/src/pages/Search.tsx`
- **SearchGroupPage** — `frontend/src/pages/SearchGroupPage.tsx`
- **SecurityPage** — `frontend/src/pages/SecurityPage.tsx`
- **SettingsHubPage** — `frontend/src/pages/SettingsHub.tsx`
- **SettingsPage** — `frontend/src/pages/SettingsPage.tsx`
- **StartDM** — props: onSuccess — `frontend/src/pages/StartDM.tsx`
- **StartDMPage** — `frontend/src/pages/StartDMPage.tsx`
- **UpdatePage** — `frontend/src/pages/UpdatePage.tsx`
- **UserProfilePage** — `frontend/src/pages/UserProfile.tsx`
- **VoiceChannelPage** — `frontend/src/pages/VoiceChannel.tsx`
- **VoiceSettingsPage** — props: label, devices, value, onChange, fallbackLabel — `frontend/src/pages/VoiceSettingsPage.tsx`

---

_Back to [index.md](./index.md)_
