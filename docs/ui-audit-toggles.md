# Frontend Toggle Controls Audit

## Overview

Pollis provides two canonical toggle primitives under `frontend/src/components/ui/`:
- **Switch** (slider-style on/off; role="switch", uses button under the hood)
- **Checkbox** (square box with checkmark; role="checkbox", uses label wrapper)

Both implement proper accessibility patterns and use CSS custom properties for theming. According to CLAUDE.md, toggles/switches **MUST use the `Switch` ui component**.

## Canonical Primitives

### Switch.tsx
- **Props**: `label`, `checked`, `onChange`, `disabled?`, `className?`, `id?`, `description?`
- **Rendering**: Button with role="switch", custom-styled slider pill (translateX animation)
- **Status**: Reference implementation ✓

### Checkbox.tsx
- **Props**: `label`, `checked`, `onChange`, `disabled?`, `className?`, `data-testid?`
- **Rendering**: Role="checkbox" in label wrapper, custom-styled square with checkmark
- **Status**: Reference implementation ✓

---

## Raw Input Type Usage (NON-CANONICAL)

**These bypass the canonical components and should be consolidated:**

### `<input type="checkbox">` (RAW, Non-canonical)
1. **NetworkStatusIndicator.tsx:51** — Kill-switch toggle
   - Raw `<input type="checkbox">` with inline `accentColor` style
   - **Issue**: Should use `Checkbox` component
   - **Replace with**: `<Checkbox label="kill-sw" {...}>`

2. **SecuritySettings.tsx:99** — Message previews notification toggle
   - Raw `<input type="checkbox">` with inline `accentColor` style
   - **Issue**: Should use `Checkbox` component
   - **Replace with**: `<Checkbox label="Message previews" {...}>`

### `<input type="radio">`
- **None found** — No radio groups in current codebase

---

## Icon Toggle Buttons (aria-pressed)

**These are toggle-like buttons that don't fit checkbox/switch semantics:**

### MessageReactions.tsx:91, 134
- **Pattern**: Emoji reaction pills with `aria-pressed` attribute
- **Behavior**: Toggle emoji reaction on/off via button click
- **Visual state**: Color changes (accent vs muted) based on `reacted` boolean
- **Current impl**: Custom button with inline style based on state
- **Issue**: Each emoji is a standalone button; no cohesive toggle component
- **Consolidation note**: Could create a reusable `<ReactionToggle emoji={string}>` wrapper if reaction UI scales

### AudioPlayer.tsx:173, 182, 191, 203
- **Play/Pause toggle** (line 173): Button toggles `isPlaying` state
- **Mute toggle** (line 203): Button toggles `isMuted` state
- **Pattern**: Icon buttons (lucide-react) with aria-label indicating toggle state
- **Visual state**: Icon changes (Play↔Pause, Volume2↔VolumeX)
- **Current impl**: Inline button with onClick handler; no toggle component wrapper
- **Issue**: Not a semantic toggle; mimics one via icon swap
- **Note**: OK for media controls (common pattern); no consolidation needed

### InlineAudioPlayer.tsx:89
- **Play/Pause toggle**: Button with onClick={togglePlay}
- **Pattern**: Same as AudioPlayer, icon-based toggle
- **Status**: OK as-is (media control pattern)

### VoiceBar.tsx:66
- **Mute toggle** (PillButton): `voiceIsMuted ? "#ff6b6b" : "var(--c-accent)"`
- **Current impl**: Uses `PillButton` component with accent color indicating state
- **Issue**: `PillButton` is a generic button, not a toggle primitive
- **Note**: Acceptable for tight UI contexts; semantic toggle not required

### SecuritySettings.tsx:132, 174
- **Eye icon buttons** (Show/hide password toggles)
- **Pattern**: Icon button that changes icon on click
- **Visual state**: Icon changes (Eye↔EyeOff)
- **Current impl**: Inline button with onClick
- **Status**: OK (password visibility toggle is a standard pattern)

---

## Slider/Range Inputs

### AudioPlayer.tsx:154, 212
- **Seek slider**: `<input type="range">` for audio progress
- **Volume slider**: `<input type="range">` for volume control
- **Rendering**: HTML range input with custom CSS class `accent-slider`
- **Status**: Not a toggle; skip

### RangeSlider.tsx
- **Canonical range component** for multi-point sliders
- **Status**: Reference implementation (not toggle-related)

---

## State-Driven Button Toggles (Selection, Not Persisted Toggles)

### Sidebar.tsx:281, 337
- **Pattern**: Rows/sections use `isActive` prop to style current selection
- **Rendering**: `background` and `borderLeft` change based on `isActive`
- **Behavior**: Navigation routing (not a toggle state)
- **Status**: Skip (routing state, not toggle control)

### SearchPanel.tsx:501
- **Pattern**: Menu item `isSelected` to highlight current focus
- **Rendering**: `borderLeft` and `background` highlight
- **Status**: Skip (keyboard navigation, not toggle)

### TerminalMenu.tsx:188
- **Pattern**: Same as SearchPanel, `isSelected` state
- **Status**: Skip (menu selection, not toggle)

---

## Settings Pages with Switch Components (CANONICAL)

**All proper usage of `<Switch>` component:**

1. **CreateGroup.tsx:134, 142** — Private/restricted group toggles (2 switches)
2. **VoiceSettingsPage.tsx:305, 381, 405, 412, 427** — Multiple voice settings toggles (5 switches)
3. **Members.tsx:47** — Admin toggle (1 switch)
4. **CreateChannel.tsx:165** — Private/restricted channel toggle (1 switch)
5. **Preferences.tsx:396, 417, 437, 443** — UI preferences toggles (4 switches)
6. **EnrollmentGateScreen.tsx:511** — `<Checkbox>` for reset confirmation (canonical)

**Status**: These files follow CLAUDE.md guidance ✓

---

## Consolidation Opportunities

### 1. Slider-Style Switches
**Current**: Switch.tsx is the canonical primitive and is used consistently across pages.
**Status**: ✓ No consolidation needed

### 2. Checkboxes
**Current**: Checkbox.tsx is canonical but bypassed in 2 places (NetworkStatusIndicator, SecuritySettings)
**Action**: Replace raw `<input type="checkbox">` with `<Checkbox>` component
- NetworkStatusIndicator.tsx:49–57
- SecuritySettings.tsx:97–104

### 3. Icon Toggle Buttons
**Current**: Each media player implements play/pause and mute toggle independently
**Status**: OK (standard UX patterns; no shared logic)
**Note**: Could create `<MediaToggleButton>` if more media controls are added

### 4. Reaction Emojis (aria-pressed)
**Current**: Each emoji is a standalone button with custom styling
**Status**: OK for now (simple on/off per emoji)
**Note**: If reactions scale (e.g., reaction groups, quorum-based reactions), consider a `<ReactionButton>` primitive

---

## Summary

- **2 canonical primitives** (Switch, Checkbox) are well-defined and mostly followed
- **2 raw `<input type="checkbox">` violations** that should migrate to `<Checkbox>` component
- **No radio groups** found (not applicable)
- **Icon toggle buttons** (play/pause, mute, emoji reactions) are acceptable as one-offs; no consolidation needed
- **State-driven button styling** (navigation, menu selection) is routing/focus logic, not toggle controls

**Recommendation**: Fix the two checkbox violations, keep Switch/Checkbox as single sources of truth, and monitor emoji reactions if feature growth occurs.
