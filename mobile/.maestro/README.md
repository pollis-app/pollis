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
| `channel-menu` | #1193 the channel kebab sheet: every item tapped by `testID`, plus `channel-menu-01-open` — the gallery shot that must show an OPAQUE sheet (no header/composer through its buttons; the invariant itself is pinned in `tests/sheet-opaque.test.ts`) | no |
| `profile-prefs` | accent re-theme, behavior toggle, display-name save | no |
| `search` | #1202 desktop parity: a seeded message hit with highlighted snippet + "About N results", sort toggle, corpus footer, opening a hit, settings-page quick-jump, and the "why no results" explanation | no |
| `security` | device list + blocked-list entry | no |
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
`tour-34-…`), so a layout or copy change can be compared before and after. It
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
— the admin creates a group and opens it from its Groups-tab header; the peer
finds it by slug (one lookup, on Search) and requests access; the admin sees
the request on the Groups tab (row + header badge) and approves it; the peer
then lists the group.

**Report (automated, #1213):** `mobile/scripts/maestro-report.sh <reporter-device> <peer-device>`
— a fresh peer signs up, its handle is read off the screen, and the reporter
reports it with "Report and block", then unblocks.

**QR device link (automated, #1207):** `mobile/scripts/maestro-qr-link.sh <existing-device> <new-device>`
— the existing device shows a code from Security (PIN first), the script reads
the code's text off its UI tree, the new device signs in with it from "Sign in
with another device" (entered as text: a simulator camera can't be aimed at
another screen), the existing device approves the tag-verified request, and the
new device must finish its PIN and list the group. Needs a dev DS with the
link endpoints.

**Device link (automated):** `mobile/scripts/maestro-device-link.sh <existing-device> <new-device>`
runs both halves of device-linking sign-in across two devices
(`.maestro/two-client/device-link/`): the existing device signs up and makes
a group, the new device signs in and shows its code, the script reads it off
the new device's UI tree, the existing device approves, and the new device
must create its PIN, finalize, and list the group. Any pair works
(iOS sim + Android emulator is the usual one). It exists because device
linking shipped broken on mobile — finalize ran before `set_pin` opened the
local DB — and no flow drove both halves.

Run a second simulator/emulator with the peer account and drive it with a
parallel Maestro invocation, reusing `subflows/sign-in.yaml` with the peer env:
```bash
maestro --device <peer-udid> test -e MAESTRO_EMAIL=$MAESTRO_PEER_EMAIL \
  -e MAESTRO_OTP=$MAESTRO_OTP -e MAESTRO_PIN=$MAESTRO_PIN \
  .maestro/subflows/sign-in.yaml
```
Then assert convergence on both devices (peer sends → primary sees it live).

## Known `testID` gaps (small #620 follow-up)

Authoring these flows surfaced a few load-bearing actions that lack a `testID`,
so the flows tap them by visible TEXT (works for stable labels, but a `testID`
is more robust):
- `group/new` — the **CREATE GROUP** / **Cancel** buttons.
- Channel/group rows are opened by their name text where the dynamic
  `row-channel-<id>` isn't known ahead of time.
Add these in a #620 follow-up and switch the `tapOn: "TEXT"` calls to
`tapOn: id:`.

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
