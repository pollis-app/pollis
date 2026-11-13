---
name: mobile-release
description: Ship the Pollis mobile app from main to App Store review (ios) or Google Play production review (android), end to end. Picks the version, writes the release notes (mobile-release-notes), gates on the Maestro suite, does the store build, uploads, records the runtime version and submits for review. Use when asked to release, ship, publish or submit the mobile app, send it for review, or "run the release" for iOS or Android.
argument-hint: "ios|android"
---

# Mobile release

One platform per run: `ios` or `android`. Each step must finish before the next
starts. Build commands take tens of minutes, so run them in the background and
wait for the notification; never poll.

House rules that apply throughout:
- Never ship with known failures (step 3).
- Never touch an App Store version that is already in review or waiting for
  release; `asc-submit.py` refuses.
- Branches are `fix/*`; never `chore/*`, whose path filters skip CI.
- Commit messages are single-line.
- Close every issue a shipped PR fixes by hand (`Fixes #n` never auto-closes).

## 1. Version

Read the version `V` on `origin/main` (`git show origin/main:mobile/package.json | jq -r .version`).
Store build numbers derive from it: `code = ((major*100+minor)*100+patch)*100+POLLIS_BUILD`.

Check whether the store already has `V`:
- **iOS:** `python3 -I mobile/scripts/asc-submit.py V <code> <any notes file>` (plan only) lists every App Store version and its state.
- **Android:** `python3 -I mobile/scripts/play-publish.py tracks` lists every track's versionCodes.

Then:
- **`V` was submitted, approved or released on this store:** bump the patch.
  Open a `fix/mobile-<new V>` PR changing only `mobile/package.json` `version`, plus the notes from step 2.
  Merge it once CI is green.
- **`V` was only uploaded (TestFlight, internal or alpha) and never submitted:**
  keep `V` and upload again with `POLLIS_BUILD` set to one more than the
  highest build already uploaded. Both stores refuse a build number they have
  seen, and say so only after the upload.

## 2. Release notes

Run the **mobile-release-notes** skill for this platform. It writes
`mobile/store/release-notes/<V>.<platform>.txt`. Commit that file in the bump PR,
or in its own `fix/` PR when there is no bump. Show the notes to the user.

## 3. Gate: the Maestro suite on the release commit

Build a Release simulator/emulator app from the release commit, in its own
worktree with the dev `mobile/.env` and `.maestro/.env` copied in. The store
build's worktree must never hold a `.env`. Build the Rust core with
`uniffi-bindgen-react-native build <ios|android> --config ubrn.config.yaml --and-generate --release`;
for iOS, leave out `--no-sim`, because the simulator needs that slice.

- **iOS:**
  1. `scripts/build-release-sims.sh ios`, then `xcrun simctl install` the app on the iPad too.
  2. `scripts/maestro-run.sh all ios` and `scripts/maestro-run.sh all ipad`.
  3. The two-client flows: `scripts/maestro-report.sh <iphone> <ipad>` and `scripts/maestro-approval-prompt.sh <iphone>`.
- **Android:**
  1. `scripts/build-release-sims.sh android` on the `pollis_e2e` emulator.
  2. `scripts/maestro-run.sh all android`.
  3. The two-client scripts that run on Android.

Triage every failure before going on:
- **A real bug:** fix it in a PR, merge it, and go back to step 1, since the commit moved.
- **A flake:** re-run that flow alone. It must pass.
- **A harness gap:** fix the flow in a PR. If it cannot be fixed now, file an issue
  and say so in the report. Only do this for failures that do not touch the app
  (the same failure on `origin/main`'s copy of the flow, for example).

Do not upload until the suite is clean or every remaining failure is filed and
explained. Report the pass counts per device. Shut the simulators and emulator
down afterwards.

## 4. Store build

```bash
POLLIS_BUILD=<n, only when re-uploading V> mobile/scripts/store-build.sh <platform>   # in the background
```

It builds from `origin/main` in `.claude/worktrees/store-<platform>` with prod
config from Doppler and a private Metro cache. It runs every binary check from
mobile/CLAUDE.md → "Store builds", and exits non-zero on the first mismatch. It
ends with `artifact=`, `version=`, `build_number=`, `commit=` and
`runtime_version=`. Check that `commit` is the commit the gate ran on.

## 5. Upload

- **iOS:**
  ```bash
  xcrun altool --upload-app -f <artifact> -t ios \
    --apiKey "$(doppler secrets get ASC_KEY_ID -p pollis -c prd_prod --plain)" \
    --apiIssuer "$(doppler secrets get ASC_ISSUER_ID -p pollis -c prd_prod --plain)"
  ```
- **Android:**
  ```bash
  python3 -I mobile/scripts/play-publish.py upload <artifact> --track internal --status completed \
    --name "V (code)" --notes-file mobile/store/release-notes/V.android.txt
  ```

## 6. Runtime-version ledger

In a `fix/runtime-versions-<V>` PR, add
`{"platform", "channel": "app-store" | "play", "version", "commit", "runtimeVersion"}`
to `mobile/store/runtime-versions.json`, using the values from step 4, and merge it.
- If a re-upload of `V` already has an entry, update that entry's `commit` to
  the build that goes to review. The notes script maps versions to commits
  through this file.
- **Android:** also tag `mobile-v<V>` on the release commit to publish the
  sideload APK (`.github/workflows/mobile-apk-release.yml`). When the run
  succeeds, add its `sideload` entry the same way. That run also publishes the
  version's SBOM and licence summary to pollis.com/open-source (#1832).
- **iOS only (no `mobile-v<V>` tag this time):** publish that evidence by hand:
  `gh workflow run oss-evidence.yml -f product=mobile -f ref=<release commit> -f version=v<V>`.

## 7. Submit for review

- **iOS:** wait for Apple to process the build (10–20 min). Running the
  plan-only command says `VALID` once it is ready. Then:
  ```bash
  python3 -I mobile/scripts/asc-submit.py V <code> mobile/store/release-notes/V.ios.txt --submit
  ```
  This creates or reuses the App Store version, attaches the build, sets
  "What's New" and submits it, with the live version's release type
  (automatic after approval). It ends at `WAITING_FOR_REVIEW`.
- **Android:**
  ```bash
  python3 -I mobile/scripts/play-publish.py promote <code> --tracks alpha,production \
    --name "V (code)" --notes-file mobile/store/release-notes/V.android.txt
  ```
  A production release goes to Google's review on commit and rolls out once
  approved.

If a permission check blocks the submit command, give the user the exact
command to run with `!`, and do not work around it.

## 8. Report

One short summary:
- the version and build number
- the commit
- the gate's pass counts and any filed issue
- the review state
- the PRs and issues opened, merged or closed
- notes on anything else that ships from the same commit and is now behind
  (the other store, the sideload APK)

Then remove this release's worktrees (`git worktree remove --force --force`)
and their local branches.
