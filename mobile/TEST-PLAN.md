# Mobile — committed test & parity plan

Canonical, **committed** scope for taking the Pollis mobile app (Expo/RN) from
"feature-wired" to "verified across iOS, iPadOS, and Android." This file is the
scope lock: the work below gets finished as a set, not trimmed at the tail. If
something here becomes out of scope, edit this file and say why — don't let it
drift silently.

Locked 2026-07-23. Tracking epic: **#342**. Stories: **#619 #620 #621 #622 #623**.

## Where the app already is

The app is **~95% feature-wired**. It is a standalone Expo app (`mobile/`, SDK 55
/ RN 0.83) that links `pollis-core` through a uniffi JSI turbo-module
(`modules/pollis-native`) — the **same** command surface (`pollis-core/src/bridge.rs`,
~82 commands) the desktop Tauri app uses. Every screen consumes real bridge
commands: auth (email → OTP → PIN → initialize → device enrollment → recovery),
groups/channels CRUD, DMs + requests, messaging (send/edit/delete/reactions,
optimistic + ingest-on-focus), search (message FTS + user lookup), profile,
preferences (incl. runtime accent theming), device management, blocking, safety
numbers. Keystore-at-rest is done on both platforms (AndroidKeyStore /
iOS Keychain KEK). The credential service (#393) is done (Turso RO token, LiveKit
token, R2 presign all DS-brokered — no secrets in the bundle). Realtime (LiveKit
**data-only**) and push are wired.

**Verified in-box already:** `pollis-core` cross-compiles for iOS + Android
(`mobile-core-check.yml`), and the DS credential broker is deployed. The residual
risk is therefore **UI/visual + on-device behavior**, which is exactly what the
Maestro screenshot harness targets.

## Locked decisions (2026-07-23)

| Decision | Choice |
|---|---|
| E2E / UI test framework | **Maestro** (YAML flows, built-in screenshots, one flow across iOS/iPad/Android) |
| Execution environment | **Local Mac** (macOS Tahoe / Xcode 26) driving simulators + Android emulator. No CI gate for now; flows authored CI-portable. |
| iPadOS | **Full adaptive layouts** — true universal parity, not a scaled-up iPhone UI |
| Media on mobile | **None.** No audio/video/screenshare. LiveKit is **data-only** (realtime events). Voice libs stay installed-but-inactive; no mic/camera-for-voice permissions. |

## The two-track split

Because this box is headless Linux (no iOS at all, and it must not run emulators),
work is split by where it can happen:

- **Static — doable in-box now:** testID/a11y instrumentation (#620), iPad
  adaptive layout code (#622), residual code gaps (#623), and **authoring** all
  Maestro flow YAML (#619/#621). All TypeScript — verifiable by `pnpm tsc` +
  review.
- **Mac — the visual evaluator later:** booting simulators/emulator, installing
  dev builds, **running** the flows, capturing screenshots, and triaging them for
  defects; iOS keystore on-device verification; push on-device delivery.

## Scope (the committed set)

### 1. Test harness — #619
Maestro harness under `mobile/.maestro/` (flows, subflows, `config.yaml`), a
`mobile/scripts/maestro-run.sh <flow> <ios|ipad|android>` runner that boots the
right device, installs the dev build (`com.pollis.mobile`), runs the flow, and
drops screenshots into `mobile/.maestro/artifacts/<date>/<platform>/`. Deterministic
seed against the **dev DS** (`DEV_OTP` bypass, fixed PIN, pre-seeded peers
Alice/Bob) so DM/group/reaction flows reproduce. `README.md` runbook.

### 2. Instrumentation — #620
Add stable `testID` + `accessibilityLabel` to the `components/ui.tsx` primitives
and every element the flow matrix touches; document the naming scheme in
`.maestro/SELECTORS.md`. The app has **zero** testIDs today, so this is a hard
prerequisite for non-brittle flows.

### 3. Parity flow matrix — #621
One discrete, independently-runnable Maestro flow per non-media feature, each
capturing **iPhone + iPad + Android** screenshots:

`auth` · `auth-restore` (keystore unlock) · `enrollment` (+ recovery) · `groups` ·
`group-members` · `dms` · `messaging` (edit/delete/reactions/reply) · `search` ·
`profile-prefs` (+ accent + change-email) · `security` (devices/safety-numbers) ·
`blocking` · `realtime` (two-client live) · `push-tap` · `invite-links` (#847
parity: mint on the invite screen with expiry/uses presets, one-time card with
verified copy + share sheet, list/revoke on `group/invite-links`, and
`pollis://invite/<token>` deep-link redemption on `app/invite/[token]` —
including a cold-start tap, which expo-router routes via the initial URL; the
`https://pollis.com/invite/<token>` form additionally needs universal/app-links
config, deliberately not yet added).

The `security` flow additionally covers the in-app account-deletion entry
(`row-delete-account` → typed-DELETE confirm screen, `btn-delete-account`
disabled until armed) — assert the disabled state only; never run the actual
deletion against a shared seed account. It also covers auto-lock (#899):
select a window via `chip-autolock-1`, tap `row-lock-now`, assert the PIN
unlock screen appears, unlock with the fixed PIN, and assert the app restores
to Security. The screen further surfaces the security-event audit list
(`row-security-event-*`, `btn-show-older-events`) and the self public-key
line (`text-identity-key`) — assert presence, not contents.

Media flows are **excluded by decision**.

### 4. iPadOS adaptive layouts — #622
A `useLayoutClass()` breakpoint hook; **two-pane** (list + detail) on regular
width mirroring desktop's sidebar+content; centered max-width columns elsewhere;
landscape + Split View graceful; iPad screenshots for App Store (#340). iPad
Maestro flows assert the adaptive layout, not a stretched phone.

### 5. Residual parity gaps — #623
Wire `dm/info` actions; handle the realtime events currently decoded-but-ignored
(`all_mention`, `member_role_changed`, `roster_changed`); ~~`device_revoked`
self-sign-out on the inbox connection~~ (done — authoritative
`is_current_device_registered` check, sign-out, and navigation to the auth
screen); purge stale "not implemented" comments; add iOS keystore on-device
verification to the checklist.

**Landed since lock (inbox-parity set):** `dm/info` actions, the
decoded-but-ignored realtime events, and `device_revoked` self-sign-out are all
wired. The inbox realtime now also holds every group/DM room (ref-counted with
the open chat's subscription) to drive real unread counts (list-row dots +
tab-bar badges, cleared on open), batched last-message previews + timestamps on
the Groups/Direct tabs (`read_last_messages`, one call per list), DM-request
Block-beside-Accept (desktop's decline path), search hits carrying their real
conversation kind, role-then-alphabetical member ordering (presence ordering
awaits a presence source), a `conversation/info` screen (roster + shared-media
grid, capped 30), and the real core version on the initializing screen. Flows
touching the Groups/Direct rows should assert previews/unread via
`unread-<id>` / `badge-<tab>` testIDs.

## Explicit non-goals (and why)

- **Audio / video / screenshare / voice UI** — product decision: mobile has no
  media. (Desktop keeps its Rust media pipeline; mobile keeps LiveKit data-only.)
- **Terminal pane, keyboard-shortcuts page, in-app auto-updater** — desktop-only;
  mobile updates via the stores.
- **CI execution of the flows** — deferred; local-Mac only for now. Flows are
  authored so a macos CI runner could adopt them later without a rewrite.

## Distribution (separate track)

App Store / Play submission stays under epic **#339** → **#340** (iOS/iPadOS,
incl. the non-exempt-encryption export-compliance declaration) and **#341**
(Google Play). Push credentials (APNs key / FCM v1 service account) + EAS
`projectId` are operational items under those.

## Definition of done (the whole set)

- `.maestro/` harness + runner + seed subflows + README exist (#619).
- Every element in the flow matrix has a stable `testID`; `SELECTORS.md` documents
  the scheme; `pnpm tsc` clean (#620).
- All matrix flows authored and, on the Mac, green across iPhone/iPad/Android with
  artifact galleries reviewed and no P0/P1 visual defects (#621).
- iPad renders true adaptive layouts, survives rotation + Split View, passes iPad
  flows (#622); satisfies #340's "genuine iPad layouts" criterion.
- All residual gaps in #623 closed, each confirmed by a matching flow or on-device
  check.
