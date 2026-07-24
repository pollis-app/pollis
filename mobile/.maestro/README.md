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
2. **A DEV build of the app installed on each target device.** The build must
   point at the **dev** Delivery Service and a dev LiveKit URL (baked at build
   time, not Maestro env):
   ```
   EXPO_PUBLIC_POLLIS_DELIVERY_URL=https://api-dev.pollis.com
   EXPO_PUBLIC_LIVEKIT_URL=wss://<dev-livekit>
   EXPO_PUBLIC_TURSO_URL=... EXPO_PUBLIC_TURSO_TOKEN=...   # dev, read-only
   ```
   Build + install: `cd mobile && pnpm expo run:ios` / `run:android` (see
   `mobile/CLAUDE.md` for the ubrn/native steps). App id: `com.anonymous.mobile`.
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

## Note on authoring vs running

Everything here is static, box-authored. The **running + screenshot capture +
defect triage** is the Mac step — the "visual evaluator later" in the plan.
Expect the first Mac run to need minor `extendedWaitUntil` timeout tweaks and to
close the `testID` gaps above.
