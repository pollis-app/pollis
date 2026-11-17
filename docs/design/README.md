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
  Modern and Mobile use plain sentences.

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
| `warning` | `text-warning` | — | `#f0c674` | `#f0c674` | Degraded connection, a changed key. |
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
  current selection, unread counts and focus. It is not a text colour for names, headings or
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

## Space, radius, size

- Desktop spacing is Tailwind's 0.25rem step; mobile uses `space.*`. Related items `0.5rem` / `8`,
  gutters `1rem` / `16`, sections `1.5–2rem` / `24–32`.
- Corners: Modern `rounded-chip` 6px, `rounded-control` 8px, panels 10px; Terminal 2 / 4 / 10;
  Mobile 12 for fields and full-width buttons, pills for inline buttons. An input and a button on
  the same form share a radius.
- Desktop chrome bars use `h-bar`; mobile tap targets are at least 44pt.

## Layout and behaviour

- **Desktop:** title bar, breadcrumb bar, sidebar, main column, optional right panel, status bar.
  Settings-type pages are a single column, at most 420px wide, inside the main pane.
- **Mobile:** tabs plus stacks, native sheets for menus and confirmations, master–detail on tablets.
  A confirmation is a `SheetOverlay` (the question as its title, the action as a row in
  `Group surface="high"`, red when destructive; Close cancels), never a stock `Alert`, whose
  platform dialog ignores the theme.
- **No modals on desktop.** The Cmd+K palette is the only overlay. A confirmation replaces the
  chat input bar or is its own page.
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
  reason and Retry / Discard go under the whole message, after its attachments.

## Components: reuse first

Use the existing primitives. Do not hand-roll a styled `<button>`, input, toggle or list row; if a
primitive cannot do what a screen needs, extend it in its own file and use it everywhere.

- **Desktop** (`frontend/src/components/ui/`): `Button` (`primary`, `secondary`, `ghost`,
  `danger`), `TextInput`, `TextArea`, `Checkbox`, `Switch`, `RadioList`, `RangeSlider`, `InputOtp`,
  `Card`, `EmptyState`, `Avatar`, `PresenceAvatar`, `PresenceDot`, `PillButton`, `PressButton`,
  `NavigableList`, `NavigableGrid`, `TerminalMenu` (Terminal hub pages), `LoaderSpinner`,
  `IconSpinner`, `ChatInput`, `EditMessageBar`, `SuggestList`.
- **Mobile** (`mobile/components/ui.tsx`): `Screen`, `Header`, `Body`, `BottomAction`, `Txt`,
  `SectionTitle`, `Group`, `ListRow`, `ActionRow`, `CheckRow`, `Field`, `Toggle`, `EmptyState`, `Button`
  (`primary`, `secondary`, `subtle`, `danger`, `default`), `Chip` (`default`, `solid`, `subtle`,
  `outline`, `danger`), `IconButton`, `Badge`, `Avatar`, `Card`, `Divider`, `Dot`.

## Copy

- Plain, short and technically honest. "Messages stay encrypted, so we never see what was said."
  "Only changes this device." "You will need a new invite to rejoin."
- Name security facts exactly. The local database is encrypted at rest: never call its contents
  "plaintext"; say "decrypted (inside the encrypted local DB)".
- Every user-visible string is in the shared catalogues (`frontend/src/i18n/locales/`, used by
  desktop and mobile), never inline.
