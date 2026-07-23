# Pollis mobile — e2e selectors (testID)

Reference for Maestro / e2e flows. Every load-bearing interactive element in the
Expo app carries a stable `testID`. These are inert in production (RN forwards
`testID` to the native view's accessibility identifier) and are purely additive —
they never change behavior, styling, or logic.

Most shared primitives in `components/ui.tsx` accept an optional `testID` (and,
where meaningful, an `accessibilityLabel`) and forward it to the underlying RN
element: `Screen`, `Button`, `CtxAct`, `Field`, `Toggle`, `ListRow`, `Chip`,
`Ctx`, `Crumb`. Raw `Pressable` / `TextInput` / `View` take `testID` natively.

## Naming scheme

- `screen-<route>` — one root anchor per screen, set on that screen's `<Screen>`.
- `btn-<name>` — buttons / pressables (actions).
- `input-<name>` — text inputs.
- `toggle-<name>` — toggles / switches.
- `chip-<name>` — interactive chips.
- `row-<kind>-<id>` — list rows; `<id>` is the real record id where the
  map/loop variable exposes one, else the row's index.
- `tab-<name>` — bottom tab bar entries.

Where a route renders a repeated action (e.g. per-request approve/reject, per-row
delete/remove/revoke), the record id is appended to keep the selector unique
(`btn-approve-<id>`, `btn-remove-member-<id>`, `btn-revoke-device-<id>`, …).

## Shared / cross-screen

| Element | testID |
| --- | --- |
| Bottom back button (in `<Ctx>` strip) | `btn-back` |
| Tab bar entries (`<TabBar>`) | `tab-groups`, `tab-direct`, `tab-search`, `tab-self` |

## Pre-existing screens (instrumented in the first pass)

### Auth

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `(auth)/email` | `screen-auth-email` | `input-email`, `btn-submit-email` |
| `(auth)/otp` | `screen-auth-otp` | `input-otp`, `btn-submit-otp` |
| `(auth)/pin` | `screen-auth-pin` | keypad `btn-pin-0`…`btn-pin-9`, `btn-pin-back`, `btn-pin-signout` |
| `(auth)/initializing` | `screen-auth-initializing` | `btn-continue` |
| `(auth)/emergency-kit` | `screen-auth-emergency-kit` | `toggle-recovery-ack`, `btn-continue` |
| `(auth)/enrollment` | `screen-auth-enrollment` | `btn-enroll-approve-device`, `btn-enroll-recovery`, `input-recovery-key`, `btn-submit-recovery`, `btn-enroll-back`, `btn-enroll-cancel` |

### Tabs

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `(tabs)/groups` | `screen-groups` | `row-invite-<id>`, `btn-decline-invite-<id>`, `btn-accept-invite-<id>`, `row-channel-<id>`, `btn-create-group`, `btn-join-group` |
| `(tabs)/direct` | `screen-direct` | `row-request-<id>`, `btn-accept-request-<id>`, `row-dm-<id>`, `btn-new-dm` |
| `(tabs)/search` | `screen-search` | `input-search`, `row-group-<id>`, `row-channel-<id>`, `row-user-<id>`, `row-message-<id>` |
| `(tabs)/self` | `screen-self` | `row-self-preferences`, `row-self-user-settings`, `row-self-security`, `btn-sign-out` |

### Pushed screens

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `chat/[id]` | `screen-chat` | `input-composer`, `btn-send`, `btn-attach`, `row-message-<id>`, `btn-members`, `btn-chat-menu`, `btn-react-<i>`, `btn-edit`, `btn-delete`, `input-edit-composer`, `btn-edit-save`, `btn-edit-cancel`, `btn-action-cancel` |
| `group/[id]` | `screen-group` | `row-channel-<id>`, `row-group-members`, `row-group-invite`, `row-group-settings`, `row-group-requests`, `btn-leave-group`, `btn-group-menu` |
| `group/new` | `screen-group-new` | `input-group-name`, `input-group-description` |

## New screens (this pass)

### User

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `user/[id]` | `screen-user` | `text-safety-number`, `btn-verify` (mark/unmark verified), `btn-block` / `btn-unblock` (label switches on state), `btn-message` (start DM) |

### Self

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `self/preferences` | `screen-self-preferences` | accent: `chip-accent-amber`, `chip-accent-citron`, `chip-accent-mint`, `chip-accent-glass`, `chip-accent-lilac`, `chip-accent-rust`; theme: `chip-theme-coal`, `chip-theme-paper`, `chip-theme-system`; density: `chip-density-compact`, `chip-density-comfortable`; behavior toggles: `toggle-show-inline-timestamps`, `toggle-show-member-avatars`, `toggle-mark-verified-peers`, `toggle-read-receipts`, `toggle-reduce-motion`; `toggle-notifications` |
| `self/user-settings` | `screen-self-user-settings` | `input-display-name`, `input-handle`, `input-email` (read-only), `btn-change-email`, `btn-save`, `btn-cancel` |
| `self/security` | `screen-self-security` | enrollment: `btn-approve-<requestId>`, `btn-reject-<requestId>`; devices: `row-device-<deviceId>`, `btn-revoke-device-<deviceId>`; `row-blocked-users` (nav to blocked list); `btn-sign-out` |
| `self/blocked` | `screen-self-blocked` | `row-blocked-<id>`, `btn-unblock-<id>` |
| `self/change-email` | `screen-self-change-email` | `input-email` (enter-email stage), `input-otp` (enter-code stage), `btn-request-otp` (enter-email stage) / `btn-submit` (enter-code stage), `btn-use-different-email` |

### Direct messages

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `dm/new` | `screen-dm-new` | `input-user-search`, `row-user-<id>` (single exact-match result; tapping starts the DM) |
| `dm/info` | `screen-dm-info` | `row-member-<userId>` (tap → user profile), `btn-leave` (leave conversation), `btn-back-to-conversation` |

### Groups

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `group/members` | `screen-group-members` | `row-member-<userId>`, `btn-toggle-role-<userId>` (make/remove admin), `btn-remove-member-<userId>` |
| `group/requests` | `screen-group-requests` | `row-request-<id>`, `btn-approve-<id>`, `btn-reject-<id>` |
| `group/settings` | `screen-group-settings` | `input-group-name`, `input-group-description`, `row-channel-<id>`, `btn-delete-channel-<id>`, `btn-delete-group` (owner-only), `btn-save` |
| `group/invite` | `screen-group-invite` | `input-user-search`, `btn-send-invite`, `btn-cancel` |
| `group/discover` | `screen-group-discover` | `input-group-search`, `btn-request-access` (join), `btn-back` |

## Coverage notes / gaps (vs. the requested selector list)

- `group/settings` has **no** leave-group control on mobile — only an
  owner-only `btn-delete-group`. There is no `btn-leave-group` here (leaving a
  group lives on `group/[id]` as `btn-leave-group`). No `toggle-*` exists on
  this screen either (group settings are name/description text fields + channel
  management only).
- `self/security` has **no** change-password / PIN-entry buttons — recovery/PIN
  management is explicitly "not wired on mobile yet" (copy in the RECOVERY
  section). Safety numbers live on `user/[id]`, not here. Enrollment
  approve/reject and device revoke are the load-bearing actions.
- `user/[id]` self-view shows no safety-number card or block/message buttons
  (it's you) — those testIDs only render for a peer.
- `dm/new` returns a single exact-match user (no result list); the one result
  row `row-user-<id>` doubles as the start-DM affordance (no separate
  `btn-start-dm`).
- `group/discover` renders the matched group in a `Card` (not a list row); the
  join affordance is `btn-request-access` (only shown when there is no
  pending/approved/rejected request).
