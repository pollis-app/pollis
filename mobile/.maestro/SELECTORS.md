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
| Active palette (#1447; invisible full-size probes drawn under every `<Screen>`'s content, `components/ThemeProbe.tsx`) | `theme-accent-<rrggbb>` — the accent the screen rendered with (default `theme-accent-fabf5a`, lilac `theme-accent-bda3e0`); `theme-boot-accent-<rrggbb>` — the accent the app was seeded with at load, i.e. the first frame's |
| Tab bar entries (`<TabBar>`) | `tab-groups`, `tab-direct`, `tab-link-device`, `tab-self`; badge `badge-<tab>`, whose number is unread messages PLUS pending items (#1561): `badge-direct` counts pending message requests, `badge-groups` pending group invites, so either shows with nothing unread and clears when the last one is accepted, declined or blocked. The tab's spoken label names each part ("Direct, 2 unread, 1 message request"). `tab-link-device` (the shield, #1550) is a shortcut, not a tab: it pushes `self/link-device` and back returns to the tab it was tapped from. There is no Search tab — search is at the top of `(tabs)/self`. |
| Bottom sheet Close button (top-end of every `SheetOverlay`; `closeTestID`) | `btn-sheet-close` by default (group menu, add group, create channel, emoji picker); the message-actions sheet's Close is `btn-action-cancel` and the channel sheet's is `btn-menu-cancel`, so those long-standing ids still close their sheets |
| Sign-out confirmation | **native Alert, no testID.** `btn-sign-out` (Self tab and `self/security`) opens it; tap the destructive button by its label, `auth:shell.signOutTitle` — en `"Sign out"`, es `"Cerrar sesión"` (Cancel = `common:actions.cancel`). Title: `mobile:self.hub.signOutConfirmTitle` ("Sign out of Pollis on this device?"). |

## Pre-existing screens (instrumented in the first pass)

### Auth

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `(auth)/email` | `screen-auth-email` | `input-email`, `btn-submit-email`, `btn-sign-in-with-device` (→ `(auth)/link`), `text-legal`, `text-session-notice` (why the last session ended — e.g. this device was removed from the account, #1338) |
| `(auth)/link` | `screen-auth-link` | `link-camera`, `btn-link-allow-camera`, `btn-link-toggle-manual`, `input-link-code`, `btn-link-submit`, `link-claiming`, `link-error`; show this phone's own code (#1361): `btn-link-show-code`, `link-offer`, `link-offer-qr`, `link-offer-payload` (the v2 code as text, zero-width spaces between characters), `link-offer-status`, `btn-link-offer-refresh`, `link-offer-error`; the top back chevron is `btn-link-use-email` (back to email sign-in), not `btn-back` |
| `(auth)/otp` | `screen-auth-otp` | `input-otp`, `btn-submit-otp` |
| `(auth)/pin` | `screen-auth-pin` | keypad `btn-pin-0`…`btn-pin-9`, `btn-pin-back`, `btn-pin-signout`; `pin-busy` (spinner + "Unlocking…" / "Saving…" from the fourth digit of an unlock or confirm until the screen navigates or the call fails) |
| `(auth)/initializing` | `screen-auth-initializing` | `btn-continue` |
| _any `<Button loading>`_ | | `<testID>-busy` — the spinner inside a submit button while its call is in flight (e.g. `btn-submit-otp-busy`) |
| `(auth)/emergency-kit` | `screen-auth-emergency-kit` | `btn-copy-recovery-key`, `btn-share-recovery-key`, `toggle-recovery-ack`, `btn-continue` |
| `(auth)/enrollment` | `screen-auth-enrollment` | `btn-enroll-approve-device`, `linked-awaiting-approval` (QR-linked device waiting), `btn-enroll-recovery`, `input-recovery-key`, `btn-submit-recovery`, `btn-enroll-back`, `btn-enroll-cancel` |

### Tabs

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `(tabs)/groups` | `screen-groups` | **No groups yet:** empty state with `btn-create-group`, `btn-join-group` directly (no pill strip, no `btn-add-group`). **With groups:** the group pill strip — `row-group-<groupId>` per group (tapping one switches the panel IN PLACE; it does not push `group/[id]`, so assert `panel-group`, not `screen-group`), then `btn-add-group` ("+", opens `sheet-add-group` holding `btn-create-group` / `btn-join-group`) — and the selected group's `panel-group` (`GroupPanel`, ids under `group/[id]` below). Pending items above the channels: `row-join-requests-<groupId>` + `badge-join-requests-<groupId>` (admins), `row-invite-<id>`, `btn-decline-invite-<id>`, `btn-accept-invite-<id>`. Flows that create/find a group from this tab tap `btn-add-group` with `optional: true` first, which covers both states. Group ids are 26-char ULIDs, so match a pill as `row-group-[0-9A-Z]{26}` — `row-group-.*` also matches the panel's `row-group-members` etc. |
| `(tabs)/direct` | `screen-direct` | `btn-new-dm` (header, always present), `input-dm-search` (only once there are DMs), `row-dm-requests` (only while a request is pending; opens `dm/requests`), `row-dm-<id>` + `unread-<id>`, empty state `direct-empty` + `btn-new-dm-empty`. DM requests are **not** accepted here any more — see `dm/requests`. |
| `(tabs)/self` | `screen-self` | **Search (#1550):** `input-search` under the large title (normal keyboard with a return key, so `hideKeyboard` works). Focusing it or typing swaps the menu below for the results in place (`search-results`, no navigation); `btn-search-cancel` (shown while searching) and `btn-search-clear` (shown while there is text) end the search and bring the menu back, and so does a second tap on `tab-self`. Results: empty state `search-filter-hint` + filter chips `search-filter-from`, `-in`, `-before`, `-after`, `-on`, `-hasattachment`, `-haslink`; results `search-about-results` ("About N results"), `search-sort-relevant`, `search-sort-recent`, `row-message-<id>`, `search-load-more`, `search-corpus-footer`, `search-indexing`, `search-no-results-why`; `row-group-<id>`, `row-channel-<id>`, `row-user-<id>` (account lookup runs on return), `row-page-<id>` (settings quick-jump, e.g. `row-page-security`). **Menu:** `card-self-profile` + `btn-edit-profile` (→ `self/user-settings`), `row-self-user-settings`, `row-self-security`, `row-self-saved`, `row-self-preferences`, `row-self-notifications` / `row-self-language` (→ `self/preferences` scrolled to that section), `row-self-autolock` (→ `self/security` scrolled to auto-lock), `btn-lock-now`, `btn-sign-out` (confirms first — see Shared) |

### Pushed screens

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `chat/[id]` | `screen-chat` | On iPad (regular width) the conversation is the Groups/Direct tab's right pane — same ids, `screen-chat` on the pane, no `btn-back`; info/members/settings/thread/profile pages open inside that pane and their `btn-back` pops it. header: `btn-back`, `btn-pins` (→ `conversation/pins`), `btn-members` (→ `conversation/info`), `btn-chat-menu` (channel: opens the channel sheet; DM: pushes `dm/info`); `list-messages`, `row-message-<id>` (long-press → message sheet; swipe toward the start edge or double-tap → reply bar; label `"<name>: <text>, <status or time>"`, led by `"Reply to <name>: <text>. "` on a reply), `reply-quote-<id>` (the quote line on a reply; tap jumps to the original), `btn-thread-<id>`, `btn-attachment-<attachmentId>` (any attachment in a row: image thumb, video tile, audio/file chip — tap opens `media`; long-press still opens the message sheet), `sensitive-cover-<attachmentId>` (#1568: a covered sensitive image/video — the tile's tap only reveals it) / `btn-sensitive-hide-<attachmentId>` (the top-right eye on a revealed one; covers it again), inside an image thumb `media-image-loading` → `media-image-rendered` (expo-image has drawn the decrypted file) or `media-unavailable` (the "Image unavailable" placeholder — same ids wherever `MediaImage` renders), `pill-reaction-<msgId>-<emoji>`, `receipt-<msgId>`, `btn-retry-message-<id>` / `btn-discard-message-<id>` (#1335: under a message that did not send — Retry re-runs delivery under the same id, Discard throws the unsent copy away; Retry is absent when an attachment upload failed before the core stored the message; `<id>` is the row's id, `pending-…` for a stub); composer `input-composer`, `btn-send`, `btn-attach`, `btn-composer-emoji`, `strip-attachments` (horizontal scroller of pending-attachment cards, #1585; the Vault composer draws the same) / `composer-attachment-<n>` (one card, `<n>` = 0-based strip position; tapping an image or video opens `media` on the picked file, `source=draft`) / `btn-remove-attachment-<n>` (remove, in the card's top-end toolbar `toolbar-attachment-<n>`) / `btn-sensitive-attachment-<n>` (#1568: the eye before remove, images and videos only; `checked` while marked) / `sensitive-cover-composer-<n>` (a marked card's blurhash + crossed-out eye; the card then opens nothing), `list-mention-suggestions` / `row-mention-<username>`, `list-emoji-suggestions` / `row-emoji-<shortcode>`; edit bar `input-edit-composer`, `btn-edit-save`, `btn-edit-cancel`; reply bar `bar-reply`, `btn-reply-dismiss`. **Message sheet:** `btn-react-<i>`, `btn-react-more` (→ emoji picker: `input-emoji-search`, `list-emoji`, `btn-tone-<i>`), `btn-reply`, `btn-reply-thread`, `btn-copy-text`, `btn-copy-link`, `btn-save`, `btn-pin` ("Pin message" / "Unpin message"), `btn-edit`, `btn-delete`, `btn-report`; Close = `btn-action-cancel`. **Channel sheet:** `toggle-channel-mute` (#1431: the per-channel mute switch, `checked` when muted), `btn-menu-info`, `btn-menu-group-settings`; Close = `btn-menu-cancel`. |
| `chat/thread` | `screen-thread` | `list-thread`, `row-thread-root-<id>`, `row-thread-<id>`, `btn-retry-message-<id>` / `btn-discard-message-<id>` (a reply that did not send, as in `chat/[id]`), `btn-attachment-<attachmentId>` (opens `media` over the thread's root + replies) |
| `media` | `screen-media-viewer` | #1248 full-screen attachment viewer (a pushed page, root stack — also on iPad). Close = `btn-media-close` (top-start); `pager-media` (horizontal pager over the conversation's images + videos, oldest first; audio/files open alone), `media-page-<attachmentId>` per page; `btn-media-prev` / `btn-media-next` (only with 2+ items; wrap around like desktop); `btn-media-save` (images/video: photo library, add-only permission asked on first use), `btn-media-share` (any type: share sheet); `text-media-feedback` (saved / denied / failed), `btn-media-open-settings` (after a denial); `text-media-unavailable` (decrypt failed); audio: `btn-media-audio-play`, `btn-media-audio-back`, `btn-media-audio-forward`, `slider-media-audio`, `text-media-audio-clock`. Video uses the platform's native controls (no testIDs inside). |
| `group/[id]` | `screen-group` | Phones only — reached from search group hits and invite links; creating a group lands on the Groups tab instead (`panel-group`), and on iPad (regular width) this route redirects to the Groups tab two-pane. `btn-back` (top back bar), then the same `GroupPanel` the Groups tab shows: `btn-group-menu` (group name; opens `sheet-group-menu`: `btn-menu-members`, `btn-menu-invite`, `btn-menu-group-settings`, `btn-menu-requests` (admins, while requests are pending), `btn-leave-group`), `row-group-invite` (invite icon), `btn-search-group` (opens the search at the top of Self, field focused), `btn-create-channel` (admins; opens `sheet-create-channel`: `input-channel-name`, `btn-create-channel-submit`), `row-channel-<id>` (`row-channel-<id>-muted` while the channel is muted, #1431), `row-group-members`, `row-group-settings`, `row-group-requests` (only while requests are pending). On the Groups tab the panel's root is `panel-group`. |
| `group/new` | `screen-group-new` | `input-group-name`, `input-group-description`, `toggle-general-channel` (opt-in #General, off by default), `btn-submit-group` (submit; dismisses back to the Groups tab with the new group selected — wait for `btn-group-menu` with text `"<name>, .*"`, then `panel-group`). No Cancel button — leave with `btn-back`. |

## New screens (this pass)

### User

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `user/[id]` | `screen-user` | `btn-report-user`, `btn-block` / `btn-unblock` (label switches on state), `btn-message` (start DM) |

### Self

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `self/preferences` | `screen-self-preferences` | `btn-back`; accent: `chip-accent-amber`, `chip-accent-citron`, `chip-accent-mint`, `chip-accent-glass`, `chip-accent-lilac`, `chip-accent-rust`; behavior toggles: `toggle-show-inline-timestamps`, `toggle-show-member-avatars`, `toggle-mark-verified-peers`, `toggle-read-receipts`, `toggle-reduce-motion`; `toggle-notifications`; language (#1074): `pref-language` (section), `pref-language-heading` (the heading Text itself; sentence case, e.g. en `"Language"`, es `"Idioma"`, from `settings:language.heading`), `chip-language-<code>` (`en`, `es`, `uk`, `fr`, `ru`, `zh`, `ar`), `pref-language-restart` (shown until a relaunch applies an LTR↔RTL switch) |
| `self/user-settings` | `screen-self-user-settings` | `input-display-name`, `input-handle`, `text-handle-invalid`, `input-email` (read-only), `btn-change-email`, `btn-save`, `btn-cancel` |
| `approve-device` | `screen-approve-device` | #1360, opened by itself over any signed-in screen when a new device asks to be approved: `input-approve-device-code` (typed off the new device; nothing on screen shows it), `btn-approve-device` (disabled until eight characters), `btn-reject-device`, `text-approve-device-error`; `btn-back` leaves it listed under Security |
| `self/security` | `screen-self-security` | enrollment: `input-approval-code-<requestId>` (the code the approver TYPES off the new device — the card displays none, #1096), `btn-approve-<requestId>` (disabled until eight characters are entered), `btn-reject-<requestId>`; devices: `row-device-<deviceId>`, `btn-revoke-device-<deviceId>`; `row-blocked-users` (nav to blocked list); export (#856): `row-export-archive`, `text-export-summary`, `text-export-files`, `btn-export-fetch` (only when attachments are missing), `text-export-fetched`, `btn-export-share`, `text-export-error` (the section sits near the bottom, so `scrollUntilVisible` to the summary/share after tapping); auto-lock `chip-autolock-off` / `-1` / `-5` / `-15` / `-60`, `row-lock-now`; `row-link-device`; `row-security-event-<id>` (the newest three only, #1548), `row-security-events-all` (→ `self/security-events`); `row-delete-account`; `btn-sign-out` (confirms first — see Shared) |
| `self/security-events` | `screen-self-security-events` | #1548 the whole log: filter chips `chip-security-events-all` / `-devices` / `-identity` / `-account`; `list-security-events` of `row-security-event-<id>` (the next page loads on scroll), `security-events-loading-more`, `security-events-empty` |
| `self/blocked` | `screen-self-blocked` | `row-blocked-<id>`, `btn-unblock-<id>` |
| `self/change-email` | `screen-self-change-email` | `input-email` (enter-email stage), `input-otp` + `input-current-otp` (enter-code stage — two codes since #1161: one to the new address, one to the address being left), `btn-request-otp` (enter-email stage) / `btn-submit` (enter-code stage, enabled only once BOTH codes are six digits), `btn-use-different-email` (in the body, under the code fields). A complete first code moves focus to `input-current-otp` on its own and scrolls it into view (#1231), so a flow can type the second code without tapping the field. |
| `self/saved` | `screen-saved` | `list-saved`, `row-saved-<msgId>`, `btn-unsave-<msgId>`, `saved-unavailable-<msgId>`, `saved-unresolved-notice` |
| `self/link-device` | `screen-link-device` | Scan to sign in a device (#1361). Reached from `row-link-device` (Self → Security) and from the tab bar shield `tab-link-device` (#1550; back is then plain "Back"): `scan-device-camera`, `btn-scan-allow-camera`, `btn-scan-toggle-manual`, `input-scan-code`, `btn-scan-code-continue`, `scan-device-error` (a v1 code scanned the wrong way round); then the PIN (`scan-device-confirm`, `btn-pin-<digit>`, `link-device-pin-error`); `link-device-linking`, `link-device-done`, `btn-link-device-done`; `link-device-failed`, `btn-scan-again` |
| `self/delete-account` | `screen-self-delete-account` | `input-delete-confirm`, `btn-delete-account`, `text-delete-error` |

### Direct messages

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `dm/new` | `screen-dm-new` | `input-user-search`, `btn-user-search`, `row-user-<id>` (single exact-match result; tapping starts the DM) |
| `dm/requests` | `screen-dm-requests` | reached from the Direct tab's `row-dm-requests`; per request `row-request-<id>`, `btn-accept-request-<id>`, `btn-block-request-<id>` |
| `dm/info` | `screen-dm-info` | `row-member-<userId>` (tap → user profile), `row-verify-safety-<userId>` (#1563: one per other member — "Verify safety number" in a 1:1, "You & @name" in a group DM; also on `conversation/info` for a DM; → `dm/safety`), export (#856, same ids as `self/security`): `row-export-archive` … `btn-export-share`, `btn-leave` (leave conversation). Back is the top `btn-back` (`btn-back-to-conversation` was removed). |
| `dm/safety` | `screen-dm-safety` | "You & @name" (#1563), params `{ peerId, name }`; a pane page on iPad. `text-safety-number` (60 digits in 5-digit blocks), `img-safety-qr` (QR of the digits), `btn-verify` (mark/unmark verified), `text-safety-shared` |
| `conversation/info` | `screen-conversation-info` | `row-member-<userId>`, media grid `btn-media-tile-<attachmentId>` (opens `media`), export (#856): `row-export-archive` … `btn-export-share`. Back is the top `btn-back` (`btn-back-to-conversation` was removed). |
| `conversation/pins` | `screen-pins` | #1315 the conversation's pinned messages (iPad: inside the detail pane). `list-pins`, `row-pin-<msgId>` (tap → back to the conversation scrolled to the message; label `"<author>, <when>: <text>"`), `btn-unpin-<msgId>`; `pins-empty`, `pins-unavailable` (pin key not on this device yet), `pins-loading`, `pins-unresolved-notice` (the message is not on this device). |

### Groups

| Route | `screen-*` | Key testIDs |
| --- | --- | --- |
| `group/members` | `screen-group-members` | `row-member-<userId>`, `btn-toggle-role-<userId>` (make/remove admin), `btn-remove-member-<userId>`; pending invites (#1506, only while there are any): `section-invited`, `row-invited-<inviteeUserId>`, `btn-revoke-invite-<inviteeUserId>` (the inviter and admins only) |
| `group/requests` | `screen-group-requests` | `row-request-<id>`, `btn-approve-<id>`, `btn-reject-<id>` |
| `group/settings` | `screen-group-settings` | Admins: `input-group-name`, `input-group-description`, `btn-save`. Non-admins (#1281): `value-group-name` (text, not an input), `value-group-description` only when the group has a description — with none there is no Description field at all; no `btn-save`. Everyone: `row-channel-<id>`, `btn-delete-channel-<id>`, `row-group-emoji` (→ `group/emoji`), `btn-settings-leave-group` (everyone, two taps), `btn-delete-group` (admins, two taps) |
| `group/emoji` | `screen-group-emoji` | `input-emoji-shortcode`, `btn-upload-emoji` (every member, #1280), `row-emoji-<shortcode>`, `btn-remove-emoji-<shortcode>` (admins only; two taps), `hint-emoji-admins-remove` (non-admins, when the group has emoji) |
| `group/invite` | `screen-group-invite` | `input-user-search`, `btn-send-invite`; invite links: `chip-expiry-<id>`, `chip-uses-<id>`, `btn-create-invite-link`, then `created-invite-link-url`, `btn-copy-invite-link`, `btn-share-invite-link`; `row-manage-invite-links` (→ `group/invite-links`). No Cancel button (`btn-cancel` was removed) — leave with `btn-back`. |
| `group/invite-links` | `screen-group-invite-links` | `row-invite-link-<id>`, `btn-revoke-invite-link-<id>` |
| `group/discover` | `screen-group-discover` | `input-group-search` (return runs the lookup), `btn-group-search` (Find, in the body under the field), `btn-request-access` (join), `discover-request-error`, `btn-back` |
| `vault` | `screen-vault` | Entry point `row-vault` (top of `screen-direct`). `chip-vault-view-chat` / `chip-vault-view-media`, `input-vault-search`, `list-vault`, `vault-empty`, `row-vault-<id>` (with `vault-pinned-<id>` and `text-vault-<id>` inside; one a11y node: "<text>, <time>[, Pinned][, (edited)]" — match entries by that label), composer `input-composer` / `btn-attach` / `btn-send`; long press → `sheet-vault-entry` with `btn-vault-pin`, `btn-vault-edit` (→ `input-edit-composer`, `btn-edit-save`), `btn-vault-delete` (→ `vault-delete-bar`, `btn-vault-delete-confirm`, `btn-vault-delete-cancel`); media view `vault-media`, `vault-media-empty`, `tile-vault-media-<key>` (→ `screen-media-viewer`) |
| `invite/[token]` | `screen-invite-landing` | `invite-landing-message`, `invite-confirm-title`, `btn-invite-join`, `btn-invite-continue` |

## Coverage notes / gaps (vs. the requested selector list)

- `group/settings` ends with `btn-settings-leave-group` (everyone) and the
  admin-only `btn-delete-group`; the group menu sheet keeps its own
  `btn-group-menu` → `btn-leave-group`. No `toggle-*` exists on
  this screen either (group settings are name/description text fields + channel
  management only).
- `self/security` has **no** change-password / PIN-entry buttons, and no
  Recovery section (#1547 removed it: a new device signs in by email or through
  `row-link-device`). Safety numbers live in the DM (`dm/safety`, #1563), not here. Enrollment
  approve/reject and device revoke are the load-bearing actions.
- `user/[id]` shows no safety number (#1563 moved it to `dm/safety`); the
  self-view shows no block/message buttons either (it's you).
- `dm/new` returns a single exact-match user (no result list); the one result
  row `row-user-<id>` doubles as the start-DM affordance (no separate
  `btn-start-dm`).
- `group/discover` renders the matched group in a `Card` (not a list row); the
  join affordance is `btn-request-access` (only shown when there is no
  pending/approved/rejected request).
