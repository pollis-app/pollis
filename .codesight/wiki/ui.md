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
| `--bar-h` / `--composer-h` / `--composer-flush-h` / `--side-w` | `h-bar`, `min-h-bar`, `min-h-composer` / `min-h-composer-flush`, `w-side` (the side panels' DEFAULT width only — see [Resizable side panels](#resizable-side-panels-985)). **The two composer utilities are not interchangeable** — see below. |
| `--radius-chip` / `--radius-control` | `rounded-chip`, `rounded-control` |
| `--msg-header-gap` / `-group-gap` / `-divider-gap` / `--msg-row-pad-y` | `pt-msg-header`, `pt-msg-group`, `mt-msg-divider`, `pb-msg-row` |
| `--lh` | `leading-msg` |

### Bottom bars align with the composer, and border-box is the trap

`--composer-h` is the height of the composer's **input row**. `ChatInput` draws
its top hairline on the root *above* that row, so the composer's real footprint
is `--composer-h` **+ 1px**.

The sidebar Close bar, the right-panel Close bar and refined's profile strip each
draw their own `border-t` on the **same element** that carries the height — and
under `box-sizing: border-box` that hairline is taken *out of* the height rather
than added to it. All three therefore rendered exactly 1px shorter than the
composer they sit beside, visible as a broken seam where the sidebar meets the
chat pane.

`--composer-flush-h` (`calc(var(--composer-h) + 1px)`) is that height with the
hairline added back:

| The bar… | Utility |
|---|---|
| draws its own `border-t` | `min-h-composer-flush` |
| has its border on an ancestor (only `ChatInput`) | `min-h-composer` |

Both class names look equally plausible at the call site, so the choice is
enforced rather than remembered: `frontend/tests/composer-bar-alignment.test.ts`
fails on a `min-h-composer` next to a `border-t`, on a `min-h-composer-flush`
without one, and on the token drifting from `--composer-h`.

**If a utility is missing, add it to the theme** — that is what the bottom seven rows
above are: tokens that had no utility, so every call site spelled them inline.

**Never put two utilities of the same property on one element** — `text-muted` and
`text-dim` in one class string, or two `focus:bg-*`. Tailwind emits its own
stylesheet order, so which one wins has nothing to do with the order you wrote them
in, and the loser is silently dropped. Pick the tone with a conditional and append
exactly one (`MessageActions` splits `barBtnBaseClass` from its resting color for
this reason), or hold the variant classes apart per call site.

Inline `style` is still correct for a value computed at runtime: a per-author
username colour, a percentage width, a **dragged panel width** ([Resizable side
panels](#resizable-side-panels-985)), a `transform`, a component-local custom property
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

**Chrome mirrors the left sidebar.** The panel's close affordance is a **footer**, not a header: a full-width `min-h-composer border-t border-line` button with the label on the left and a `<kbd>` on the right carrying the live `useShortcutLabel("app.toggleRightPanel")` combo (default `mod+shift+b`, the shifted twin of the sidebar's `mod+b`). AppShell binds the same command id, so the chip and the key can't drift. Width tracks `--side-w` and the aside carries `font-mono`, which is the entire skin switch — terminal renders DM Mono, refined re-points `.font-mono:not(.font-machine)` at the sans face. Terminal additionally borrows the sidebar's `SectionHeader` chrome (sticky `h-bar` strip, hairline under, a second hairline above every section after the first) and its one-text-line rows with a bare `PresenceDot`; refined keeps the airier avatar rows.

**Content is contextual, the column is not.** The panel no longer collapses on routes with no conversation (Preferences, Search, root). `MembersPanel` degrades there to the plain online roster — self plus everyone visible in `presenceStore.byUser`, named from the DM list — and drops the media grid, since "who is online" still answers a question off-conversation and "media shared in this conversation" does not. A stale `?thread=` on such a route falls back to the roster rather than rendering an empty thread.

### Resizable side panels (#985)

Both side columns — the left `Sidebar` and the right `RightPanel` — are dragged by a
`ResizeHandle` on their **inner** edge. One component serves both: same gesture, same
bounds, one place to fix a bug in either. The three behaviours are the ones Slack,
Discord and VS Code all ship, and they behave the same way here: **drag** to resize,
**drag past the minimum** to collapse, **double-click** to reset.

The handle straddles the panel's hairline border (`w-2`, half over each side — the VS
Code sash arrangement) so it is comfortably grabbable while the visible affordance
stays a **1px solid line** at the edge, painted `bg-line-strong` on hover and for the
duration of the drag. No glow, no wide hover band.

**Widths are device-local `localStorage`, `pollis.layout.sidebarWidthPx` and
`pollis.layout.rightPanelWidthPx`.** Not the synced user-preferences blob and not the
database — deliberately, and for two reasons:

- A width answers *how much of THIS screen* is worth spending on a channel list. A
  13" laptop and a 34" ultrawide want different numbers, and syncing forces one
  machine's answer onto the other. Same reasoning as the right panel's open/closed
  state, the device language (`i18n/storage.ts`) and the device font size.
- Nothing on the server has any use for it, and a width arriving over the network
  could move a panel the user is looking at.

Unlike the right panel's open/closed state these keys are **not scoped by user id**.
That state is seeded from a synced preference, so it already waits on a query and a
signed-in user; a width has to be on the element at **first paint** or the layout
visibly jumps from the default to the user's measure, and at first paint there is no
user id yet. `panelWidthStore` therefore reads both keys in its field initialisers, at
module eval.

**Default is still the token.** A panel with no stored width renders with `w-side`
(`--side-w`, a per-skin rem measure), so the default keeps tracking the skin and the
font-size preference. A stored width is an inline `style={{ width }}` — the sanctioned
runtime-computed exception. Never both on one element: that would be two sources for
one CSS property, which is exactly what the class-order rule forbids. Double-clicking
a handle stores `null`, which is what makes "reset" mean *the token* rather than a
hardcoded number.

**The bounds** live as pure arithmetic in `components/Layout/panelWidth.ts`, covered by
`frontend/tests/panel-width.test.ts`:

| Rule | Value |
| --- | --- |
| Minimum | **50px**. Landing exactly on it is a legitimate width. |
| Below the minimum | **Collapse**, not a clamp — `resolveDrag` returns a union, not a number. |
| Maximum, one panel open | Window width **− 50px** (`MIN_CONTENT_PX`, the centre column's floor). |
| Maximum, both open | Window width **− the other panel's measured width − 50px**. |
| Window too small for both | Each panel floors at 50px rather than collapsing something the user opened. |

The combined rule is deliberately "bounded by what the other panel *actually*
occupies" rather than a fixed half-the-window split, which would stop someone widening
one panel while the other is a stub. The other panel's width has to be **measured**,
not read from the store, because a panel on the token default has no pixel number
anywhere but the DOM; `usePanelWidth` reports `offsetWidth` back after every render and
0 on unmount.

**Collapse is the existing collapse.** Dragging shut calls the same writer the keyboard
uses — `onToggle` (`app.toggleSidebar`, `mod+b`) and `setPanel("none")`
(`app.toggleRightPanel`, `mod+shift+b`) — so there is one collapsed state, not a
second drag-only one. Collapsing deliberately writes **no width**, so reopening
restores the width it was dragged from and never a 0 or a stuck 50. The collapse lands
on pointer-up rather than mid-drag: the handle is a child of the panel it resizes, so
an unmount mid-gesture would strand the pointer capture. While below the minimum the
panel visibly pins at 50px instead.

**Clamping.** `panelWidthStore.clampToWindow` runs on `window`'s `resize` event (from
`AppShell`, event-driven — no polling) and from `usePanelWidth`'s effect, which covers
the other case that changes the arithmetic: the *other panel opening*, after this one
was dragged wide while it was shut. It clamps the sidebar first and bounds the right
panel with the result, so one pass cannot leave a pair that together overflow. It only
ever narrows, and a pass that changes nothing writes nothing, so it converges.

**Cost.** `pointermove` fires at 60–1000 Hz; a `setState` per move would re-render the
whole sidebar subtree (every group, channel, DM row and badge) to move one edge a few
pixels. So a move writes `panel.style.width` directly and React is not involved during
the drag at all — the store and `localStorage` are written **once**, on release, which
is the only moment a width is worth remembering. The imperative style is cleared before
that write so the committed render owns the width afterwards.

**RTL.** Which physical edge moves is measured at pointer-down (whichever edge the
handle is nearer to is the one that moves), so the same component works under
`dir="rtl"`, where the sidebar is drawn on the right and grows leftwards, without
knowing the reading direction.

### Composer completions: `@mention` and `:shortcode:`

`ChatInput` carries **two** autocomplete tracks and they are built to the same
shape, so learning one is learning the other:

| | mentions (#843) | emoji shortcodes |
| --- | --- | --- |
| trigger | `@` at a word start | `:` at a word start |
| parser | `utils/mentions.ts` → `mentionQueryAt` | `components/Emoji/emojiShortcodeQuery.ts` → `shortcodeQueryAt` |
| refined skin | `ui/MentionSuggestList` above the composer | `ui/EmojiSuggestList`, same anchoring |
| terminal skin | ghost node inside `ui/RichTextInput`, Tab accepts | same ghost node, Tab accepts |
| candidates | the conversation's roster | custom emoji the user may send, then the standard table |

**A trigger only opens at the start of the string or after whitespace.** That one
rule is what keeps `http://example.com` and `10:30` from ever reading as a
shortcode, and `a@b.com` from reading as a mention; both parsers share it and both
have negative tests for it (`frontend/tests/emoji-shortcode-query.test.ts`).

**Enter / Tab / Esc have exactly one owner.** `handleKeyDown` tests the mention
track first and the emoji track second, and an open mention query suppresses the
emoji one outright. The two cannot in fact both be open — neither body alphabet
contains the other's trigger character — but the precedence is written down so a
future widening of either alphabet has one place to be reasoned about.

**The alphabets are not the same.** A custom emoji shortcode is `[a-z0-9_]{2,32}`,
enforced by pollis-core and the DS. The standard aliases vendored from gemoji also
use `+` and `-` (`:+1:`, `:-1:`, `:e-mail:`), so the composer's body class is
`[A-Za-z0-9_+-]`. That widens what the *composer will look up*, never what a custom
shortcode may be — a query containing `+` simply cannot match a custom emoji.

**Custom emoji beat standard ones**, in the suggestion list and in direct
substitution alike: a group that uploaded its own `:tada:` means that one. Same
rule `emojiSearch.ts` already applies in the picker.

**Substitution happens in the composer, before send, so the wire format is
untouched.** Typing the closing `:` of a complete shortcode replaces the whole
`:name:` on the spot, Slack-style — a standard emoji becomes the literal Unicode
character, a custom one becomes the existing `<:shortcode:content_hash>` token.
There is no new grammar on the wire, nothing to parse in Rust, and no migration.
Accepting from the list or the ghost adds a trailing space (the word is finished);
typing the colon yourself does not (you are mid-word).

**The 1500-entry table stays out of the startup chunk.** `emojiData.ts` is the
largest module in the bundle and #874 code-split it behind the picker's lazy
import; the composer is always mounted, so `components/Emoji/emojiShortcodeIndex.ts`
is the *only* module in this path that imports it, and `useShortcodeEntries` pulls
it in with a dynamic import once the composer takes focus. `emojiShortcodeQuery.ts`
itself imports nothing but a type, which is also what makes it unit-testable under
`node --test`.

**Skin-tone variants (`:wave::skin-tone-3:`) are out of scope.** Completions use the
tone already stored by the picker; there is no tone syntax in the composer.

### The composer is a `contentEditable`, not a `<textarea>`

`ui/RichTextInput` replaced the composer's textarea so that a custom emoji renders
as an **inline image while you type** instead of showing seventy characters of
`<:shortcode:content_hash>`. Sent messages always rendered correctly
(`Emoji/EmojiText`); this was only ever an input problem.

**The value is still one plain string.** That is the load-bearing decision. The
DOM is a *projection* of the wire text and is read back into it after every edit
(`ui/richInputModel.ts`), so mentions, `:shortcode:` completion, drafts, the
`@all` hint, the history hand-off and the send path all still work on
`(text, caret)` exactly as they did — none of them learned a document model, and
no editor dependency was added (no Lexical, Slate or ProseMirror; the startup
chunk is unchanged).

**Serialization is the contract.** A text node contributes its characters; an
element contributes its `data-emoji-token` attribute, and only when that attribute
parses as a token; the ghost contributes nothing; a trailing `<br>` is an editing
host's filler and contributes nothing. No branch emits an element's markup, so a
`<`, `>` or `&` can reach the wire only by having been typed. Paste is
intercepted and reads `text/plain` only — the `text/html` flavour is never parsed.
`frontend/tests/rich-input-model.test.ts` pins the round trip, the near-token
(`<:oops:>` stays literal), the forged-attribute case and the paste rules.

**The children are built imperatively, not by React.** React reconciliation of a
`contentEditable`'s children destroys the caret and there is no way to opt out.
The element renders empty and the component owns everything inside it, which is
also why the DOM is rebuilt *only* when the value disagrees with what was last
serialized out of it — ordinary typing rebuilds nothing, so IME composition and
the caret survive.

**The inline ghost moved inside the editable.** The old `ui/InlineGhost` was an
absolutely-positioned invisible mirror of the textarea; it worked only because it
rendered identical text, and an emoji image of a different width breaks the
line-wrapping it depends on. The completion is now a `contenteditable=false` span
appended after the caret — which is exactly right, since the caller only offers a
ghost with the caret at end-of-value. Both tracks keep their own testids
(`mention-ghost`, `emoji-ghost`).

**A caret next to an emoji is decided against the model, never the engine.**
Backspace and forward-delete remove the token's whole byte range, because Chrome,
WebKit and Gecko each do something different beside a `contenteditable=false`
element. Arrow keys and drag-selection are left to the browser, which treats the
node as an atom on its own.

**Browser specs read `data-value`, not `.value`.** A `contentEditable` has no
`.value` for Playwright's `toHaveValue`, so the component mirrors the serialized
wire text onto that attribute on every change; `e2e/lib/harness.js` grows
`setComposerText` for the same reason on the WebDriver side.

## Components

> Generated from `frontend/src` by `scripts/ui-inventory.mjs` — the article said
> "regenerate rather than patch" for a long time before anything could (#933).
> Everything a line carries **after** its backticked path is a hand-written note
> and survives regeneration; everything before it does not, so prose belongs
> there or in a section above.

<!-- BEGIN GENERATED: component inventory (scripts/ui-inventory.mjs) -->
**151 `.tsx` files** under `frontend/src`, by directory. Regenerate with
`node scripts/ui-inventory.mjs`; `--check` fails if this is stale.

### `(root)` (3)

- **App** — `frontend/src/App.tsx`
- **main** — `frontend/src/main.tsx`
- **router** — `frontend/src/router.tsx`

### `components` (6)

- **ErrorBoundary** — `frontend/src/components/ErrorBoundary.tsx`
- **SearchPanel** — props: isOpen, onClose — `frontend/src/components/SearchPanel.tsx`. Selection moves by arrows, Tab/Shift-Tab, and the rebindable `search.next` / `search.prev` commands (defaults `mod+j` / `mod+k`, vim's direction). Those two register at priority 10 and only while the menu is open, which is what lets `mod+k` move the selection instead of re-firing `app.toggleSearch` and closing the menu the user just opened — Esc already closes it. All three routes call the same wrapping `selectNext` / `selectPrev`, and the hint row renders the live combos via `useShortcutLabel`, so a rebind shows up there.
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
- **EmojiPicker** — props: onSelect, onClose, closeOnSelect, className, maxHeight — `frontend/src/components/Emoji/EmojiPicker.tsx`
- **EmojiPickerButton** — props: onSelect, closeOnSelect, placement, align, className, ariaLabel — `frontend/src/components/Emoji/EmojiPickerButton.tsx`. The trigger plus its anchored, non-modal panel. **`placement` is a preference, not an instruction** (#1028): the panel is a fixed `h-[24rem]`, and a side fixed at the call site is wrong exactly when the trigger nears the edge it opens toward — a short window, a large font setting (the panel is sized in `rem`, so it grows while the viewport does not), or a layout that rides the composer high. It then rendered off the top of the content region, which `AppShell` clips with `overflow: hidden`, making the missing part unreachable rather than merely off-screen. `pickerPlacement.ts` resolves the side from the trigger's measured rect: the preference wins whenever the panel fits there, the other side is taken when it does not, and only when neither fits is the panel clamped (`maxHeight`) to the roomier side, with a tie keeping the preference so nothing flaps. Re-measured on `resize` and on `scroll` (capturing, since scroll does not bubble) while open.
- **EmojiSection** — props: id, title, items, toneIndex, baseIndex, onSelect, onPreview, scrollRoot — `frontend/src/components/Emoji/EmojiSection.tsx`
- **EmojiText** — props: text, renderText, jumbo — `frontend/src/components/Emoji/EmojiText.tsx`
- **SkinTonePicker** — props: toneIndex, onChange — `frontend/src/components/Emoji/SkinTonePicker.tsx`

### `components/Invites` (3)

- **CreatedInviteLinkCard** — props: link — `frontend/src/components/Invites/CreatedInviteLinkCard.tsx`
- **InviteLinkManager** — props: groupId — `frontend/src/components/Invites/InviteLinkManager.tsx`
- **InviteLinkRow** — props: link, onRevoke, isRevoking — `frontend/src/components/Invites/InviteLinkRow.tsx`

### `components/Layout` (10)

- **AppShell** — `frontend/src/components/Layout/AppShell.tsx`
- **BreadcrumbNav** — `frontend/src/components/Layout/BreadcrumbNav.tsx`. The bar under the TitleBar: a back/forward chevron pair, then the clickable trail, then search + settings. The chevrons walk **router history**, one entry per click, the way a browser (and Slack, and Discord) does — there is no hierarchical "up one level" navigation any more; the trail's own segments are what jump up the tree. **Both are always rendered**, root included: a control that vanishes at the root shifts everything beside it sideways on the way home. When a direction has nowhere to go it is greyed out and truly `disabled` (a `ui/Button` `variant="ghost"`), so assistive tech is told what the grey says. Enablement comes from `stores/historyNavStore`, which subscribes to `router.history` (event-driven, never polled) and keeps a cursor plus a ceiling — `canGoBack` is `index > 0`, `canGoForward` is `index < maxIndex`, and a PUSH resets the ceiling to the new entry because it discards whatever was ahead. That arithmetic is pure in `utils/historyNav.ts` and pinned by `frontend/tests/history-nav.test.ts`; it exists because no browser API answers "can I go forward?" — `history.length` counts a session, not a direction, and never shrinks on a truncating push. The `nav.back` shortcut (Escape, handled in `AppShell`) routes through the same `historyNavStore.goBack()`, so the key and the chevron cannot land in different places.
- **MainContent** — props: pendingDmRequest — `frontend/src/components/Layout/MainContent.tsx`
- **PageShell** — props: title, children, scrollable — `frontend/src/components/Layout/PageShell.tsx`
- **ResizeHandle** — props: panelRef, edge, label, maxPx, onCommit, onCollapse, onReset — `frontend/src/components/Layout/ResizeHandle.tsx`. Shared by both side panels; drags write `style.width` straight to the panel and never through React. See [Resizable side panels](#resizable-side-panels-985).
- **Sidebar** — props: isOpen, onToggle — `frontend/src/components/Layout/Sidebar.tsx`. Its settings rows carry `sidebar-row-*` testids like `SectionHeader`'s, so navigation tests never match on a translated label (#932).
- **SidebarProfilePanel** — `frontend/src/components/Layout/SidebarProfilePanel.tsx`. Refined only; identity row + the persistent voice strip (channel, mic, deafen, screenshare, disconnect) that replaces terminal's `VoiceBar`. The status/channel text is a button that navigates back to the connected room (same targets as `VoiceBar`'s channel pill), and the strip floors on `min-h-composer` so it lines up with the composer across the sidebar edge.
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

### `components/Message` (14)

- **AttachmentDisplay** — `frontend/src/components/Message/AttachmentDisplay.tsx`
- **LastMessagePreview** — props: message, isLoading — `frontend/src/components/Message/LastMessagePreview.tsx`
- **MediaLinkUnfurl** — props: text — `frontend/src/components/Message/MediaLinkUnfurl.tsx`
- **MentionToken** — props: name, isSelf, skin — `frontend/src/components/Message/MentionToken.tsx`
- **MessageActions** — props: messageId, variant, isOwn, canModerate, isSaved, copyLinkState, onReply, onOpenThread, onToggleSave, onCopyLink, onEdit, onDelete — `frontend/src/components/Message/MessageActions.tsx`. The per-message hover toolbar, shared by both skins: Reply, Edit (own messages), and a "more" trigger whose anchored menu (icon + label rows, Delete last) carries thread/save/copy-link/delete. Non-modal — `absolute` inside its own `relative` wrapper, same shape as `EmojiPickerButton`. Its Escape claim uses `stopImmediatePropagation` so closing the menu never also fires the window-level `nav.back` Escape shortcut. Keyboard selection **inverts** the focused control (`focus:bg-accent focus:text-bg`; menu rows too, with Delete inverting to `bg-danger` instead of borrowing the affirmative accent) — a recolor alone was invisible in the terminal skin, and it replaces the browser's default focus ring on the menu rows. The inversion is on plain `focus:`, not `focus-visible:`, because `MessageList` projects nav state with a programmatic `.focus()` that WebKitGTK does not reliably treat as focus-visible — the row's own `focus-within:bg-hover` is plain-focus for the same reason. The "more" trigger rests one step brighter (`text-dim`) than Reply/Edit: three dots carry less ink than the arrow or pencil, so at a shared `text-muted` it read as disabled.
- **MessageAvatar** — props: userId, username, size — `frontend/src/components/Message/MessageAvatar.tsx`
- **MessageBody** — props: text, ctx — `frontend/src/components/Message/MessageBody.tsx`. Renders resolving `@mentions` as tokens and delegates the rest to `LinkifiedText`. Plain `React.memo`, not `observer()` — it reads no observables, taking skin and the mention roster from `ctx`.
- **MessageItem** — props: message, ctx, replyToMessage, authorUsername, isAuthorAdmin, canModerate, isGroupStart, onReply, onOpenThread, threadReplyCount, onEdit, onDelete, onToggleSave, onCopyLink, copyLinkState, isSaved, onScrollToReply, receipt, peerCount, isDm — `frontend/src/components/Message/MessageItem.tsx`. Memoised via `observer()`. `ctx` is the list-wide `MessageRenderContext`; `replyToMessage` and `receipt` are resolved per row BY THE LIST, replacing an `allMessages.find()` (O(N^2)) and a whole-`Map` receipts prop that re-rendered every row whenever any one receipt landed.
- **MessageList** — props: messages, conversationId, groupIdForNames, adminUserIds, viewerIsAdmin, onReply, onOpenThread, threadReplyCounts, onEdit, onDelete, onScrollToMessage, getAuthorUsername, hasMore, isFetchingMore, onLoadMore, focusComposer — `frontend/src/components/Message/MessageList.tsx`. Passing `focusComposer` opts the list into arrow-key log navigation (bash-history style): ArrowUp from an empty/first-line composer walks the log, Left/Right walk the focused row's action bar, ArrowDown past the newest (or Tab/Escape) returns to the composer. The pure state machine lives in `utils/messageNav.ts` (unit-pinned by `frontend/tests/message-nav.test.ts`), the live state in `stores/messageNavStore.ts`, and rows style keyboard focus purely via CSS `focus-within` so keystrokes re-render nothing; browser-level coverage is `e2e/message-nav.spec.ts`. Windowed via `@tanstack/react-virtual` — see "The windowed log" above; `components/Message/messageWindow.ts` (not a `.tsx`, so not listed here) is the only door from a message id to a live row, and owns the rem-based row estimates and load-more threshold (#934).
- **MessageQueue** — `frontend/src/components/Message/MessageQueue.tsx`
- **MessageReactions** — props: messageId — `frontend/src/components/Message/MessageReactions.tsx`. Reaction pills plus the hover-revealed `+` trigger, rendered by **both** skins from `MessageItem` and gated on `!isDeleted`. Restored in #931 after #874 deleted it as a dead path: the Rust commands survived and mobile still called them, so the feature was live with no desktop UI. A pill toggles — clicking one you are already in removes your reaction, keyed on `(message, user, emoji)` so it never touches anyone else's. The emoji is run through `splitEmojiSegments` and a custom one renders via `CustomEmojiImage`, which is what lets a `<:name:hash>` reaction display as an image for a viewer who is not in the group that owns it (#848); reactions are stored as opaque strings, so a custom reaction simply *is* its wire token. The picker is `absolute` inside a `relative` wrapper — no portal, no backdrop — the same non-modal shape as `MessageActions` and `EmojiPickerButton`. The `+` trigger rests at `opacity-0 group-hover:opacity-60` and relies on the `group` class the message row already carries in both skins.
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

### `components/ui` (27)

- **AudioPlayer** — props: src, title, className, autoPlay, loop, preload — `frontend/src/components/ui/AudioPlayer.tsx`
- **Avatar** — props: avatarKey, size, alt, testId, variant, presence — `frontend/src/components/ui/Avatar.tsx`
- **Button** — props: children, onClick, disabled, isLoading, loadingText, className, variant, size, type, onKeyDown, autoFocus — `frontend/src/components/ui/Button.tsx`
- **Card** — props: children, className, style, padding — `frontend/src/components/ui/Card.tsx`
- **ChatInput** — props: onSend, placeholder, disabled, autoFocus, className, maxAttachments, onValueChange, draftKey, canNotifyAll, onHistoryUp — `frontend/src/components/ui/ChatInput.tsx`
- **Checkbox** — props: label, checked, onChange, disabled, className — `frontend/src/components/ui/Checkbox.tsx`
- **DotMatrix** — props: algorithm, dotSize, spacing, speed, className, style — `frontend/src/components/ui/DotMatrix.tsx`
- **EmojiSuggestList** — props: entries, activeIndex, query, onSelect, onHover — `frontend/src/components/ui/EmojiSuggestList.tsx`
- **EmptyState** — props: children, testId, messageTestId, tone, background, actions — `frontend/src/components/ui/EmptyState.tsx`. The centred "nothing here" line; hand-rolled a dozen times before it existed (#874). Not a loading state.
- **InlineAudioPlayer** — props: src, title, className, autoPlay, onClick — `frontend/src/components/ui/InlineAudioPlayer.tsx`
- **InputOtp** — props: length, value, onChange, disabled, autoFocus, mask — `frontend/src/components/ui/InputOtp.tsx`
- **LinkifiedText** — props: text — `frontend/src/components/ui/LinkifiedText.tsx`. URL detection and `ensureProtocol` live in `frontend/src/utils/links.ts`, shared with `MediaLinkUnfurl`; the `/g` regex is reset inside the single shared scanner, which is what makes it safe to share (#874).
- **LoadingSpinner** — props: size, className — `frontend/src/components/ui/LoaderSpinner.tsx`
- **MentionSuggestList** — props: candidates, activeIndex, query, onSelect, onHover — `frontend/src/components/ui/MentionSuggestList.tsx`
- **NavigableGrid** — props: items, getKey, renderCell, onActivate, minCellWidth, maxCellWidth, aspect, gap, autoFocus, emptyLabel, testId — `frontend/src/components/ui/NavigableGrid.tsx`
- **NavigableList** — props: items, getKey, renderRow, controls, trailing, onEnterRow, onClickRow, isLoading, loadingLabel, emptyLabel, rowTestId, testId, autoFocus — `frontend/src/components/ui/NavigableList.tsx`
- **PillButton** — props: accent, onClick, title, square, children — `frontend/src/components/ui/PillButton.tsx`
- **PollisLogo** — props: size, color — `frontend/src/components/ui/PollisLogo.tsx`
- **PresenceAvatar** — props: userId, avatarKey, size, alt, testId, variant — `frontend/src/components/ui/PresenceAvatar.tsx`
- **PresenceDot** — props: userId, testId — `frontend/src/components/ui/PresenceDot.tsx`. Bare online/offline dot for rows with no avatar to anchor it (terminal sidebar DMs, right-panel members).
- **RangeSlider** — props: label, value, onChange, min, max, step, disabled, className, id, sublabel, description — `frontend/src/components/ui/RangeSlider.tsx`
- **RichTextInput** — props: value, onChange, onSelectionChange, onKeyDown, onPaste, onFocus, onBlur, placeholder, disabled, autoFocus, className, style, ariaLabel, testId, ghost, ghostTestId, ghostClassName — `frontend/src/components/ui/RichTextInput.tsx`
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
