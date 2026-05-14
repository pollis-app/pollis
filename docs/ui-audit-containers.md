# UI Audit: Containers & Small-Display Components

## Summary

Strong canonical primitives exist (`Card`, `Avatar`, `PillButton`) but scattered reimplementations of badges, status indicators, and list rows. Three concrete consolidation opportunities: unread badges built 3+ different ways, status/strength indicators duplicated across Settings, and list-row hover/active states hand-coded in every component.

---

## Canonical Primitives & Utilities

### UI Components (`frontend/src/components/ui/`)

| Component | Purpose | Notes |
|---|---|---|
| **Card** (`Card.tsx:18`) | Generic bordered surface with padding variants | Inline: `background: var(--c-surface)`, `border: 2px solid var(--c-border)`, `border-radius: 6px` |
| **Avatar** (`Avatar.tsx:23`) | User circles with optional presence dot; "list" & "profile" variants | `border-radius: 50%` (list) or `0.5rem` (profile); presence dot bottom-right |
| **PresenceAvatar** (`PresenceAvatar.tsx:20`) | Wraps Avatar to inject live presence | — |
| **PillButton** (`PillButton.tsx:21`) | Filled accent pill with invert-on-hover | `border-radius: 3px`, custom `accent` prop |
| **Button** (`Button.tsx`) | Primary/ghost variants | Uses `.btn-primary` / `.btn-ghost` CSS |

### CSS Utilities (`frontend/src/index.css`)

| Class | Intent |
|---|---|
| `.panel` (line 126) | Light bordered surface (`--c-surface`, 2px border, rounded) |
| `.panel-raised` (line 132) | Elevated surface (`--c-surface-raised`) |
| `.section-label` (line 139) | Section headers — uppercase mono muted |
| `.sidebar-item` (line 145) | Clickable list row — flex, padding, hover transitions |
| `.sidebar-item-active` (line 155) | Active sidebar entry — 2px left accent border |
| `.icon-btn` / `.icon-btn-sm` | Icon-only buttons |

---

## Category 1: Panels & Cards

**Inline panel-likes (not using `.panel` / `Card`)**
- `SearchPanel.tsx:406` — `border: "1px solid var(--c-border)"; borderRadius: "0.75rem"` (uses 1px instead of canonical 2px)

**`.panel-raised` usage (correct)**
- `MessageReactions.tsx:85, 104, 116` — reaction pills, add-reaction button, emoji picker

**Could consolidate to**: `SearchPanel.tsx:406` should switch to `.panel` (or `Card`). The 1px-vs-2px difference is real visual drift.

---

## Category 2: List Rows

Three independent implementations of "clickable row with hover + active-border" pattern:

| Implementation | File | Active style | Hover style |
|---|---|---|---|
| Sidebar `Row` | `Sidebar.tsx:337–421` | `borderLeft: 2px solid var(--c-accent)` (else transparent), inline JS handler `setHover()` | `background: var(--c-hover)` |
| TerminalMenu item | `TerminalMenu.tsx:192–242` | `borderLeft: 3px solid var(--c-accent)` (else transparent) | `background: var(--c-active)` |
| `.sidebar-item-active` CSS | `index.css:155` | 2px left-border, accent color | — |

**Issues**:
- 2px vs 3px accent indicator (Sidebar 2px, TerminalMenu 3px)
- Sidebar's `Row` reinvents `.sidebar-item-active` without using the class
- Hover backgrounds use different tokens (`--c-hover` vs `--c-active`)

**Could consolidate to**: a `ListRow` component wrapping `.sidebar-item` + `.sidebar-item-active` with consistent 2px indicator. Medium effort.

---

## Category 3: Badges / Pills / Chips / Counters

### 3a. Unread count bubbles

| File | Style | Look |
|---|---|---|
| `Sidebar.tsx:423` (`UnreadBadge`) | `padding: 2px 6px; borderRadius: 8; background: muted \| accent` | Rounded pill, white text |
| `TerminalMenu.tsx:233–239` | `<span>[{badge}]</span>` in accent text | Bracket-wrapped plain text |

**Could consolidate to**: `Badge` component with `variant: "pill" | "bracket"`.

### 3b. Status/state indicators

Inline `<span className="inline-flex items-center gap-1 text-2xs font-mono">` with computed color, repeated across:
- `SecuritySettings.tsx:228` — "verified" (`var(--c-accent)`, `ShieldCheck`)
- `SecuritySettings.tsx:256` — "active" / "stale" / "warning" (status-computed)
- `KeyVerification.tsx:76` — "Key changed" (`#f0b429`, `ShieldAlert`)
- `KeyVerification.tsx:97` — "needs verification" (`#f0b429`)
- `KeyVerification.tsx:141` — "Matches" / "No match" (accent or `#ff6b6b`)

**Could consolidate to**: `StatusBadge` with `status` + optional `icon` + `label`. Low effort, high readability win.

### 3c. Password strength

- `SecuritySettings.tsx:73–74` — `strengthColor(score: 0–5)` returns `#ff6b6b` … `var(--c-accent)`
- `SecuritySettings.tsx:142, 184` — `<span style={{ color: strengthColor(...) }}>{label}</span>`

**Could consolidate to**: `StrengthMeter` component.

### 3d. Quality indicator

- `VoiceChannelView.tsx:12–26` — `qualityIndicator(quality)` returns `{ color, label }`
- `VoiceChannelView.tsx:122` — `<Circle size={8} fill={color} color={color} />`

**Could consolidate to**: `QualityIndicator` component (low priority, single instance today, but template for future signal/strength dots).

---

## Category 4: Avatars

**No duplication.** `Avatar` and `PresenceAvatar` cover all usage. ✓

---

## Consolidation Roadmap

| Category | Current state | Suggested | Priority |
|---|---|---|---|
| Panels | `SearchPanel.tsx:406` uses 1px inline | Standardize on `.panel` | Low |
| List rows | 3 hand-coded implementations, 2px vs 3px drift | `ListRow` component, standardize 2px | Medium |
| Unread badges | `UnreadBadge` pill + bracket text | `Badge` with variant | Low |
| Status badges | 4+ inline `<span>` patterns | `StatusBadge` component | Low |
| Strength meter | Inline `strengthColor` function | `StrengthMeter` component | Low |
| Quality indicator | One-off inline | `QualityIndicator` component | Low |
| Avatars | Fully consolidated | — | — |

**Highest-value win**: extract `ListRow` — eliminates the 3px/2px inconsistency across the three biggest navigation surfaces (Sidebar, TerminalMenu, SearchPanel results) and removes inline hover JS in `Sidebar.Row`.
