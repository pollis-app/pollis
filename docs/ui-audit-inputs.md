# Text Input Elements Audit

## Canonical Primitives

The frontend defines **three reusable input components** under `frontend/src/components/ui/`:

1. **TextInput** (`TextInput.tsx`) — controlled form field with label, error/description, focus-indicator chevron.
   - Supports: `type="text|password|email|number"`, required flag, disabled state
   - Focus ring: `focus:ring-4 focus:ring-[var(--c-accent)]` with offset-2
   - Border on focus: switches to `var(--c-border-active)`
   - Dynamic padding: `1.5rem` (left) when focused with icon, `0.75rem` otherwise

2. **TextArea** (`TextArea.tsx`) — multi-line variant mirroring TextInput styling/behavior.
   - Same focus ring, border logic, chevron indicator on focus
   - Disables resize (`resize-none`)

3. **ChatInput** (`ChatInput.tsx`) — specialized textarea for messages with attachment UI.
   - No label; inline focus class (`is-focused`) for background color swap
   - Focus background: `var(--c-accent)`, text color swaps to `var(--c-bg)`
   - Placeholder styling: uses `.chat-input-textarea.is-focused::placeholder` in CSS

4. **InputOtp** (`InputOtp.tsx`) — OTP/PIN field: 6 single-digit inputs in a row.
   - Individual inputs: `2px border`, `2px solid var(--c-border)` → `var(--c-accent)` on focus
   - Background: `var(--c-surface)` → `var(--c-accent)` on focus
   - No label per input; aria-label per digit

## CSS Utility Classes

From `frontend/src/index.css` (lines 183–218):

- `.pollis-input` — standard form field style
  - Padding: `px-3 py-2`, rounded panel, `2px border` var(--c-border)
  - Focus: border → `var(--c-border-active)` (no ring)
  - Used in: SecuritySettings password inputs, KeyVerification safety-number, SearchView

- `.pollis-textarea` — wrapper extending `.pollis-input` with `resize-none`

- `.chat-input-textarea` — message composer placeholder color override
  - Unfocused: `var(--c-text-muted)`
  - Focused (`.is-focused`): blended with `var(--c-bg)` for dark-on-light readability

---

## Raw Input/Textarea Usage by Category

### Standard Form Fields (login, profile, settings)

Migrate these to **TextInput**:

- `SecuritySettings.tsx:123` — password input (export), `type="password"`, uses `.pollis-input`, visibility toggle button
- `SecuritySettings.tsx:165` — password input (import), `type="password"`, uses `.pollis-input`, visibility toggle button
- `SearchView.tsx:144` — message search, `type="text"`, uses `.pollis-input`, **no label**
- `KeyVerification.tsx:124` — manual safety-number entry, `type="text"`, uses `.pollis-input`, **no label**
- `Preferences.tsx:205` — accent hex input, `type="text"`, **raw inline styles** (border, background, ring-4)
  - Border: `1px solid var(--c-border)`, focus ring: `focus:ring-4 focus:ring-[var(--c-accent)]`
  - Inconsistent with TextInput: missing offset, no padding helper
- `Preferences.tsx:289` — background hex input, `type="text"`, **raw inline styles** (same as accent)

**Duplication note:** Preferences color inputs (lines 205 & 289) recreate TextInput styling inline instead of composing it. Also use `focus:ring-4` vs TextInput's `focus:ring-4` with `focus:ring-offset-2` — inconsistent offset pattern.

---

### Search Bars (filter-as-you-type)

- `SearchPanel.tsx:428` — global search panel, `type="text"`, **no className at all**
  - Bare inline `style={{}}`: transparent background, no border, no focus ring
  - This is intentional (overlay input) but visually different from form inputs
  - **Not a consolidation candidate** — specific UI pattern

- `SearchView.tsx:144` — per-channel message search, `type="text"`, uses `.pollis-input`
  - Has label, full border+ring treatment
  - **Could consolidate to TextInput** if label placement fits

---

### OTP / PIN / Verification Inputs

**Dedicated `InputOtp` component** (lines 80–118) — properly encapsulates:
- Multi-input row layout
- Digit masking (password mode)
- Paste support & backspace navigation
- Auto-advance on fill

Used by:
- `PinEntryScreen.tsx:104` — PIN unlock (4 digits, masked)
- `PinCreateScreen.tsx:122` — PIN setup (4 digits, masked)
- `EmailOTPAuth.tsx:121` — email verification code (6 digits, unmasked)
- `ChangePinPage.tsx` — (same pattern)

**All OTP/PIN use InputOtp. No duplication here.**

---

### Message Composer / Chat Textarea

**ChatInput** (`ChatInput.tsx:535`) — multi-line, auto-grow, attachment management.
- Uses `.chat-input-textarea` class + inline focus styles
- Background: `isFocused ? var(--c-accent) : var(--c-hover)`
- Focus indicator: only background swap + placeholder color

**Edit message textarea** (`MainContent.tsx:502`) — reuses `.chat-input-textarea`
- Same styling, same focus-driven background swap
- **Properly consolidated: both use ChatInput class.**

**Duplication within MainContent:** The edit textarea (line 502) uses inline `style={{}}` to set focus colors instead of relying on `.is-focused` class like ChatInput does. See lines 523–526:
```tsx
background: editBarFocused ? 'var(--c-accent)' : 'var(--c-hover)',
color: editBarFocused ? 'var(--c-bg)' : 'var(--c-text)',
```
This is duplicated inline instead of letting CSS handle it via `.is-focused`. **Could extract a shared `EditMessageTextarea` component or consolidate styling into CSS.**

---

### Inline Edit Fields (rename in place)

No dedicated inline-edit inputs found. Some pages use hidden `<input type="hidden">` for test IDs:
- `RenameGroup.tsx:103, 114` — hidden inputs (read-only, test-only)
- `RenameChannel.tsx:106, 117` — hidden inputs (test-only)
- `CreateGroup.tsx:109, 121, 132` — hidden inputs (test-only)
- `CreateChannel.tsx:141, 152, 163, 173` — hidden inputs (test-only)
- `Settings.tsx:308, 317, 329, 382, 409, 449` — hidden inputs (test-only)
- `SearchGroup.tsx:74` — hidden input (test-only)
- `StartDM.tsx:65` — hidden input (test-only)

**These are scaffolding (bridge to React Query), not actual UI inputs.** Not relevant to this audit.

---

### Password Fields with Visibility Toggle

- `SecuritySettings.tsx:123` — export password + Eye/EyeOff toggle
  - `.pollis-input` wrapper with icon button sibling
- `SecuritySettings.tsx:165` — import password + Eye/EyeOff toggle
  - Same pattern

**Pattern is correct but not extracted.** Could create a `PasswordInput` component wrapping TextInput + visibility button.

---

### Numeric / Range / Slider Fields

- `Preferences.tsx:205` — hex color input, `type="text"` with hex validation
  - Maxlength={7}, raw inline styles, focus ring inconsistency (no offset)
- `Preferences.tsx:289` — background hex, same as above
- `AudioPlayer.tsx:154, 212` — range sliders (`type="range"`), **not text inputs**
- `RangeSlider.tsx:64` — custom range wrapper, **not a text input**

**Hex inputs (Preferences):** duplicated inline styles across two inputs. No shared component. Could extract `HexColorInput` component.

---

## Summary of Consolidation Opportunities

### High Priority (Remove Duplication)

1. **Preferences hex color inputs** (lines 205 & 289)
   - Both use identical raw inline `style={{}}` patterns
   - **Consolidate to:** `TextInput` with `type="text"`, validation wrapper, and consistent focus ring (add `focus:ring-offset-2`)

2. **MainContent edit textarea** (line 502)
   - Duplicates ChatInput's focus-driven styling via inline `style={{}}` instead of `.is-focused` class
   - **Consolidate to:** Extract shared `EditMessageTextarea` component or ensure both use identical CSS class pattern

3. **SecuritySettings password inputs** (lines 123 & 165)
   - Both use `.pollis-input` + visibility toggle button in identical layout
   - **Consolidate to:** `PasswordInput` component (wraps TextInput + Eye toggle) to avoid button sibling duplication

### Medium Priority (Standardize)

4. **SearchView message search** (line 144)
   - Uses `.pollis-input` (inconsistent with SearchPanel's bare input)
   - Already follows pattern but **could use TextInput** if label placement works

5. **KeyVerification safety-number input** (line 124)
   - Uses `.pollis-input` (good)
   - **Missing visible label:** uses section-label above but input has no associated `<label>`; already has `id` so could pair them

6. **Focus ring inconsistency across inlines**
   - TextInput/TextArea: `focus:ring-4 focus:ring-offset-2 focus:ring-offset-black` (ring width + offset)
   - Preferences hex inputs: `focus:ring-4` (no offset) — **add offset for consistency**
   - SearchPanel: no ring (intentional) — keep as-is
   - ChatInput: no ring (background swap is indicator) — keep as-is

### Low Priority (Already Correct)

7. **OTP/PIN inputs** — all use `InputOtp`, no duplication
8. **Chat textareas** — both use `.chat-input-textarea`, properly consolidated

