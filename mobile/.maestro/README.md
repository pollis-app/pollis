# Pollis mobile — Maestro e2e + visual suite (#619 / #621)

Automated flows that drive the app on **iOS Simulator, iPad Simulator, and
Android emulator**, capturing a **screenshot gallery** at each meaningful state
so a human/visual evaluator can confirm quality or spot defects across all three
form factors.

> **Runs on a local Mac only.** This suite was *authored* in a headless Linux
> box that cannot run Maestro, simulators, or emulators — so the flows are
> written best-effort against the real `testID`s (see `SELECTORS.md`) and need a
> first-run shakedown on the Mac (timing waits, and the small `testID` gaps noted
> below). Decision log: Maestro / local-Mac / full adaptive iPad, in
> `mobile/TEST-PLAN.md`.

## Prerequisites (Mac)

1. **Maestro** — `curl -Ls "https://get.maestro.mobile.dev" | bash`.
1b. **A Java runtime.** Maestro is a JVM tool and dies with "Unable to locate a
   Java Runtime" without one. `brew install openjdk@17` is keg-only, so
   `/usr/libexec/java_home` cannot see it and installing alone is not enough —
   export it:
   ```bash
   export JAVA_HOME=/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home
   ```
2. **A RELEASE build of the app, pointed at the dev DS.** Both halves matter.

   *Release, not dev-client.* Every flow opens with `clearState`, which wipes
   expo-dev-client's stored dev-server URL — so a Debug (dev-client) build
   boots to the **dev-client launcher menu** and the app never loads. The suite
   cannot drive that build at all. A Release build embeds the JS bundle and has
   no launcher. It is also the more honest target: these flows are
   timing-sensitive and dev-mode JS is not what ships.
   Xcode 27 ships no Simulator.app, so `expo run:ios` fails (`Can't determine
   id of Simulator app`); build and install with xcodebuild/simctl instead
   (full sequence in `mobile/CLAUDE.md` → "Dev loop — iOS"):
   ```bash
   cd mobile
   xcrun simctl boot <simulator-udid>
   xcodebuild -workspace ios/Pollis.xcworkspace -scheme Pollis \
     -configuration Release -destination id=<simulator-udid> \
     -derivedDataPath ios/build build
   xcrun simctl install <simulator-udid> \
     ios/build/Build/Products/Release-iphonesimulator/Pollis.app
   ```
   Address the simulator by **UDID** (`xcrun simctl list devices available`)
   and boot it first; a name that matches no *booted* simulator makes xcodebuild
   fall through to a physical-device destination and fail.

   *Dev DS.* The build must point at the dev Delivery Service (baked at build
   time, not Maestro env):
   ```
   EXPO_PUBLIC_POLLIS_DELIVERY_URL=https://api-dev.pollis.com
   EXPO_PUBLIC_LIVEKIT_URL=wss://<dev-livekit>
   # No database credential is needed or read since #987 — the client speaks
   # only to the Delivery Service.
   ```
   See `mobile/CLAUDE.md` for the ubrn/native steps. App id: `com.pollis.mobile`.

   *After changing the ubrn target set* (e.g. building simulator-only slices),
   re-run `pod install`: CocoaPods bakes the xcframework's slice paths into a
   generated copy script, so a stale script silently copies **nothing** and the
   app dies at launch on a uniffi checksum mismatch.
3. **Seed env** — `cp .maestro/env.example .maestro/.env` and fill it in. The dev
   DS must have a fixed `DEV_OTP` so `MAESTRO_OTP` is deterministic (no inbox
   polling). `.env` is gitignored.

## Run

```bash
# one flow on the iPhone simulator
mobile/scripts/maestro-run.sh auth ios
# the whole suite on the iPad simulator (exercises the #622 two-pane)
mobile/scripts/maestro-run.sh all ipad
# on Android
mobile/scripts/maestro-run.sh messaging android
```
Screenshots land in `mobile/.maestro/artifacts/<date>/<platform>/` — that's the
gallery the visual evaluator reviews. Override device names with `IOS_DEVICE=…`,
`IPAD_DEVICE=…`, `ANDROID_AVD=…`. The defaults are Xcode 27's `iPhone 18 Pro`
and `iPad Pro 13-inch (M5)`, and the `pollis_e2e` AVD (Pixel 8, API 36
`google_apis` arm64 — see `mobile/CLAUDE.md` for creating it). Simulator names change with each Xcode, so the
script resolves the name among *available* simulators and, if none matches,
exits printing the ones that exist rather than running with no `--device` at
all:
```bash
IOS_DEVICE="iPhone 17 Pro" mobile/scripts/maestro-run.sh auth ios   # an Xcode 26 machine
```

## Flow matrix (`flows/`)

| Flow | What it proves | Peer? |
| --- | --- | --- |
| `auth` | email→OTP→PIN→inbox (also the smoke test: bridge + dev DS + keystore live) | no |
| `groups` | create group, open, channel visible | no |
| `messaging` | send / edit / delete / react in a self-owned channel | no |
| `outbox` | #1335 a message whose send is killed mid-flight (`stopApp` right after Send in a fresh group) is still there after relaunch + unlock and is delivered by the outbox drain (no `btn-retry-message-*` left); Android only: a send made in airplane mode is kept as failed with `btn-retry-message-<id>` / `btn-discard-message-<id>`, and back online Retry settles it | no |
| `reply` | #1270 swipe a message toward the start edge → reply bar (not the message sheet), send, the row quotes the original; double tap does the same; dismiss cancels; tapping a quote jumps to the original | no |
| `channel-menu` | #1193 the channel kebab sheet: every item tapped by `testID`, plus `channel-menu-01-open` — the gallery shot that must show an OPAQUE sheet (no header/composer through its buttons; the invariant itself is pinned in `tests/sheet-opaque.test.ts`) | no |
| `profile-prefs` | accent re-theme, behavior toggle, display-name save | no |
| `search` | #1202 desktop parity: a seeded message hit with highlighted snippet + "About N results", sort toggle, corpus footer, opening a hit, settings-page quick-jump, and the "why no results" explanation | no |
| `security` | device list + blocked-list entry | no |
| `media-viewer` | #1248 send an image through the SYSTEM photo picker (seeded by `addMedia` from `fixtures/`, cell picked by its accessibility label), cold-restart, and require the image to render from R2 (`media-image-rendered`, never `media-unavailable`); then the full-screen viewer: double-tap zoom, Save to photos with feedback, Share reachable, Close. Every step is mandatory | no |
| `vault` | #1316 Direct → Vault: empty state, save two entries, pin, edit (pin survives), search filters, media view, a best-effort image entry opening the viewer from the grid, delete asks first (cancel keeps it, confirm removes it), re-open re-syncs from the DS | no |
| `export` | #856 on-device archive from Security: summary renders, share button reachable, no network offer on an attachment-free account | no |
| `ipad-two-pane` | #622 list+detail side-by-side (run on **iPad**) | no |
| `dms` | start a DM with the seeded peer (initiator side) | yes |
| `deep-link` | #1223 a `pollis://` link routes both warm (scene `openURLContexts`) and cold (scene `connectionOptions` → `Linking.getInitialURL`) under the UIScene life cycle | no |
| `i18n` | switch language (copy re-renders), persists across relaunch, Arabic mirrors the layout after relaunch (#1074) | no |
| `enrollment-approval` | #1096 the approver TYPES the code off the new device — the card shows none, approve is gated on eight characters | yes |

Two-client / special flows (`enrollment-approval`, `realtime`, `blocking`,
`push-tap`, and the DM accept/reply side) are scaffolded in `_two-client.md` —
they need the two-device setup below and a Mac shakedown.

## Screen tour (visual audit)

`flows/tour.yaml` (tag `audit`) signs up, makes a group with a message, and
takes one numbered screenshot of every reachable screen (`tour-01-…` to
`tour-33-…`), so a layout or copy change can be compared before and after. It
asserts nothing; collect the shots with `--debug-output <dir>`.

## Visual before/after comparison

For a re-skin or any layout change, shoot the tour on the old build and the new
one, then pair them up:

```bash
# 1. "before": the tour on the pre-change Release build (kept, gitignored, at
#    artifacts/baseline-before/{ios,android}/ — 34 shots each, 2026-10-04/05)
# 2. build + install the working tree, then shoot "after"
mobile/scripts/build-release-sims.sh both            # or ios | android
mobile/scripts/maestro-run.sh tour ios               # -> artifacts/<date>/ios/tour-*.png
mobile/scripts/maestro-run.sh tour android
# 3. compare
NAME=ios mobile/scripts/visual-compare.sh \
  mobile/.maestro/artifacts/baseline-before/ios mobile/.maestro/artifacts/<date>/ios
open mobile/.maestro/artifacts/compare/<run>/index.html
```

`visual-compare.sh` pairs `tour-NN-*.png` by name (falling back to the `NN`
number when a slug was renamed) and writes, under the gitignored
`artifacts/compare/<timestamp>[-NAME]/`: `side/<name>.png` — one labelled
BEFORE | AFTER image per pair (composited by a small Swift helper, so nothing
to install; one image per screen is also what a reviewing agent can open);
`index.html` — the contact sheet, every pair side by side with a
changed / identical / only-before / only-after tag and a "hide identical"
toggle; `summary.txt`. The tag is a byte comparison, not a perceptual diff:
the status-bar clock and each run's fresh signup handle make practically every
shot "changed" even between two runs of the same build, so the review is by
eye. Set `PREFIX=` to compare a gallery other than the tour.
Screenshots are never committed — `artifacts/` is gitignored.

## iOS driver under Xcode 27

Maestro 2.11.0's iOS driver never comes up under Xcode 27 ("iOS driver not
ready in time", on every simulator, whatever `MAESTRO_DRIVER_STARTUP_TIMEOUT`
is): its bundle runs a unit-test class before the HTTP-server test, and Xcode
27's XCTest then blocks after the first test in
`XCTCrashLogTracker.waitForPendingCrashlogs()`, so the server never starts.
`mobile/scripts/maestro-ios-driver-fix.sh` skips that class in the driver's
`.xctestrun` inside `~/.maestro/lib/maestro-ios-driver.jar` (backup kept as
`.orig`, `--revert` restores it). `maestro-run.sh` applies it on every iOS run,
so a Maestro reinstall heals itself; drop both once Maestro ships the fix.

## Two-client flows

**Join requests (automated):** `mobile/scripts/maestro-join-request.sh <admin-device> <peer-device>`
— the admin creates a group and opens it from the Groups tab's group pill
strip; the peer finds it by slug (Groups "+" → Find group, one lookup) and
requests access; the admin sees the request on the Groups tab (pending row +
badge, above the channels) and approves it; the peer then lists the group.
Step 5 (`5-peer-settings.yaml`, #1281) opens Group settings as that plain
member: the name is text (`value-group-name`), and with no description there
is no Description field at all — no inputs, no Save. Step 1 checks the
admin's own settings are still the editable form.

**Group emoji permissions (automated, #1280):** `mobile/scripts/maestro-group-emoji.sh <admin-device> <peer-device>`
— the join-request steps make the peer a plain member of the admin's group; the
peer then adds a custom emoji (system photo picker, seeded by `addMedia`) and
must see no remove control for it, not even as its creator; the admin then sees
the remove control and removes it.

**Report (automated, #1213):** `mobile/scripts/maestro-report.sh <reporter-device> <peer-device>`
— a fresh peer signs up, its handle is read off the screen, and the reporter
reports it with "Report and block", then unblocks.

**Lost message (automated, #1312):** `mobile/scripts/maestro-lost-message.sh <device>`
— one device plus a dev-DS peer client, not a second device: the script runs
`pollis-tui`'s `lost_message_peer` example (a `test-harness` build that never
ships), which signs up a fresh peer and prints its handle. The device signs up,
opens a DM with it (`two-client/lost-message/1-open-dm.yaml`); the peer accepts
and sends four messages with the middle two corrupted; then the device
re-opens the DM and must show the readable two with ONE collapsed "2 messages
couldn't be decrypted" placeholder between them, whose link opens Diagnostics
(`2-see-placeholder.yaml`). The dev gate
key is read from Doppler `dev_personal` when `POLLIS_DEV_ACCESS_KEY` is unset.

**Device revoked while closed (automated, #1338):** `mobile/scripts/maestro-device-revoked.sh <device>`
— the `revoke_peer` example signs up a fresh account as its first device and
prints the recovery key; `two-client/device-revoked/1-enroll.yaml` joins that
account on the device with the key and closes the app; the peer revokes the
device; `2-revoked.yaml` reopens and unlocks and must land on sign-in with
`text-session-notice` ("removed from your account").

**A group reaches the user's other device, both ways (automated):** `mobile/scripts/maestro-own-groups.sh <device>`
— `pollis-tui`'s `own_groups_peer` example signs up a fresh account as its
first device (the stand-in for the user's desktop) and prints the recovery
key; `two-client/own-groups/1-enroll.yaml` joins that account on the device.
`2-phone-creates.yaml` creates a group with #General — which must show on the
phone with another device in the account — and posts in it; the peer must list
the group and read the post (the create admitted its device). The peer then
creates a group, and `3-peer-group-live.yaml`, which never touches the phone,
must see its pill appear: only `create_group`'s inbox wake-up can do that.

**A new device's approval request comes to the phone (automated, #1360):**
`mobile/scripts/maestro-approval-prompt.sh <device>` — `pollis-core`'s
`enroll_peer` example plays the desktop on the desktop's own core commands
(built `--no-default-features`, so it never touches the macOS Keychain). The
phone signs up and sits on the Groups tab; three peer "desktops" then each
press "Approve from another device" — while the app is open (the DS's live
nudge must open `screen-approve-device`), while it is closed (it must appear
after reopen + unlock) and while it is backgrounded (it must appear on resume;
this one is rejected and the peer must see the rejection). No step opens
Security. Then the other way: a peer signs up a second account, the phone
signs in to it as a new device, its code is read off the screen and handed to
the peer, which approves with `approve_device_enrollment`, and the phone must
land in the app. `maestro-device-link.sh`'s step 3 likewise waits for the
approval screen to open by itself.

**Pins across members (automated, #1315):** `mobile/scripts/maestro-pins.sh <device>`
— the lost-message shape with `pollis-tui`'s `pins_peer` example: the device
opens a DM with a fresh peer (`two-client/pins/1-open-dm.yaml`), the peer sends
a message and pins it, then the device must list the peer's pin, offer "Unpin
message" on it, pin its own message and unpin the peer's
(`2-pin-across-members.yaml`); the script passes only when the peer has seen
the device's pin and unpin in its own list. The one-device half (pin, list,
jump, unpin) is `flows/pins.yaml`.

**QR sign-in, phone to phone (automated, #1361):** `mobile/scripts/maestro-qr-link.sh <signed-in-device> <new-device>`
— the NEW device shows its code ("Sign in with another device" → "Show a code
instead", `two-client/qr-link/2-new-show-code.yaml`), the script reads it off
that device's UI tree, the signed-in device scans it from Security → Scan to
sign in a device (entered as text through the camera's own handler: a simulator
camera can't be aimed at another screen) and confirms with its PIN, and the new
device must finish its PIN and list the group — no Approve, no code typed on
it. Needs a dev DS with the v2 link endpoints.

**QR sign-in, phone and desktop (automated, #1361 + #1207):** `mobile/scripts/maestro-qr-scan.sh <device>`
— `enroll_peer` plays the desktop. First the desktop is not signed in and
offers its code (`offer` mode); the phone scans it and the peer must enroll.
Then the other way: the peer signs up and shows a v1 code (`show` mode), the
phone signs in with it as a new device (`v1-1-…`, `v1-2-…`), and the peer
approves.

**Device link (automated):** `mobile/scripts/maestro-device-link.sh <existing-device> <new-device>`
runs both halves of device-linking sign-in across two devices
(`.maestro/two-client/device-link/`): the existing device signs up and makes
a group, the new device signs in and shows its code, the script reads it off
the new device's UI tree, the existing device approves, and the new device
must create its PIN, finalize, and list the group. Any pair works
(iOS sim + Android emulator is the usual one). It exists because device
linking shipped broken on mobile — finalize ran before `set_pin` opened the
local DB — and no flow drove both halves.
Steps 5-10 (#1291) then use the one account on both devices: a message from
each device reaches the other live in the open #General, and one sent while
the new device is backgrounded **and offline** (Android: airplane mode for
~20 s, so its LiveKit wake-up is lost) must land when it returns — via the
resume catch-up alone (`launchApp: stopApp: false`, no relaunch). On the
pre-#1291 build step 10 fails exactly there.

Run a second simulator/emulator with the peer account and drive it with a
parallel Maestro invocation, reusing `subflows/sign-in.yaml` with the peer env:
```bash
maestro --device <peer-udid> test -e MAESTRO_EMAIL=$MAESTRO_PEER_EMAIL \
  -e MAESTRO_OTP=$MAESTRO_OTP -e MAESTRO_PIN=$MAESTRO_PIN \
  .maestro/subflows/sign-in.yaml
```
Then assert convergence on both devices (peer sends → primary sees it live).

## Selecting by text, and the 2026-10 redesign

Every action the flows tap has a `testID` (`group/new`'s submit is
`btn-submit-group`; the redesign removed its Cancel). The flows still match
TEXT in only a few places: the "General" channel row (its dynamic
`row-channel-<id>` isn't known ahead of time), message/search-hit regexes
(`.*hello from maestro.*`), group names in a peer's list, and the iOS
"Open" system prompt. The redesign made all copy sentence case — there are no
uppercased labels any more — and Maestro's text match is a full,
case-sensitive regex, so any new text selector must match the catalogue string
exactly (`frontend/src/i18n/locales/en/*.json`) or use a regex.

Layout changes that flows depend on (see `SELECTORS.md` for the ids):
- **Groups tab:** with no groups it shows an empty state with
  `btn-create-group` / `btn-join-group`; with groups, those two live in the
  "+" sheet (`btn-add-group`) at the end of the group pill strip. Flows tap
  `btn-add-group` with `optional: true` first. A pill (`row-group-<id>`)
  switches the in-place `panel-group`; it does not push `screen-group`.
  Creating a group (`btn-submit-group`) also lands here, with the new group
  selected: wait for `btn-group-menu` with text `"<name>, .*"` (its label is
  "<name>, group menu…") and `panel-group` — there is no back step.
- **iPad (regular width):** Groups/Direct are two-pane; a conversation opens
  in the right pane (`screen-chat`, no `btn-back`) and the pages it opens
  (info, members, settings, thread, profile) push inside that pane. Flows that
  tap `btn-back` to leave a conversation mark it `optional: true`.
- **Direct tab:** DM requests are accepted on `dm/requests`, via
  `row-dm-requests`.
- **Sign out** asks first, in a native Alert with no testID: after
  `btn-sign-out`, tap the label `"Sign out"` (`auth:shell.signOutTitle`; es
  `"Cerrar sesión"`). No flow signs out today.
- **Search** puts its field at the top with the hits right under it, so a
  point tap to dismiss the keyboard can open a hit; `hideKeyboard` works there
  (normal keyboard with a return key). The number pad (OTP / change-email
  codes) has no return key, so `hideKeyboard` cannot close it.

## Known issues found by the first real run (2026-08-22)

The suite had never been executed anywhere before this. Fixed here:

- **Non-idempotent accounts.** Every flow `clearState`s and then signs UP, so a
  single fixed `MAESTRO_EMAIL` only worked on its first use ever — after that
  the account existed, auth took the returning-device enrollment path, no
  create-PIN screen appeared, and the flow died on `screen-auth-pin`. Seven of
  eight flows failed for this one reason. `maestro-run.sh` now mints a fresh
  disposable address per flow and runs each flow as its own invocation.
  (Enrollment needs the recovery key, which the harness cannot hold, so
  re-signing-up is the only self-contained option.)
- **`dms.yaml` had a step that could not fail.** It tapped
  `row-user-${MAESTRO_PEER_HANDLE}`, but the app emits `row-user-<userId>`.
  With `optional: true` the flow walked straight past and "sent" a message into
  whatever screen it was on. Now an id regex prefix, and not optional.
- **`sign-in.yaml` asserted the wrong landing screen.** It waited for
  `screen-groups`; it now waits for the tab bar and each flow taps the tab it
  needs.

Still open, needs app-side triage (not harness bugs):

- **Sign-in lands on the Self tab.** `initializing.tsx:76` replaces to
  `/(tabs)/groups` and nothing in the app routes to `/(tabs)/self`, yet Self is
  what renders. Reproducible.
- **A handle rename does not reach messages you then send.** `sender_username`
  is denormalized onto each message at send time from a cached `currentUser`
  (`hooks/queries/useMessages.ts`), so after changing your handle your own new
  messages still carry the old one — including across an app restart — while
  the conversation header shows the new one.

## Note on authoring vs running

Everything here is static, box-authored. The **running + screenshot capture +
defect triage** is the Mac step — the "visual evaluator later" in the plan.
Expect the first Mac run to need minor `extendedWaitUntil` timeout tweaks and to
close the `testID` gaps above.
