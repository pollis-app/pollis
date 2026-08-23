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
   expo-dev-client's stored dev-server URL — so a `pnpm expo run:ios` build
   boots to the **dev-client launcher menu** and the app never loads. The suite
   cannot drive that build at all. A Release build embeds the JS bundle and has
   no launcher. It is also the more honest target: these flows are
   timing-sensitive and dev-mode JS is not what ships.
   ```bash
   cd mobile && pnpm expo run:ios --device <simulator-udid> --configuration Release
   ```
   Pass the simulator by **UDID** (`xcrun simctl list devices`) and boot it
   first; a name that matches no *booted* simulator makes xcodebuild fall
   through to a physical-device destination and fail.

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
`IPAD_DEVICE=…`, `ANDROID_AVD=…`.

## Flow matrix (`flows/`)

| Flow | What it proves | Peer? |
| --- | --- | --- |
| `auth` | email→OTP→PIN→inbox (also the smoke test: bridge + dev DS + keystore live) | no |
| `groups` | create group, open, channel visible | no |
| `messaging` | send / edit / delete / react in a self-owned channel | no |
| `profile-prefs` | accent re-theme, behavior toggle, display-name save | no |
| `search` | message/user/group search | no |
| `security` | device list + blocked-list entry | no |
| `ipad-two-pane` | #622 list+detail side-by-side (run on **iPad**) | no |
| `dms` | start a DM with the seeded peer (initiator side) | yes |

Two-client / special flows (`enrollment`, `realtime`, `blocking`, `push-tap`,
and the DM accept/reply side) are scaffolded in `_two-client.md` — they need the
two-device setup below and a Mac shakedown.

## Two-client flows

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
