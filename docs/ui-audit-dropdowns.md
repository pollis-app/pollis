# UI Audit: Dropdowns and Select Components

## Summary

Pollis has **minimal dropdown duplication** by design. It uses:
- One canonical reusable component: `TerminalMenu` (keyboard-navigable list with selection)
- Native `<select>` for simple form dropdowns (audio device pickers)
- One command-menu overlay: `SearchPanel` (Cmd+K, the sanctioned exception per CLAUDE.md)
- No custom popovers, action menus, autocomplete, or context menus

No consolidation needed in this category.

---

## Canonical Primitives

### TerminalMenu
**File**: `frontend/src/components/ui/TerminalMenu.tsx`

Keyboard-navigable list of selectable items with optional icons, badges, descriptions, and secondary actions.

Props of note:
- `items: TerminalMenuItem[]` — rows with label, icon, description, action, disabled, badge
- `onEsc?: () => void`
- `autoFocus?: boolean` (default true)

Capabilities: Arrow Up/Down navigation with wrapping, Enter to select, Escape to dismiss, skip separators, scroll-into-view on select, hover/keyboard sync, secondary action button (⋮) per row, `onSelect` callback (used to eagerly warm LiveKit voice connections).

### NavigableList
**File**: `frontend/src/components/ui/NavigableList.tsx:51`

Two-dimensional list: rows with right-aligned focusable controls (buttons, switches). Used by Members, Requests, Blocked Users, Invites pages. Not a dropdown — distinct from TerminalMenu's single-select flow.

---

## Native `<select>` Usage

### Form Selects in Voice Settings
**File**: `frontend/src/pages/VoiceSettingsPage.tsx`

Two thin wrappers around native `<select>` share one style object:

1. **DeviceSelect** (lines 29–57) — mic/speaker device picker, used at lines 209–222
2. **NoiseSuppressionSelect** (lines 64–91) — enum dropdown (Off/Low/Moderate/High), used at line 400

Both reuse `selectStyle` (lines 93–106) and an inline ChevronDown overlay. Focus state mutates `borderColor` inline (lines 37–38, 72–73).

**Could consolidate to**: already consolidated — both reuse the shared `selectStyle`.

---

## Custom Dropdown Popovers

**None found.** No floating popovers, no kebab/action menus, no autocomplete, no context menus.

---

## By Category

### 1. Form selects (label + dropdown)
- `DeviceSelect` — `frontend/src/pages/VoiceSettingsPage.tsx:29`
- `NoiseSuppressionSelect` — `frontend/src/pages/VoiceSettingsPage.tsx:64`
- Already consolidated via shared `selectStyle`.

### 2. Navigable lists (keyboard-driven row selection)
- `TerminalMenu` — `frontend/src/components/ui/TerminalMenu.tsx:33`
- `NavigableList` — `frontend/src/components/ui/NavigableList.tsx:51`
- Different purposes (flat single-select vs. row + controls). Keep separate.

### 3. Command menu (Cmd+K, sanctioned overlay)
- `SearchPanel` — `frontend/src/components/SearchPanel.tsx:218`
- Fixed-position centered overlay, arrow navigation, real-time filtering. Unique use case; not a candidate for merging with TerminalMenu.

### 4. Lightbox overlays (media)
- Image, audio, video lightboxes — `frontend/src/components/Message/MessageItem.tsx:698, 770, 887`
- Content-viewing, not selection dropdowns. Out of scope.

---

## Duplication Analysis

- **Form selects**: both reuse `selectStyle` — no duplication.
- **Navigable lists**: `TerminalMenu` and `NavigableList` serve different needs; merging adds complexity.
- **Outside-click handling**: only `SearchPanel` and lightboxes use it; no duplication.
- **Escape-to-close**: each component handles independently and appropriately; shared primitive unwarranted.

---

## Recommendation

**No consolidation needed.** Guidance for future work:
- New menu-selection flows → use or extend `TerminalMenu`
- New form dropdown → native `<select>` with `selectStyle`
- New search/filter UI → reference `SearchPanel` but respect the no-modals rule
