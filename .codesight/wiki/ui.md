# UI

> **Two halves, and they are maintained differently.**
>
> Everything down to [Components](#components) is **prose, written by hand** — the
> conventions, the token rules, why the log is windowed the way it is. Change the
> code, change the paragraph.
>
> [Components](#components) is a **generated inventory**: `node scripts/ui-inventory.mjs`
> reads `frontend/src` and rewrites it, and `--check` fails when it is stale.
> Never hand-patch a name, a path or a count there — regenerate. Per-component
> notes written after the path DO survive regeneration, and are the right place
> for anything an extractor could not know.
>
> The article told readers to "regenerate rather than patch" for a long time
> with no generator in the repo, which is how the count drifted by a third —
> 111 claimed against 147 real, with an entire directory (`components/Emoji`)
> missing. That instruction is executable now (#933).
>
> Read the source before adding props or modifying component logic; the
> inventory is for finding the file, not for understanding it.

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
- **The threshold is in rem, and read at call time (#934).** `loadMoreThresholdPx()`
  in `messageWindow.ts`, 10rem — which is the old hardcoded 150px exactly, at the
  15px default root. A px constant meant a reader on a 20px root got taller rows
  and the same 150px of slack, i.e. a trigger that fired proportionally later in
  what they could see. Reading it per call rather than capturing it is what lets it
  follow a font-size change under a mounted log.
- **The prepend and the end of the load are two commits, deliberately (#934).**
  `MainContent` used to clear `loadingMore` in the same `finally` that prepended
  the page; React batches both into one commit, so no committed state ever had the
  new rows present while the log still called itself busy. The successful path now
  ends in a `useEffect` keyed on `olderPages` — which runs after the commit
  carrying the rows, and after the virtualiser's layout-effect re-anchor — while
  only the paths that never reached the prepend clear it inline. `e2e/message-window.spec.ts`
  pins the seam with a `MutationObserver`, since a state that lasts one commit
  cannot be polled for.

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
- **A `queryFn` fetches and returns. It does not write to a store (#928.)** React
  Query runs it on refetch, on retry and on cache-restore, in no guaranteed order
  against render, and skips it entirely on a cache hit — so a store write inside one
  is mutated by background refetches nobody asked for, and *missed* exactly when the
  data was already cached. `useUserProfile` hydrated `appStore.userAvatarUrl` from
  its `queryFn` (the local voice tile reads that field, and `VoiceSessionManager` is
  not a React consumer); it is now a `useEffect` keyed on the resolved value.
  Denormalising query data into a store at all is a last resort for non-React
  readers — prefer reading the query. Guarded by
  `frontend/tests/query-store-boundary.test.ts`, which scans every `queryFn` in
  `hooks/queries/`.

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

### The right panel (#824, #904)

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

## Components

> Generated from `frontend/src` by `scripts/ui-inventory.mjs` — the article said
> "regenerate rather than patch" for a long time before anything could (#933).
> Everything a line carries **after** its backticked path is a hand-written note
> and survives regeneration; everything before it does not, so prose belongs
> there or in a section above.

<!-- BEGIN GENERATED: component inventory (scripts/ui-inventory.mjs) -->
**148 `.tsx` files** under `frontend/src`, by directory. Regenerate with
`node scripts/ui-inventory.mjs`; `--check` fails if this is stale.

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

### `components/Emoji` (9)

- **CustomEmojiImage** — props: contentHash, shortcode, sizeRem, className — `frontend/src/components/Emoji/CustomEmojiImage.tsx`
- **EmojiCategoryRail** — props: entries, activeId, onJump — `frontend/src/components/Emoji/EmojiCategoryRail.tsx`
- **EmojiCell** — props: item, toneIndex, index, onSelect, onPreview — `frontend/src/components/Emoji/EmojiCell.tsx`
- **EmojiDropZone** — props: onPick, onReject, disabled, children — `frontend/src/components/Emoji/EmojiDropZone.tsx`. The image-first half of the custom-emoji upload (Discord's model): drop a gif/image anywhere on the window or click to browse, and the file's PATH — never its bytes — goes to the caller. Registers as an *inline* drop target so `AppShell`'s full-window drop overlay stays out of the way and this zone lights up instead; a native drag never reaches the DOM, so "drag-over" is a window-level fact rather than a hover.
- **EmojiPicker** — props: onSelect, onClose, closeOnSelect, className — `frontend/src/components/Emoji/EmojiPicker.tsx`
- **EmojiPickerButton** — props: onSelect, closeOnSelect, placement, align, className, ariaLabel — `frontend/src/components/Emoji/EmojiPickerButton.tsx`
- **EmojiSection** — props: id, title, items, toneIndex, baseIndex, onSelect, onPreview, scrollRoot — `frontend/src/components/Emoji/EmojiSection.tsx`
- **EmojiText** — props: text, renderText, jumbo — `frontend/src/components/Emoji/EmojiText.tsx`
- **SkinTonePicker** — props: toneIndex, onChange — `frontend/src/components/Emoji/SkinTonePicker.tsx`

### `components/Invites` (3)

- **CreatedInviteLinkCard** — props: link — `frontend/src/components/Invites/CreatedInviteLinkCard.tsx`
- **InviteLinkManager** — props: groupId — `frontend/src/components/Invites/InviteLinkManager.tsx`
- **InviteLinkRow** — props: link, onRevoke, isRevoking — `frontend/src/components/Invites/InviteLinkRow.tsx`

### `components/Layout` (9)

- **AppShell** — `frontend/src/components/Layout/AppShell.tsx`
- **BreadcrumbNav** — `frontend/src/components/Layout/BreadcrumbNav.tsx`
- **MainContent** — props: pendingDmRequest — `frontend/src/components/Layout/MainContent.tsx`
- **PageShell** — props: title, children, scrollable — `frontend/src/components/Layout/PageShell.tsx`
- **Sidebar** — props: isOpen, onToggle — `frontend/src/components/Layout/Sidebar.tsx`. Its settings rows carry `sidebar-row-*` testids like `SectionHeader`'s, so navigation tests never match on a translated label (#932).
- **SidebarProfilePanel** — `frontend/src/components/Layout/SidebarProfilePanel.tsx`. Refined only; identity row + the persistent voice strip (channel, mic, deafen, screenshare, disconnect) that replaces terminal's `VoiceBar`.
- **StatusBarSummary** — props: color — `frontend/src/components/Layout/StatusBarSummary.tsx`
- **TitleBar** — `frontend/src/components/Layout/TitleBar.tsx`
- **WindowResizeEdges** — `frontend/src/components/Layout/WindowResizeEdges.tsx`

### `components/Layout/RightPanel` (7)

- **MediaGrid** — props: attachments — `frontend/src/components/Layout/RightPanel/MediaGrid.tsx`
- **MediaTile** — props: attachment — `frontend/src/components/Layout/RightPanel/MediaTile.tsx`
- **MemberRow** — props: userId, label, avatarKey, isAdmin — `frontend/src/components/Layout/RightPanel/MemberRow.tsx`
- **MembersPanel** — props: groupId, channelId, conversationId — `frontend/src/components/Layout/RightPanel/MembersPanel.tsx`
- **RightPanel** — `frontend/src/components/Layout/RightPanel/RightPanel.tsx`. The generic slot.
- **ThreadMessageRow** — props: message, isRoot — `frontend/src/components/Layout/RightPanel/ThreadMessageRow.tsx`
- **ThreadPanel** — props: threadId, channelId, conversationId — `frontend/src/components/Layout/RightPanel/ThreadPanel.tsx`

### `components/Message` (13)

- **AttachmentDisplay** — `frontend/src/components/Message/AttachmentDisplay.tsx`
- **LastMessagePreview** — props: message, isLoading — `frontend/src/components/Message/LastMessagePreview.tsx`
- **MediaLinkUnfurl** — props: text — `frontend/src/components/Message/MediaLinkUnfurl.tsx`
- **MentionToken** — props: name, isSelf, skin — `frontend/src/components/Message/MentionToken.tsx`
- **MessageActions** — props: messageId, variant, isOwn, canModerate, isSaved, copyLinkState, onReply, onOpenThread, onToggleSave, onCopyLink, onEdit, onDelete — `frontend/src/components/Message/MessageActions.tsx`. The per-message hover toolbar, shared by both skins: Reply, Edit (own messages), and a "more" trigger whose anchored menu (icon + label rows, Delete last) carries thread/save/copy-link/delete. Non-modal — `absolute` inside its own `relative` wrapper, same shape as `EmojiPickerButton`. Its Escape claim uses `stopImmediatePropagation` so closing the menu never also fires the window-level `nav.back` Escape shortcut.
- **MessageAvatar** — props: userId, username, size — `frontend/src/components/Message/MessageAvatar.tsx`
- **MessageBody** — props: text, ctx — `frontend/src/components/Message/MessageBody.tsx`. Renders resolving `@mentions` as tokens and delegates the rest to `LinkifiedText`. Plain `React.memo`, not `observer()` — it reads no observables, taking skin and the mention roster from `ctx`.
- **MessageItem** — props: message, ctx, replyToMessage, authorUsername, isAuthorAdmin, canModerate, isGroupStart, onReply, onOpenThread, threadReplyCount, onEdit, onDelete, onToggleSave, onCopyLink, copyLinkState, isSaved, onScrollToReply, receipt, peerCount, isDm — `frontend/src/components/Message/MessageItem.tsx`. Memoised via `observer()`. `ctx` is the list-wide `MessageRenderContext`; `replyToMessage` and `receipt` are resolved per row BY THE LIST, replacing an `allMessages.find()` (O(N^2)) and a whole-`Map` receipts prop that re-rendered every row whenever any one receipt landed.
- **MessageList** — props: messages, conversationId, groupIdForNames, adminUserIds, viewerIsAdmin, onReply, onOpenThread, threadReplyCounts, onEdit, onDelete, onScrollToMessage, getAuthorUsername, hasMore, isFetchingMore, onLoadMore, focusComposer — `frontend/src/components/Message/MessageList.tsx`. Passing `focusComposer` opts the list into arrow-key log navigation (bash-history style): ArrowUp from an empty/first-line composer walks the log, Left/Right walk the focused row's action bar, ArrowDown past the newest (or Tab/Escape) returns to the composer. The pure state machine lives in `utils/messageNav.ts` (unit-pinned by `frontend/tests/message-nav.test.ts`), the live state in `stores/messageNavStore.ts`, and rows style keyboard focus purely via CSS `focus-within` so keystrokes re-render nothing; browser-level coverage is `e2e/message-nav.spec.ts`. Windowed via `@tanstack/react-virtual` — see "The windowed log" above; `components/Message/messageWindow.ts` (not a `.tsx`, so not listed here) is the only door from a message id to a live row, and owns the rem-based row estimates and load-more threshold (#934).
- **MessageQueue** — `frontend/src/components/Message/MessageQueue.tsx`
- **ReceiptIndicator** — props: receipts, peerCount, visible — `frontend/src/components/Message/ReceiptIndicator.tsx`
- **ReplyPreview** — props: messageId, allMessages, onDismiss, onScrollToMessage — `frontend/src/components/Message/ReplyPreview.tsx`
- **ThreadReplyCount** — props: count, onOpen — `frontend/src/components/Message/ThreadReplyCount.tsx`

### `components/Preferences` (2)

- **LanguageSection** — props: userId — `frontend/src/components/Preferences/LanguageSection.tsx`
- **RelayServingSection** — props: config, status, applyError, onChange — `frontend/src/components/Preferences/RelayServingSection.tsx`

### `components/Saved` (1)

- **SavedMessageRow** — props: item, conversationLabel, senderLabel — `frontend/src/components/Saved/SavedMessageRow.tsx`

### `components/Search` (1)

- **SearchView** — props: onNavigateToConversation — `frontend/src/components/Search/SearchView.tsx`

### `components/Security` (3)

- **AccountKeyAuditLine** — props: status, detail, testId — `frontend/src/components/Security/AccountKeyAuditLine.tsx`
- **BuildVerifyLine** — props: status, detail, testId — `frontend/src/components/Security/BuildVerifyLine.tsx`
- **KeyChangeBanner** — props: peerUserId, peerLabel — `frontend/src/components/Security/KeyChangeBanner.tsx`

### `components/ui` (26)

- **AudioPlayer** — props: src, title, className, autoPlay, loop, preload — `frontend/src/components/ui/AudioPlayer.tsx`
- **Avatar** — props: avatarKey, size, alt, testId, variant, presence — `frontend/src/components/ui/Avatar.tsx`
- **Button** — props: children, onClick, disabled, isLoading, loadingText, className, variant, size, type, onKeyDown, autoFocus — `frontend/src/components/ui/Button.tsx`
- **Card** — props: children, className, style, padding — `frontend/src/components/ui/Card.tsx`
- **ChatInput** — props: onSend, placeholder, disabled, autoFocus, className, maxAttachments, onValueChange, draftKey, canNotifyAll, onHistoryUp — `frontend/src/components/ui/ChatInput.tsx`
- **Checkbox** — props: label, checked, onChange, disabled, className — `frontend/src/components/ui/Checkbox.tsx`
- **DotMatrix** — props: algorithm, dotSize, spacing, speed, className, style — `frontend/src/components/ui/DotMatrix.tsx`
- **EmptyState** — props: children, testId, messageTestId, tone, background, actions — `frontend/src/components/ui/EmptyState.tsx`. The centred "nothing here" line; hand-rolled a dozen times before it existed (#874). Not a loading state.
- **InlineAudioPlayer** — props: src, title, className, autoPlay, onClick — `frontend/src/components/ui/InlineAudioPlayer.tsx`
- **InputOtp** — props: length, value, onChange, disabled, autoFocus, mask — `frontend/src/components/ui/InputOtp.tsx`
- **LinkifiedText** — props: text — `frontend/src/components/ui/LinkifiedText.tsx`. URL detection and `ensureProtocol` live in `frontend/src/utils/links.ts`, shared with `MediaLinkUnfurl`; the `/g` regex is reset inside the single shared scanner, which is what makes it safe to share (#874).
- **LoadingSpinner** — props: size, className — `frontend/src/components/ui/LoaderSpinner.tsx`
- **MentionGhost** — props: value, ghost, focused, scrollTop — `frontend/src/components/ui/MentionGhost.tsx`
- **MentionSuggestList** — props: candidates, activeIndex, query, onSelect, onHover — `frontend/src/components/ui/MentionSuggestList.tsx`
- **NavigableGrid** — props: items, getKey, renderCell, onActivate, minCellWidth, maxCellWidth, aspect, gap, autoFocus, emptyLabel, testId — `frontend/src/components/ui/NavigableGrid.tsx`
- **NavigableList** — props: items, getKey, renderRow, controls, trailing, onEnterRow, onClickRow, isLoading, loadingLabel, emptyLabel, rowTestId, testId, autoFocus — `frontend/src/components/ui/NavigableList.tsx`
- **PillButton** — props: accent, onClick, title, square, children — `frontend/src/components/ui/PillButton.tsx`
- **PollisLogo** — props: size, color — `frontend/src/components/ui/PollisLogo.tsx`
- **PresenceAvatar** — props: userId, avatarKey, size, alt, testId, variant — `frontend/src/components/ui/PresenceAvatar.tsx`
- **PresenceDot** — props: userId, testId — `frontend/src/components/ui/PresenceDot.tsx`. Bare online/offline dot for rows with no avatar to anchor it (terminal sidebar DMs, right-panel members).
- **RangeSlider** — props: label, value, onChange, min, max, step, disabled, className, id, sublabel, description — `frontend/src/components/ui/RangeSlider.tsx`
- **ScrambleText** — props: text, placeholderLength, typeSpeed, scrambleInterval, className — `frontend/src/components/ui/ScrambleText.tsx`
- **Switch** — props: label, checked, onChange, disabled, className, id, description — `frontend/src/components/ui/Switch.tsx`
- **TerminalMenu** — props: items, onEsc, className, autoFocus — `frontend/src/components/ui/TerminalMenu.tsx`
- **TextArea** — props: label, value, onChange, placeholder, description, error, disabled, rows, className, id, onKeyDown — `frontend/src/components/ui/TextArea.tsx`
- **TextInput** — props: label, value, onChange, placeholder, description, error, disabled, type, className, id, required, autoFocus, autoComplete — `frontend/src/components/ui/TextInput.tsx`

### `components/Voice` (8)

- **CameraPicker** — `frontend/src/components/Voice/CameraPicker.tsx`
- **RemoteUserVolumeSlider** — props: identity, participantName — `frontend/src/components/Voice/RemoteUserVolumeSlider.tsx`
- **RemoteVideoTile** — `frontend/src/components/Voice/RemoteVideoTile.tsx`
- **ScreenSharePicker** — `frontend/src/components/Voice/ScreenSharePicker.tsx`
- **ScreenShareViewer** — `frontend/src/components/Voice/ScreenShareViewer.tsx`
- **SidebarVoiceControls** — `frontend/src/components/Voice/SidebarVoiceControls.tsx`. Mic (four-state) + deafen for refined's sidebar voice strip; shares `micIndicatorOf` and `voiceControlLabels` with `VoiceBar` / `VoiceStage`.
- **VoiceBar** — props: channelId, channelName — `frontend/src/components/Voice/VoiceBar.tsx`
- **VoiceInputModeSelect** — props: value, onChange — `frontend/src/components/Voice/VoiceInputModeSelect.tsx`

### `components/Voice/stage` (2)

- **StageTile** — `frontend/src/components/Voice/stage/StageTile.tsx`
- **VoiceStage** — props: channelName, isInCall, onJoin, onLeave, onBack, onOpenSettings, observerParticipants, headerActions, footer, callMode — `frontend/src/components/Voice/stage/VoiceStage.tsx`

### `pages` (48)

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
- **GroupEmoji** — props: groupId — `frontend/src/pages/GroupEmoji.tsx`. Add/list/remove a group's custom emoji. Adding is image-first: an `EmojiDropZone` takes the file, the shortcode is auto-filled from the filename sanitised to `[a-z0-9_]`, and a field the user has already typed in is never clobbered — either order works. Preview is a blob URL of the source file (gifs animate), shown both at 4rem and at the 1.375rem it renders at inline; the size shown before upload is the SOURCE size, since the 48 KB re-encode runs in Rust during `upload_group_emoji`. Empty/invalid/duplicate shortcodes and unsupported files are validated inline and disable the add button.
- **GroupEmojiPage** — `frontend/src/pages/GroupEmojiPage.tsx`
- **GroupPage** — `frontend/src/pages/Group.tsx`
- **GroupsPage** — `frontend/src/pages/Groups.tsx`
- **InviteLinkLandingPage** — `frontend/src/pages/InviteLinkLandingPage.tsx`
- **InviteMember** — props: groupId, groupName — `frontend/src/pages/InviteMember.tsx`
- **InviteMemberPage** — `frontend/src/pages/InviteMemberPage.tsx`
- **InvitesPage** — `frontend/src/pages/InvitesPage.tsx`
- **JoinByInvite** — props: initialToken, autoRedeem — `frontend/src/pages/JoinByInvite.tsx`
- **JoinByInvitePage** — `frontend/src/pages/JoinByInvitePage.tsx`
- **JoinRequests** — props: groupId, groupName — `frontend/src/pages/JoinRequests.tsx`
- **JoinRequestsPage** — `frontend/src/pages/JoinRequestsPage.tsx`
- **KeyboardShortcutsPage** — `frontend/src/pages/KeyboardShortcutsPage.tsx`
- **KickMemberPage** — `frontend/src/pages/KickMemberPage.tsx`
- **LeaveGroupPage** — `frontend/src/pages/LeaveGroup.tsx`
- **Members** — props: groupId, isAdmin — `frontend/src/pages/Members.tsx`
- **MembersPage** — `frontend/src/pages/MembersPage.tsx`
- **PreferencesPage** — `frontend/src/pages/PreferencesPage.tsx`
- **RenameChannelPage** — `frontend/src/pages/RenameChannelPage.tsx`
- **RenameEntity** — props: kind — `frontend/src/pages/RenameEntity.tsx`. One form for both routes; `RenameGroup`/`RenameChannel` were eleven normalised lines apart and are gone (#874).
- **RenameGroupPage** — `frontend/src/pages/RenameGroupPage.tsx`
- **RequestsPage** — `frontend/src/pages/RequestsPage.tsx`
- **RootPage** — `frontend/src/pages/Root.tsx`
- **SavedPage** — `frontend/src/pages/SavedPage.tsx`
- **SearchGroupPage** — `frontend/src/pages/SearchGroupPage.tsx`
- **SearchPage** — `frontend/src/pages/Search.tsx`
- **SecurityPage** — `frontend/src/pages/SecurityPage.tsx`
- **SettingsHubPage** — `frontend/src/pages/SettingsHub.tsx`
- **SettingsPage** — `frontend/src/pages/SettingsPage.tsx`
- **StartDM** — props: onSuccess — `frontend/src/pages/StartDM.tsx`
- **StartDMPage** — `frontend/src/pages/StartDMPage.tsx`
- **UpdatePage** — `frontend/src/pages/UpdatePage.tsx`
- **UserProfilePage** — `frontend/src/pages/UserProfile.tsx`
- **VoiceChannelPage** — `frontend/src/pages/VoiceChannel.tsx`
- **VoiceSettingsPage** — `frontend/src/pages/VoiceSettingsPage.tsx`
<!-- END GENERATED: component inventory -->

---

_Back to [index.md](./index.md)_
