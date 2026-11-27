# Pollis design language

The one reference for how Pollis looks and reads, on every platform. Read it before building or
changing any UI on desktop (`frontend/`) or mobile (`mobile/`). If a screen needs something this
page does not cover, take the closest existing pattern from the app, not a new invention, and add
the rule here in the same PR.

## The three looks

Pollis ships one product with one look and two variants of it.

| Look | Where | What it is |
|---|---|---|
| **Modern** | Desktop, the default skin | The base. A conventional, calm chat app: Geist sans, soft corners, neutral-warm text on a near-black ground, the accent used sparingly. Reference points: Slack, Linear, Discord. |
| **Terminal** | Desktop, opt-in skin (Preferences → Appearance) | A variant of Modern for people who want an IRC/ncurses feel: DM Mono, text in the accent, tight corners, a denser log, keyboard-first menus. It re-skins and simplifies; it is chosen, never imposed. |
| **Mobile** | iOS and Android | Modern's palette and voice with native patterns: grouped inset lists, bottom sheets, tab bar, 44pt touch targets. |

Modern and Mobile share the same colours exactly. Terminal shares the same token names and the same
recipe inputs, and recolours them. Terminal may lack conveniences Modern has (a mention popup list,
thread links, read receipts) where they would break the minimal IRC feel; Modern never lacks a
feature Terminal has.

### What Pollis is not

Pollis is end-to-end encrypted, and the UI never performs that. No hacker theatre, in any look:

- No fake shell output, `// COMMENTS`, `BEGIN_…`, `>_` prompts, scanlines, matrix rain, glitch
  text, neon or glow. Solid borders and fills only.
- No security words as decoration. Everything is encrypted, so nothing is labelled
  `[encrypted]`, "secure channel", "E2EE ✓" or similar. Name a security fact only where a person
  must act on it (a changed safety number, an unverified device), in plain words.
- A message that shows no text says why, in muted words, the same on every look and platform:
  "Message deleted" (italic on desktop), "Couldn't decrypt" for a quote whose original is
  unreadable or not on this device, "Sent before you joined" for one that predates the reader.
  Never `[deleted]`, `[redacted]` or another bracketed tag.
- No emoji as icons or ornament. Emoji are user content and reactions only.
- Terminal idioms (brackets around words, `> prompt`, `↑↓ navigate`, UPPERCASE spaced labels, the
  braille spinner) belong to the Terminal skin only, behind a skin check or a terminal-only string.
  Modern and Mobile use plain sentences. How desktop keeps them there (#1965):
  - A bracketed string is a `…Terminal` sibling key (`list.blockedBodyTerminal: "[blocked]"`
    beside `list.blockedBody: "Blocked"`), picked by `useSkin()` at the call site, in every locale.
  - The braille `LoadingSpinner` draws a plain ring in Modern (`.loader-spinner` in `index.css`),
    so no call site checks the skin.
  - An `uppercase` label with a tracking utility, or `.section-label`, reads as written in Modern
    (`index.css` undoes the transform and spacing), so its string must be sentence case.
  - No `::` separators in titles; an em dash joins a page and its group ("Invite Member — Acme").
  - `e2e/terminal-idioms.spec.ts` holds all of this over a channel, a group page and two settings
    pages.

## Colour

**Source of truth: [`design/theme.ts`](../../design/theme.ts).** Every colour on every platform is
derived there from exactly two inputs, an accent and a background. Desktop paints the result onto
`<html>` as `--c-*` variables (`frontend/src/utils/colorUtils.ts`) and exposes them as Tailwind
utilities; mobile reads them through `semantic.*` (`mobile/theme/tokens.ts`). Never write a hex,
`rgb()`, `hsl()` or Tailwind palette colour (`red-500`) in UI code; never invent a token in one
platform only. Change the recipe in `design/theme.ts` and both platforms follow.

Defaults: accent **`#fabf5a`** (amber), background **`#0a0907`** (warm near-black). Presets, the
same list everywhere: accents Amber, Citron, Mint, Glass, Lilac, Rust; backgrounds Warm, Neutral,
Cool, Green, Purple. Both colours sync across devices as `accent_color` and `background_color`.

### Tokens

| Token | Desktop utility | Mobile (`semantic.`) | Modern / Mobile | Terminal | Use |
|---|---|---|---|---|---|
| `bg` | `bg-bg` | `bg` | `#0a0907` | `#0a0907` | The ground of every screen. |
| `surface` | `bg-surface` | `panel` | `#1c1811` | `#13110d` | Sidebar, tab bar, cards on `bg`. |
| `surfaceRaised` | `bg-surface-raised` | `raised` | `#262118` | `#1c1914` | Inputs, menus, sheets, grouped lists. |
| `surfaceHigh` | `bg-surface-high` | `high` | `#332c20` | `#25211a` | Popovers, avatars, controls on a raised card. |
| `text` | `text-fg` | `text` | `#f4e7d1` | accent | Body text. |
| `textDim` | `text-dim` | `dim` | `#c8b698` | `#c8b89d` | Secondary text: previews, read rows. |
| `textMuted` | `text-muted` | `muted` | `#b5a07d` | `#a99b84` | Tertiary: timestamps, hints, placeholders. |
| `border` | `border-line` | `hair` | accent 16% | accent 22% | Decorative separators only. |
| `borderSoft` | `border-line-soft` | `hairSoft` | accent 10% | accent 12% | Quiet separators inside a group. |
| `borderStrong` | `border-line-strong` | `edge` | `#a78956` | accent 66% | The edge of every control (input, outlined button, checkbox). |
| `accent` | `bg-accent`, `text-accent` | `accent` | `#fabf5a` | `#fabf5a` | The primary action, the selected marker, unread badges, focus. |
| `onAccent` | `text-on-accent` | `onAccent` | `#0a0907` | `#0a0907` | Ink on an accent fill. Never white. |
| `accentFaint` / `Soft` / `Mid` | `bg-accent-faint` … | `accentFaint` … | accent over bg at 10 / 16 / 22% | same | Tinted fills (selected chips, pressed rows). |
| `danger` | `bg-danger`, `text-danger` | `danger` | `#ff6b6b` | `#ff6b6b` | Destructive actions and errors. The one red. |
| `onDanger` | `text-on-danger` | `onDanger` | `#0a0907` | `#0a0907` | Ink on a danger fill. |
| `warning` | `text-warning` | `warning` | `#f0c674` | `#f0c674` | Degraded connection, a changed key, a key that failed its audit. |
| `success` | `text-connected` | — | `#69d39e` | `#69d39e` | "In a voice room" only. |
| `hover` / `active` | `bg-hover` / `bg-active` | — | white 5% / accent 13% | accent 8% / 14% | Hovered row / the selected row. |

Values are the defaults; every one moves with the user's accent and background, and on a light
background the ramp flips toward black.

### Rules

- **Contrast is tested, not hoped for.** Every text token and `danger` reaches 4.5:1 on `bg`,
  `surface`, `surfaceRaised` and `surfaceHigh`; `borderStrong` reaches 3:1; `onAccent` and
  `onDanger` reach 4.5:1 on their fills. This holds for every skin, every accent preset and every
  background preset (`frontend/tests/theme-contrast.test.ts`, `mobile/tests/theme-contrast.test.ts`).
  A recipe change that breaks it does not merge.
- **The accent is rare.** In Modern and Mobile, it marks the one primary action on a screen, the
  current selection, unread counts and focus. A selection is the `active` tint plus a marker (the
  radio dot, the sidebar's edge), never a solid accent fill, in every look: not a selected radio
  row, not a focused composer, not the status bar (#1965). It is not a text colour for names, headings or
  links in running text. (Terminal is the exception: its text is the accent by design.)
- **One red.** Every destructive action uses `danger`: remove, kick, revoke, reject, decline,
  block, leave, delete, discard, wipe. Desktop `<Button variant="danger">`; mobile
  `<Button variant="danger">`, `<Chip variant="danger">`, or a `ListRow` whose glyph and name are
  `semantic.danger`. A destructive action never wears the accent, so it can never look like the
  affirmative choice beside it. The only other red in the app is the Windows title-bar close
  hover, which copies the operating system.
- **Controls have a visible edge.** Inputs, outlined buttons, checkboxes and OTP cells use
  `borderStrong` (`border-line-strong`); a focused input uses `accent`. `border` / `line` is for
  separators that carry no meaning.
- **Step surfaces in order.** `bg` → `surface` → `surfaceRaised` → `surfaceHigh`. Don't skip a
  step for contrast; use a border.

## Type

| | Desktop Modern | Desktop Terminal | Mobile |
|---|---|---|---|
| UI face | Geist Sans | DM Mono (UI) + Atkinson Hyperlegible Next | Geist |
| Machine text (ids, keys, codes) | DM Mono via `font-machine` | DM Mono | `fonts.mono400` |

The scale is named by role, the same names on both platforms; each keeps its own sizes (mobile
body 16 against desktop 15 is platform-normal). Desktop sizes are rem at a 15px root, shown in px.

| Role | Desktop (`text-*`) | Mobile (`typeSize.*`, `ty.*`) | Used for |
|---|---|---|---|
| Meta | `text-meta` 11 | `meta` 12 | timestamps, counts, hints |
| Section | — | `section` 13 | section titles, small labels |
| Secondary | `text-secondary` 13 | `secondary` 14 | previews, subtitles, chips |
| Button | — | `button` 15 (`ty.button`) | inline button labels, dense rows |
| Body | `text-body` 15 | `body` 16 | running text, fields |
| Row title | `text-row` 15 (taller line) | `rowTitle` 16 (`ty.rowTitle`, semibold) | a list row's name; a full-width button's label |
| Heading | `text-heading` 16.9 | `heading` 17 | page and card headings |
| Title | `text-title` 18.75 | `title` 20 | page titles |
| Display | `text-display` 22.5 | `display` 28 | large titles, PIN and code digits |

- Desktop's fontSize theme holds only these roles (plus `glyph` / `glyph-lg` for emoji and the
  spinner, `code` for the enrollment code), so a Tailwind t-shirt size (`text-xs`) draws nothing;
  `frontend/tests/type-scale.test.ts` fails on one.
- A mobile component's `fontSize` reads `typeSize` or spreads a `ty.*` step, never a number.
  `mobile/tests/type-scale.test.ts` lists the deliberate exceptions: emoji and initials sized to
  their box, the enrollment code, and the 11pt count badges.
- Desktop sizes are `rem` so the font-size preference scales them. Never a `px` font size.
- Sentence case everywhere in Modern and Mobile: titles, buttons, section headers. Terminal may use
  its uppercase spaced labels.
- Mono is for machine text, never for decoration.
- Every weight in use is a loaded face (`frontend/src/index.css`): Geist and Atkinson 400–700,
  DM Mono 500. A synthesized bold is a missing import.

## Space, radius, size

- Desktop spacing is Tailwind's 0.25rem step; mobile uses `space.*`. Related items `0.5rem` / `8`,
  gutters `1rem` / `16`, sections `1.5–2rem` / `24–32`.
- Corners: Modern `rounded-chip` 6px, `rounded-control` 8px, panels 10px; Terminal 2 / 4 / 10;
  Mobile `r.box` (12) for fields, full-width buttons, cards, grouped lists and list rows, so a
  search bar and the card under it match (#2101); a box under a search field also takes the
  field's fill, `raised`; pills for inline buttons. An input and a button
  on the same form share a radius.
- Every desktop corner is one of those tokens, picked by role (#1967): `rounded-chip` for
  chip-sized things (a key chip, a preset, a badge, a small thumbnail), `rounded-control` for
  controls (buttons, inputs, menus, icon buttons), `rounded-panel` for cards, panels and media
  frames, `rounded-full` for a circle or a pill. No inline `borderRadius` and no stock
  `rounded`, `rounded-sm/md/lg` or `rounded-[…]`; a corner the token set cannot draw (an avatar
  circle, a radius the OS draws for the window) says why in a comment above it.
- Desktop chrome bars use `h-bar`; mobile tap targets are at least 44pt.
- Desktop `Button` has three sizes, all on the type scale so they follow the font-size
  preference: `xs` (a row's inline action) and `sm` (a compact button beside content) in
  `text-meta`, `md` (a form's own action, the default) in `text-secondary`; padding tells them
  apart. A call site never sets a button's text size, letter-spacing or casing. Labels are
  sentence case ("Approve", not "approve") in every look. `secondary` is a neutral outline; the
  accent belongs to `primary`.
- **No hand-rolled `<button>` outside `components/ui/`** (#1967). A text action is a `Button`; an
  icon-only one is an `IconButton` (`aria-label` required, `sm` for a header or row action, `md`
  for window chrome); a link-styled action in a sentence or under a status is a `LinkButton`; a
  preset, a remembered account or a key combo to rebind is a `Chip` (`selected` is the accent edge
  on the `active` tint); a shortcut beside an action is a `Kbd`. A whole row, a grid cell or a
  tile whose look is its own is a `PressButton` (`activateOn="click"` when it sends, saves,
  toggles or destroys), so press activation stays one implementation.
- **Focus** is one ring on every control, desktop: 2px, offset by `bg`, keyboard focus only
  (`ui/focusRing.ts`: `FOCUS_RING`; `FOCUS_RING_DANGER` on a destructive control, which never
  wears the accent; `FOCUS_WITHIN_RING` for a field drawn around a native input). No other
  ring width, no black offset band.

## Layout and behaviour

- **Desktop:** title bar, breadcrumb bar, sidebar, main column, optional right panel, status bar.
  Settings-type pages are a single column, at most 420px wide, inside the main pane. Every page's
  title is `PageShell`'s `<h1>` (#1966): a sentence-case heading in `text-fg` in Modern, Terminal's
  small accent strip; a page draws no second title of its own. Every route has a full breadcrumb
  (`utils/breadcrumbTrail.ts`, walked for every route by `tests/breadcrumb-trail.test.ts`), and the
  breadcrumb, page title and status bars share `h-bar`.
- **Sign-in looks like the app.** The device remembers the last account's skin, accent and
  background (`colorUtils.rememberBootLook`), so sign-in, the code step and the PIN paint in them
  rather than the defaults; signing out with "delete data" forgets it. Mobile does the same with
  its boot accent (#1447).
- **Mobile:** tabs plus stacks, native sheets for menus and confirmations, master–detail on tablets.
  A confirmation is a `SheetOverlay` (the question as its title, the action as a row in
  `Group surface="high"`, red when destructive; Close cancels), never a stock `Alert`, whose
  platform dialog ignores the theme.
- **No modals on desktop.** The Cmd+K palette is the only overlay. A confirmation replaces the
  chat input bar or sits in the page column (see "Confirmations and forms").
- **Confirmations and forms (desktop, #1968).** One pattern each.
  - **The form pattern.** A form, settings or confirmation page is `PageShell` (the title) over one
    `PageColumn`: a single column at most 420px, top-aligned and centred in the main pane, with no
    card, border or fill around it. Fields stack in it (`spacing="fields"`), a settings page's
    sections are spaced wider (`spacing="sections"`). A step that needs a heading under the page
    title uses `FormLead` (a question or step name and one sentence), never a second page title.
    Errors sit under the fields in `danger`. The actions are one `FormActions` row under them:
    left-aligned, every button at its natural width, the action first (`primary`, or `danger` when
    it destroys) and Cancel last as a `ghost` button. No full-width submit, no button stack.
  - **The confirmation pattern.** A question that names its object ("Leave Studio?"), the
    consequence in one plain sentence ("You will need a new invite to rejoin."), then the
    destructive action (`danger`, and on a confirmation page the only one) and Cancel. Cancel,
    never the action, is the default focus; Escape goes back as on every page. On a page this is a `ConfirmBlock` in the
    `PageColumn`: as its own page (`ConfirmPage`: Leave group, Remove member), as a section of a
    settings page (Conversation settings' Leave), or in place of the control it confirms (the
    Security page's Revoke device and Revoke now). Inside a conversation it is a `ConfirmBar`, which
    replaces the chat input (delete a message, delete a channel, delete in the Vault): there the
    user has just picked the action on that very message, so the action holds focus and Enter
    confirms, while Escape or the bar's X cancels.
  - Kept apart, on purpose: sign-out and "wipe this device" confirm on the sign-in card, because
    they run outside the app shell where there is no main pane; Custom emoji keeps a wider column
    for its grid; the Settings hub and a user's profile keep their profile header layout.
  - Mobile keeps its native patterns (a `SheetOverlay`, a tap-again row or button, grouped lists);
    the copy is shared, and the destructive row or button is the only red one.
- **Arranging a list (#2031).** Desktop arranges by dragging the rows themselves, for the people
  allowed to (everyone else gets no drag wiring at all), with the drop slot drawn as a thin accent
  line across the row, never an edge stripe, and Alt+↑/↓ for the keyboard; a rename edits the row
  in place (`InlineInput`) and a delete replaces the row with an inline confirmation. Mobile uses
  an Edit mode (the iOS Mail / Discord pattern): Edit in the header, a ≡ handle on every row so
  dragging is deliberate, the row's actions shown only while editing, Done to leave; screen readers
  get Move up / Move down on the handle.
- **Same-frame feedback.** Anything that can take time shows an optimistic result or a busy,
  un-repeatable control on the same frame, on every platform.
- **Name what an action acts on.** A confirmation names its object ("Join Design team?", not
  "Join this group?") whenever the app can know it; a scoped search shows its scope as a
  removable chip ("In Design team"); a count over a filtered list counts what is shown
  ("Packages shown: 4 of 312"). A screen opened with nothing to act on says so in one plain
  sentence, never a sentence with a blank where the object should be.
- **People in rows** (members, message requests, conversation lists): the display name when the
  person set one, else the handle. A name stays on one line and never wraps mid-word: mobile
  truncates a handle in the middle (`ListRow`'s `nameIsHandle`, so both ends stay readable) and a
  display name at the end; desktop truncates at the end. Row actions (Accept / Block, a role,
  Remove) never squeeze the name: on a narrow row they go on their own line under it, past the
  avatar (mobile `stackRowActions`, desktop's row controls keep their width and the name shrinks).
- **Bots** (#1182): a bot account always wears the **Bot badge** beside its name, wherever a
  name stands for it: the member list and right panel, its messages' header, an `@mention` of
  it and the mention picker, its profile. One quiet tag, the chip radius, `surfaceHigh` fill and
  `textDim` ink, never the accent and never colour alone (the word "Bot"; Terminal `[bot]`).
  It says who answers for the bot: "Managed by the admins of <group>", as the badge's tooltip and
  in words on the bot's profile, never in a modal. Desktop `BotBadge` (`isBotUser` decides), mobile
  its twin. Bot management copy says plainly that the bot runs on a machine an admin controls and
  can read what it receives.
- **Presence** (#132, #2059): a person's avatar carries a small corner dot, a quarter of the
  avatar: online is the accent ringed in the surface behind it, away (#2110) the same dot with
  a bite taken out of its top corner in the ring's colour (a crescent moon, same box), offline a
  hollow dot (`bg` ringed in `accentMuted`). Solid, never a glow. Online means a room this device
  shares shows them in use, away that it shows them with the app running but not in use; you are
  always online on your own device. Groups and channels get no dot. Desktop
  `PresenceAvatar` / `PresenceDot`; mobile draws it on every `UserAvatar`.
- **Empty states:** every list that can be empty uses `EmptyState`, on desktop
  (`frontend/src/components/ui/EmptyState.tsx`, also what `NavigableList` / `NavigableGrid` draw
  for `emptyLabel`) and on mobile (`EmptyState` in `mobile/components/ui.tsx`). One plain sentence,
  centred, in `textMuted`, and the action that fills the space under it when there is one (Create /
  Find on an empty Groups tab, New message on an empty Direct tab, Join on an empty voice room).
  Mobile may add an icon above the sentence, only where the empty list is the whole screen; never
  in a section of a longer page. Not for loading or errors. Where desktop and mobile show the same
  list, they use the same copy key.
- **Message rows** (desktop `MessageItem`, mobile `MessageRow`): the first message of a run has one
  header line, the sender's name in `text` then the time in `textMuted`, never wrapping (a long
  name truncates); a quoted reply sits above it and changes nothing in it. Pictures and videos come
  before files: one keeps its own shape, several sit in equal squares (desktop's wrapping thumbs,
  mobile's 2-up grid, `MessageAttachments`); files and audio follow as a list. A failed send's
  reason and Retry / Discard go under the whole message, after its attachments. In every look and
  client, attachments start on their own line below the text, with or without text (Slack,
  Discord).
- **Terminal message line** (desktop Terminal, `pollis-tui`): a shell line, not two columns (#2130).
  The time sits in its own gutter; the name, `:` and the text flow as one line, and wrapped text
  continues under the name, never in a hanging block after it. Pictures, files and reactions start
  at the name on the lines below. A quoted reply's arrow starts where the names start (#2146).
- **Name colour** (#2146): a person's name has one colour wherever the chat shows it, your own
  included. On desktop it is the per-user colour (`getUsernameColor`, keyed on the handle), in
  the message line, the admin chip's fill, reply quotes (on the row and above the composer), and
  the thread and pinned views, in both looks. There is no accent for self (#1488's rule is
  gone). Quotes are muted by colour, never by opacity, so the name in them keeps its colour. A
  mention of you keeps its accent highlight, which marks the mention rather than the name. Mobile
  draws no per-user colours: every name is `text` (`textDim` in a quote) for everyone, you
  included.
- **Message hover actions** (desktop, both looks): Slack/Discord style (#2144). Reply, react, edit
  and more float on a `surface-raised` panel at the row's top-end corner, over its top edge, with
  their bottom on the row's first line. They never cover the text at any column width or in either
  direction, so the whole line stays selectable while they show. Keyboard focus in the row shows
  them too.
- **Read receipts** (#2140): DMs only, Modern and Mobile. Terminal has none. **Mobile and desktop
  differ on purpose**: each uses the better pattern for its platform. A design audit keeps them
  different and does not unify them.
  - **Mobile:** the other person's small avatar sits on a narrow rail along the list's right edge,
    level with the last message of yours they have read. It is one element, never a marker per
    row. When they read further it glides down to the newer message on `MOTION` timing, and it
    scrolls with the list. Nothing shows until they have read something, and delivery is never
    shown. With reduced motion it moves without the glide. One-to-one DMs only; a DM with more
    members keeps the count indicator (`ReceiptIndicator`) until a later ticket.
  - **Desktop Modern:** one line in words under your newest message that a peer has received:
    "Delivered", then "Read 11:03 AM". When it moves to a newer message, the old line collapses as
    the new one opens, over the same duration, so the log's height stays the same. A change of
    state cross-fades in place at a fixed height. It follows the new-message rules (#2125): it
    never scrolls or moves someone who has scrolled up.
  - With receipts off (#892), mobile never shows the avatar, and desktop stops at "Delivered",
    exactly as for someone who hasn't opened the chat yet.

## Motion

Motion is small and quiet, and it shows what changed; it never decorates (#2125).

- **Tokens.** `MOTION` in [`design/theme.ts`](../../design/theme.ts), shared like the colours:
  `fastMs` 120 (popovers, a fade under reduced motion), `baseMs` 180 (arrivals, folds, the sheet
  entrance), `slowMs` 240 (the longest anything runs: the tap bump), `exitMs` 140 (a closing
  sheet), and one ease-out curve, `cubic-bezier(0.22, 1, 0.36, 1)` (its mirror `easeIn` plays an
  entrance backwards). Distances and scales are tokens too: a new message rises 10px, a popover
  4px; a new pill or badge grows from 0.7 to a 1.08 peak. Desktop paints them as `--motion-*`
  variables (`frontend/src/utils/motion.ts`) and every keyframe lives in
  `frontend/src/components/ui/motion.css`. Mobile imports the same `MOTION` through
  `mobile/theme/motion.ts`, which only turns it into Reanimated builders (`EASE_OUT`,
  `messageRiseIn`, `POP_IN`, `ROLL_IN`, the fold and fade presets), all running on the UI
  thread. Never type a duration or a curve in a component.
- **Limits.** Nothing runs longer than 240 ms; nothing overshoots its resting value by more than
  8 %; no springs that wobble, no glow, no motion that loops (the status bar's unread blink is
  the one loop, and it stops under reduced motion). `frontend/tests/motion-tokens.test.ts` holds
  these for desktop; on mobile `tests/sheet-entrance.test.ts` and `tests/reaction-motion.test.ts`
  check the sheet and pill timings against the same ceiling.
- **What moves.**
  - A message that arrives while the reader is at the bottom rises and fades in, and the log
    follows it. Scrolled up, nothing moves: a "N new messages" button appears at the bottom of
    the log, and a click, or reaching the bottom by hand, clears it. The first load, older
    pages, a switch of conversation and a sending message turning into a sent one never move.
    Sending while scrolled up takes you to your message, as in Slack. Typing never moves the
    log (#2082).
  - A new reaction pill pops in, a changed count rolls its digit up, a tapped pill bumps.
    Pills that were there when the row painted hold still.
  - Desktop popovers (the emoji picker, the message menu, the reaction breakdown, the suggest
    list) fade in and rise 4px from the control that opened them; an emoji under the pointer
    in the picker grows slightly.
  - Mobile sheets close with their entrance played backwards, in 140 ms, when the sheet closes
    itself (Close, the backdrop, Android Back). `onClose` runs from a timer at 140 ms, never from
    the animation, so a stalled animation can never hold a sheet open, and it fires once. A row
    whose action closes the sheet and goes somewhere else still closes it at once, so the next
    step is not delayed.
  - Desktop: the sidebar's group chevron turns as its channels fold open and shut. Mobile: a
    category in a group's channel list does the same (its rows fade, the rows under it slide).
    An unread badge that turns up pops in: desktop's sidebar, mobile's tab bar, DM, channel and
    category rows. A badge that was there when the counts first loaded does not.
  - Content that replaces a loading state fades in where the loader was: the chat log on both,
    desktop's lists and empty states, mobile's Groups and DMs tabs.
- **Reduced motion.** One source per platform. Desktop: `useReducedMotion()`
  (`frontend/src/hooks/useReducedMotion.ts`) and `scrollBehavior()` for code, and the one
  `@media (prefers-reduced-motion: reduce)` block at the end of `motion.css` for CSS. Mobile:
  `useReduceMotion()` (`mobile/hooks/useReduceMotion.ts`), live as the setting changes; the
  presets in `theme/motion.ts` opt out of Reanimated's own handling, so each caller picks its
  reduced form from the hook. Under it every movement becomes an opacity fade at `fastMs` or
  nothing at all: arrivals, pills and popovers fade, bumps, folds, turns, the blink and the
  scrambled preview text stop, and smooth scrolls jump. On mobile, sheets open and close at
  once, swipe-to-reply follows the finger then snaps home with no nudge, the media viewer pages
  and dismisses without sliding, and a stack push cross-fades instead of sliding.

## Components: reuse first

Use the existing primitives. Do not hand-roll a styled `<button>`, input, toggle or list row; if a
primitive cannot do what a screen needs, extend it in its own file and use it everywhere.

- **Desktop** (`frontend/src/components/ui/`): `Button` (`primary`, `secondary`, `ghost`,
  `danger`), `IconButton`, `LinkButton`, `Chip`, `Kbd`, `PanelCloseRow`, `TextInput`, `TextArea`,
  `InlineInput` (a field edited in place in a list row), `Checkbox`, `Switch`, `RadioList`, `RangeSlider`, `InputOtp`, `PageColumn`, `FormLead`,
  `FormActions`, `ConfirmBlock` (and `Layout/ConfirmPage`), `ConfirmBar`, `ComposerBarHeader`,
  `Select` (a pick-one dropdown with its label), `SectionHeader` (a panel section's title),
  `Card`, `EmptyState`, `Avatar`, `PresenceAvatar`, `PresenceDot`, `PillButton`, `PressButton`,
  `BotBadge`, `SasCodeInput` (a code typed off another screen; never pre-filled),
  `NavigableList`, `NavigableGrid`, `TerminalMenu` (Terminal hub pages), `LoaderSpinner`,
  `IconSpinner`, `ChatInput`, `EditMessageBar`, `SuggestList`, `Fold` (a list that folds open and shut).
- **Mobile** (`mobile/components/ui.tsx`): `Screen`, `Header`, `Body`, `BottomAction`, `Txt`,
  `SectionTitle`, `Group`, `ListRow`, `ActionRow`, `CheckRow`, `Field`, `Toggle`, `EmptyState`, `Button`
  (`primary`, `secondary`, `subtle`, `danger`, `default`), `Chip` (`default`, `solid`, `subtle`,
  `outline`, `danger`), `IconButton`, `Badge`, `Avatar` (the letter tile; groups only), `Card`,
  `Divider`, `Dot`; every user avatar is `components/UserAvatar.tsx`.

## Copy

- Plain, short and technically honest. "Messages stay encrypted, so we never see what was said."
  "Only changes this device." "You will need a new invite to rejoin."
- Name security facts exactly. The local database is encrypted at rest: never call its contents
  "plaintext"; say "decrypted (inside the encrypted local DB)".
- Every user-visible string is in the shared catalogues (`frontend/src/i18n/locales/`, used by
  desktop and mobile), never inline.
