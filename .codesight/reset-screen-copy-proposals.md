# Reset account screen — copy shortening proposals

Context: `frontend/src/components/Auth/EnrollmentGateScreen.tsx`, `ResetConfirmPane` (~lines 453–555). The "NEW DEVICE → Reset this account" panel is too tall for small device heights. Goal: reduce vertical size while preserving the sense that the action is destructive and irreversible.

## Proposal 1 — Light trim (conservative)

- Heading stays: "Reset this account"
- Merge paragraphs into one: "Wipes this device's messages, signs out your other devices, and removes you from your groups (admins can re-invite). Email and username stay. A new Secret Key appears once — save it or you'll do this again."
- Checkbox label: "I understand my messages and group memberships will be wiped."
- Keep email confirm + both buttons.

Saves: ~80–100px.
Sacrifices: Less hand-holding on "keep email/username"; tucked into one clause.

## Proposal 2 — Medium (recommended)

- Heading: "Reset this account"
- Single line: "Wipes this device, signs out your others, removes you from all groups. Email and username stay. New Secret Key shown once."
- Drop the checkbox entirely — the typed-email confirmation is already a strong gate.
- Keep "Type your email to confirm" + Reset button + Back.

Saves: ~160–200px.
Sacrifices: Loses explicit "irreversible" wording and the double-confirmation ritual. "Admins can invite you back" reassurance is gone.

## Proposal 3 — Dense (aggressive)

- Heading + inline warning on one row: "Reset account — destroys messages, groups, other sessions."
- Helper under email input: "Keeps email + username. New Secret Key shown once."
- No checkbox. No standalone paragraphs.
- Reset button + Back.

Saves: ~240–280px.
Sacrifices: Near-zero ceremony. Relies entirely on the destructive button variant and typed-email gate to convey severity.

## Proposal 4 — Progressive disclosure

- Phase A: heading + one line "This wipes messages and removes you from groups." + "Continue" + Back.
- Phase B (after Continue): email-type confirm + Reset button, with a small "What this does" expander.

Saves: ~200px on each phase.
Sacrifices: Adds a click. Full consequences behind an expander may be skipped.

## Ranking by aggression
1 < 4 < 2 < 3. Proposal 2 is the best tradeoff.
