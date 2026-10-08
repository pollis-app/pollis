# Pollis mobile — e2e selectors (testID)

Reference for Maestro / e2e flows. Every load-bearing interactive element in the
Expo app carries a stable `testID`. These are inert in production (RN forwards
`testID` to the native view's accessibility identifier) and are purely additive —
they never change behavior, styling, or logic.

Most shared primitives in `components/ui.tsx` accept an optional `testID` (and,
where meaningful, an `accessibilityLabel`) and forward it to the underlying RN
element: `Screen`, `Button`, `IconButton`, `Field`, `Toggle`, `ListRow`, `Chip`,
`Badge`, `SectionTitle`, `Txt`; `Header` takes `backTestID` (default
`btn-back`). The old `Ctx` / `CtxAct` / `Crumb` strip is gone. Raw `Pressable` / `TextInput` / `View` take `testID` natively.

**Copy is sentence case since the 2026-10 redesign** — no uppercased labels
anywhere ("Create group", "Find group", "Language"). Maestro's text match is a
full, case-sensitive regex, so prefer a `testID`; where a flow must match text
(a dynamic row, a system dialog), match the catalogue string exactly or use a
`.*…*` / `(?i)` regex.

## Naming scheme

- `screen-<route>` — one root anchor per screen, set on that screen's `<Screen>`.
- `btn-<name>` — buttons / pressables (actions).
- `input-<name>` — text inputs.
- `toggle-<name>` — toggles / switches.
- `chip-<name>` — interactive chips.
- `row-<kind>-<id>` — list rows; `<id>` is the real record id where the
  map/loop variable exposes one, else the row's index.
- `tab-<name>` — bottom tab bar entries.
- `sheet-<name>` — a bottom sheet's root (`SheetOverlay`'s `testID`).
- `panel-<name>` — an in-place panel that is not its own route (no `screen-*`).

Where a route renders a repeated action (e.g. per-request approve/reject, per-row
delete/remove/revoke), the record id is appended to keep the selector unique
(`btn-approve-<id>`, `btn-remove-member-<id>`, `btn-revoke-device-<id>`, …).

## Shared / cross-screen

| Element | testID |
| --- | --- |
| Back chevron, top-start of every pushed screen (`<Header>`; `<BackBar>` on `group/[id]`) | `btn-back` |
| Tab bar entries (`<TabBar>`) | `tab-groups`, `tab-direct`, `tab-search`, `tab-self`; unread badge `badge-<tab>` |
| Bottom sheet Close button (top-end of every `SheetOverlay`; `closeTestID`) | `btn-sheet-close` by default (group menu, add group, create channel, emoji picker); the message-actions sheet's Close is `btn-action-cancel` and the channel sheet's is `btn-menu-cancel`, so those long-standing ids still close their sheets |
| Sign-out confirmation | **native Alert, no testID.** `btn-sign-out` (Self tab and `self/security`) opens it; tap the destructive button by its label, `auth:shell.signOutTitle` — en `"Sign out"`, es `"Cerrar sesión"` (Cancel = `common:actions.cancel`). Title: `mobile:self.hub.signOutConfirmTitle` ("Sign out of Pollis on this device?"). |

## Pre-existing screens (instrumented in the first pass)

### Auth

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `(auth)/email` | `screen-auth-email` | `input-email`, `btn-submit-email`, `btn-sign-in-with-device` (→ `(auth)/link`), `text-legal` |
| `(auth)/link` | `screen-auth-link` | `link-camera`, `btn-link-allow-camera`, `btn-link-toggle-manual`, `input-link-code`, `btn-link-submit`, `link-claiming`, `link-error`; the top back chevron is `btn-link-use-email` (back to email sign-in), not `btn-back` |
| `(auth)/otp` | `screen-auth-otp` | `input-otp`, `btn-submit-otp` |
| `(auth)/pin` | `screen-auth-pin` | keypad `btn-pin-0`…`btn-pin-9`, `btn-pin-back`, `btn-pin-signout` |
| `(auth)/initializing` | `screen-auth-initializing` | `btn-continue` |
| `(auth)/emergency-kit` | `screen-auth-emergency-kit` | `btn-copy-recovery-key`, `btn-share-recovery-key`, `toggle-recovery-ack`, `btn-continue` |
| `(auth)/enrollment` | `screen-auth-enrollment` | `btn-enroll-approve-device`, `linked-awaiting-approval` (QR-linked device waiting), `btn-enroll-recovery`, `input-recovery-key`, `btn-submit-recovery`, `btn-enroll-back`, `btn-enroll-cancel` |

### Tabs

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `(tabs)/groups` | `screen-groups` | **No groups yet:** empty state with `btn-create-group`, `btn-join-group` directly (no pill strip, no `btn-add-group`). **With groups:** the group pill strip — `row-group-<groupId>` per group (tapping one switches the panel IN PLACE; it does not push `group/[id]`, so assert `panel-group`, not `screen-group`), then `btn-add-group` ("+", opens `sheet-add-group` holding `btn-create-group` / `btn-join-group`) — and the selected group's `panel-group` (`GroupPanel`, ids under `group/[id]` below). Pending items above the channels: `row-join-requests-<groupId>` + `badge-join-requests-<groupId>` (admins), `row-invite-<id>`, `btn-decline-invite-<id>`, `btn-accept-invite-<id>`. Flows that create/find a group from this tab tap `btn-add-group` with `optional: true` first, which covers both states. Group ids are 26-char ULIDs, so match a pill as `row-group-[0-9A-Z]{26}` — `row-group-.*` also matches the panel's `row-group-members` etc. |
| `(tabs)/direct` | `screen-direct` | `btn-new-dm` (header, always present), `input-dm-search` (only once there are DMs), `row-dm-requests` (only while a request is pending; opens `dm/requests`), `row-dm-<id>` + `unread-<id>`, empty state `direct-empty` + `btn-new-dm-empty`. DM requests are **not** accepted here any more — see `dm/requests`. |
| `(tabs)/search` | `screen-search` | `input-search` (top of the screen; normal keyboard with a return key, so `hideKeyboard` works), empty state `search-filter-hint` + filter chips `search-filter-from`, `-in`, `-before`, `-after`, `-on`, `-hasattachment`, `-haslink`; results `search-about-results` ("About N results"), `search-sort-relevant`, `search-sort-recent`, `row-message-<id>`, `search-load-more`, `search-corpus-footer`, `search-indexing`, `search-no-results-why`; `row-group-<id>`, `row-channel-<id>`, `row-user-<id>` (account lookup runs on return), `row-page-<id>` (settings quick-jump, e.g. `row-page-security`) |
| `(tabs)/self` | `screen-self` | `card-self-profile` + `btn-edit-profile` (→ `self/user-settings`), `row-self-user-settings`, `row-self-security`, `row-self-saved`, `row-self-preferences`, `row-self-notifications` / `row-self-language` (→ `self/preferences` scrolled to that section), `row-self-autolock` (→ `self/security` scrolled to auto-lock), `btn-lock-now`, `btn-sign-out` (confirms first — see Shared) |

### Pushed screens

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `chat/[id]` | `screen-chat` | On iPad (regular width) the conversation is the Groups/Direct tab's right pane — same ids, `screen-chat` on the pane, no `btn-back`; info/members/settings/thread/profile pages open inside that pane and their `btn-back` pops it. header: `btn-back`, `btn-members` (→ `conversation/info`), `btn-chat-menu` (channel: opens the channel sheet; DM: pushes `dm/info`); `list-messages`, `row-message-<id>` (long-press → message sheet; label `"<name>: <text>, <status or time>"`), `btn-thread-<id>`, `pill-reaction-<msgId>-<emoji>`, `receipt-<msgId>`; composer `input-composer`, `btn-send`, `btn-attach`, `btn-composer-emoji`, `strip-attachments` / `chip-attachment-<id>`, `list-mention-suggestions` / `row-mention-<username>`, `list-emoji-suggestions` / `row-emoji-<shortcode>`; edit bar `input-edit-composer`, `btn-edit-save`, `btn-edit-cancel`. **Message sheet:** `btn-react-<i>`, `btn-react-more` (→ emoji picker: `input-emoji-search`, `list-emoji`, `btn-tone-<i>`), `btn-reply-thread`, `btn-copy-text`, `btn-copy-link`, `btn-save`, `btn-edit`, `btn-delete`, `btn-report`; Close = `btn-action-cancel`. **Channel sheet:** `btn-menu-info`, `btn-menu-group-settings`; Close = `btn-menu-cancel`. |
| `chat/thread` | `screen-thread` | `list-thread`, `row-thread-root-<id>`, `row-thread-<id>` |
| `group/[id]` | `screen-group` | Phones only — reached from Search group hits and invite links; creating a group lands on the Groups tab instead (`panel-group`), and on iPad (regular width) this route redirects to the Groups tab two-pane. `btn-back` (top back bar), then the same `GroupPanel` the Groups tab shows: `btn-group-menu` (group name; opens `sheet-group-menu`: `btn-menu-members`, `btn-menu-invite`, `btn-menu-group-settings`, `btn-menu-requests` (admins, while requests are pending), `btn-leave-group`), `row-group-invite` (invite icon), `btn-search-group` (opens Search), `btn-create-channel` (admins; opens `sheet-create-channel`: `input-channel-name`, `btn-create-channel-submit`), `row-channel-<id>`, `row-group-members`, `row-group-settings`, `row-group-requests` (only while requests are pending). On the Groups tab the panel's root is `panel-group`. |
| `group/new` | `screen-group-new` | `input-group-name`, `input-group-description`, `toggle-general-channel` (opt-in #General, off by default), `btn-submit-group` (submit; dismisses back to the Groups tab with the new group selected — wait for `btn-group-menu` with text `"<name>, .*"`, then `panel-group`). No Cancel button — leave with `btn-back`. |

## New screens (this pass)

### User

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `user/[id]` | `screen-user` | `text-safety-number`, `btn-verify` (mark/unmark verified), `btn-block` / `btn-unblock` (label switches on state), `btn-message` (start DM) |

### Self

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `self/preferences` | `screen-self-preferences` | `btn-back`; accent: `chip-accent-amber`, `chip-accent-citron`, `chip-accent-mint`, `chip-accent-glass`, `chip-accent-lilac`, `chip-accent-rust`; behavior toggles: `toggle-show-inline-timestamps`, `toggle-show-member-avatars`, `toggle-mark-verified-peers`, `toggle-read-receipts`, `toggle-reduce-motion`; `toggle-notifications`; language (#1074): `pref-language` (section), `pref-language-heading` (the heading Text itself; sentence case, e.g. en `"Language"`, es `"Idioma"`, from `settings:language.heading`), `chip-language-<code>` (`en`, `es`, `uk`, `fr`, `ru`, `zh`, `ar`), `pref-language-restart` (shown until a relaunch applies an LTR↔RTL switch) |
| `self/user-settings` | `screen-self-user-settings` | `input-display-name`, `input-handle`, `text-handle-invalid`, `input-email` (read-only), `btn-change-email`, `btn-save`, `btn-cancel` |
| `self/security` | `screen-self-security` | enrollment: `input-approval-code-<requestId>` (the code the approver TYPES off the new device — the card displays none, #1096), `btn-approve-<requestId>` (disabled until eight characters are entered), `btn-reject-<requestId>`; devices: `row-device-<deviceId>`, `btn-revoke-device-<deviceId>`; `row-blocked-users` (nav to blocked list); export (#856): `row-export-archive`, `text-export-summary`, `text-export-files`, `btn-export-fetch` (only when attachments are missing), `text-export-fetched`, `btn-export-share`, `text-export-error` (the section sits near the bottom, so `scrollUntilVisible` to the summary/share after tapping); auto-lock `chip-autolock-off` / `-1` / `-5` / `-15` / `-60`, `row-lock-now`; `row-link-device`; `text-identity-key`; `row-security-event-<id>`, `btn-show-older-events`; `row-delete-account`; `btn-sign-out` (confirms first — see Shared) |
| `self/blocked` | `screen-self-blocked` | `row-blocked-<id>`, `btn-unblock-<id>` |
| `self/change-email` | `screen-self-change-email` | `input-email` (enter-email stage), `input-otp` + `input-current-otp` (enter-code stage — two codes since #1161: one to the new address, one to the address being left), `btn-request-otp` (enter-email stage) / `btn-submit` (enter-code stage, enabled only once BOTH codes are six digits), `btn-use-different-email` (in the body, under the code fields). A complete first code moves focus to `input-current-otp` on its own and scrolls it into view (#1231), so a flow can type the second code without tapping the field. |
| `self/saved` | `screen-saved` | `list-saved`, `row-saved-<msgId>`, `btn-unsave-<msgId>`, `saved-unavailable-<msgId>`, `saved-unresolved-notice` |
| `self/link-device` | `screen-link-device` | PIN first (`btn-pin-<digit>`), then `chip-link-qr` / `chip-link-code`, `link-device-qr`, `link-device-payload`, `link-device-copy`, `link-device-status`, `link-device-showing`, `link-device-claimed`, `link-device-approve-card`, `btn-link-device-approve`, `btn-link-device-reject`, `link-device-done`, `btn-link-device-done`, `link-device-pin-error` |
| `self/delete-account` | `screen-self-delete-account` | `input-delete-confirm`, `btn-delete-account`, `text-delete-error` |

### Direct messages

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `dm/new` | `screen-dm-new` | `input-user-search`, `btn-user-search`, `row-user-<id>` (single exact-match result; tapping starts the DM) |
| `dm/requests` | `screen-dm-requests` | reached from the Direct tab's `row-dm-requests`; per request `row-request-<id>`, `btn-accept-request-<id>`, `btn-block-request-<id>` |
| `dm/info` | `screen-dm-info` | `row-member-<userId>` (tap → user profile), export (#856, same ids as `self/security`): `row-export-archive` … `btn-export-share`, `btn-leave` (leave conversation). Back is the top `btn-back` (`btn-back-to-conversation` was removed). |
| `conversation/info` | `screen-conversation-info` | `row-member-<userId>`, export (#856): `row-export-archive` … `btn-export-share`. Back is the top `btn-back` (`btn-back-to-conversation` was removed). |

### Groups

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `group/members` | `screen-group-members` | `row-member-<userId>`, `btn-toggle-role-<userId>` (make/remove admin), `btn-remove-member-<userId>` |
| `group/requests` | `screen-group-requests` | `row-request-<id>`, `btn-approve-<id>`, `btn-reject-<id>` |
| `group/settings` | `screen-group-settings` | `input-group-name`, `input-group-description`, `row-channel-<id>`, `btn-delete-channel-<id>`, `row-group-emoji` (→ `group/emoji`), `btn-settings-leave-group` (everyone, two taps), `btn-delete-group` (admins, two taps), `btn-save` |
| `group/emoji` | `screen-group-emoji` | `input-emoji-shortcode`, `btn-upload-emoji`, `row-emoji-<shortcode>`, `btn-remove-emoji-<shortcode>` |
| `group/invite` | `screen-group-invite` | `input-user-search`, `btn-send-invite`; invite links: `chip-expiry-<id>`, `chip-uses-<id>`, `btn-create-invite-link`, then `created-invite-link-url`, `btn-copy-invite-link`, `btn-share-invite-link`; `row-manage-invite-links` (→ `group/invite-links`). No Cancel button (`btn-cancel` was removed) — leave with `btn-back`. |
| `group/invite-links` | `screen-group-invite-links` | `row-invite-link-<id>`, `btn-revoke-invite-link-<id>` |
| `group/discover` | `screen-group-discover` | `input-group-search` (return runs the lookup), `btn-group-search` (Find, in the body under the field), `btn-request-access` (join), `discover-request-error`, `btn-back` |
| `invite/[token]` | `screen-invite-landing` | `invite-landing-message`, `invite-confirm-title`, `btn-invite-join`, `btn-invite-continue` |

## Coverage notes / gaps (vs. the requested selector list)

- `group/settings` ends with `btn-settings-leave-group` (everyone) and the
  admin-only `btn-delete-group`; the group menu sheet keeps its own
  `btn-group-menu` → `btn-leave-group`. No `toggle-*` exists on
  this screen either (group settings are name/description text fields + channel
  management only).
- `self/security` has **no** change-password / PIN-entry buttons — the RECOVERY
  section only points at email sign-in and `row-link-device`. Safety numbers live on `user/[id]`, not here. Enrollment
  approve/reject and device revoke are the load-bearing actions.
- `user/[id]` self-view shows no safety-number card or block/message buttons
  (it's you) — those testIDs only render for a peer.
- `dm/new` returns a single exact-match user (no result list); the one result
  row `row-user-<id>` doubles as the start-DM affordance (no separate
  `btn-start-dm`).
- `group/discover` renders the matched group in a `Card` (not a list row); the
  join affordance is `btn-request-access` (only shown when there is no
  pending/approved/rejected request).
