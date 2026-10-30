# mobile/CLAUDE.md

Notes for future Claude sessions working inside `mobile/`. Root `CLAUDE.md` applies for repo-wide rules (commits, pnpm, etc.), but **design / UX rules in root `CLAUDE.md` are desktop-only and do NOT carry over to mobile** — in particular the "NO MODALS" rule is a desktop constraint. Mobile uses native mobile patterns (bottom sheets, full-screen confirmations, gesture-driven flows).

## Project isolation — read first

The `mobile/` directory is **NOT** a pnpm workspace member. It is a standalone Expo project that happens to live inside the repo.

- Root `pnpm-workspace.yaml` lists only `frontend`. **Never add `mobile`** — doing so hoists mobile packages into the root `node_modules` and destroys the Expo install. (We already hit this once; recovering takes a full clean reinstall.)
- `mobile/.npmrc` sets `node-linker=hoisted` — Metro requires a flat `node_modules`.
- All pnpm commands run from inside `mobile/` with `--ignore-workspace`:
  ```bash
  cd mobile && pnpm install --ignore-workspace
  cd mobile && pnpm add <pkg> --ignore-workspace
  ```
- `mobile/pnpm-lock.yaml` is independent of the root lock.
- **Use pnpm 10.25.0 here, the version CI uses.** The root `packageManager: pnpm@10.25.0` does not apply under `--ignore-workspace`, so a bare `pnpm` in `mobile/` runs whatever is on `PATH` — Homebrew's is 12.x, which rewrites the lockfile differently from CI and makes `--frozen-lockfile` fail there. Install with `npx -y pnpm@10.25.0 install --ignore-workspace` (or a corepack-pinned pnpm).
- If Expo complains about missing packages, first check that `node_modules` is inside `mobile/` and not at the repo root.
- `patches/` holds pnpm patches, registered in `package.json` → `pnpm.patchedDependencies` (not a `mobile/pnpm-workspace.yaml`, which would make `mobile/` look like a workspace root). Today there is one: `decode-uri-component@0.2.2`, which backports 0.5.0's linear decoder because every `pollis://` link runs through it and 0.5.0 is ESM-only (#1296; `tests/deep-link-decode.test.ts`). Drop it once expo-router stops pulling query-string 7.
- Mobile imports **no frontend TypeScript**. Generated data is copied across by its generator (`emojiData.ts`, `emoji/annotations/`). The ONE shared directory is the translation catalogues, `frontend/src/i18n/locales/`, reached through a `watchFolders` entry in `metro.config.js` — see [Localization](#localization-i18n-1074). Do not widen that list.

## Stack

- Expo SDK 57, React Native 0.86.3, React 19.2.3, TypeScript 6.0
- Hermes V1 — the default engine since RN 0.84 / SDK 56; nothing configures it
- `expo-router` 57 (file-based routing in `app/`). Since SDK 56 it carries its own fork of react-navigation, so it versions with the SDK rather than as "Router v6"
- Reanimated 4.5, `react-native-worklets` 0.10, Gesture Handler 2.32, Screens 4.26, Safe Area Context 5.7, `react-native-svg` 15.15
- `expo-camera`, `expo-secure-store`, `expo-notifications`
- `expo-image` (caching, and the blurhash placeholders — there is no separate blurhash package)
- `@livekit/react-native-webrtc` pinned to `144.1.2` + `@livekit/react-native-expo-plugin` `^1.0.3` (installed for #343, not wired — see "Voice")
- `lucide-react-native` icons, wrapped in `components/icons.tsx` (stable `Icon.*` API, strokeWidth pinned to 1.75 — `ICON_STROKE`). `react-native-svg` is still used directly for the Initializing dot-field.
- Geist (UI, the desktop refined skin's face) via `@expo-google-fonts/geist`, one family name per weight (`fonts.regular/medium/semibold/bold`; never pair with `fontWeight`). Monospace (crypto material, e.g. the enrollment approval code) uses the **system** mono face — `fonts.mono*` = `Platform.select({ ios: 'Menlo', android: 'monospace' })`, no bundled font.
- Rust core via `pollis-native` Turbo Module (uniffi-bindgen-react-native)

## Tests

```bash
cd mobile && pnpm test        # node --test over mobile/tests/
```

Node's own runner, no Jest and no RN renderer — it covers the **pure modules**
under `hooks/`/`lib/`/`i18n/`/`components/emoji/` (the reaction-toggle reducer,
the language registry and resolution rules, the emoji search ranking against
the real generated tables), which is where the rules that are worth pinning
actually live. Node 22 strips the TS types, so
there is no compile step; imports carry an explicit `.ts` extension, which is
why `tsconfig.json` sets `allowImportingTsExtensions` (and `noEmit`, which that
option requires) rather than excluding `tests/` from typechecking the way
`frontend/tsconfig.json` does.

It runs in CI in the **`expo-doctor` job** of `mobile-core-check.yml` — the
cheap one, with no Rust, NDK or Gradle — so a broken reducer fails in seconds
instead of behind a 3-ABI cross-compile. The same job runs the OTA update
routes' tests (`node --test pollis-delivery/worker-tests/*.test.ts`); `tests/ota-*`
cover the publish pipeline (signing with a throwaway in-memory certificate,
never the real one) and the app's `updates` config.

Anything needing a device or a renderer stays out: Maestro (`.maestro/`) is the
tier for that.

## Localization (i18n, #1074)

`i18next` + `react-i18next`, the same stack and the **same catalogues** as
desktop — `mobile/i18n/resources.ts` is a generated static table of every
`frontend/src/i18n/locales/<lng>/<ns>.json`, which Metro can resolve because
`metro.config.js` adds that one directory to `watchFolders`. A key is
addressed exactly as on desktop (`t("settings:language.heading")`, or
`t("language.heading")` under `useTranslation("settings")`); copy that exists
only on mobile lives in the `mobile` namespace, in the same directory, so
translators and `scripts/i18n-check.mjs` see one catalogue. The canonical
article is `.codesight/wiki/i18n.md`; `frontend/src/i18n/README.md` is the
working checklist and applies here verbatim (keys, plurals, interpolation,
what is not translated).

Mobile-specific pieces, all under `mobile/i18n/`:

- `languages.ts` — the registry (a copy of desktop's; `tests/i18n.test.ts`
  fails if it drifts from the locale directories), tag normalization, and the
  device locale via `Intl.DateTimeFormat().resolvedOptions().locale` (Hermes
  answers with the OS locale on both platforms — no `expo-localization`
  prebuild).
- `storage.ts` — the device-local choice in `expo-secure-store`, user-scoped
  like desktop's `localStorage` key. Never the synced preferences blob: the
  pre-auth screens need it.
- `index.ts` — init, `hydrateLanguage()` (the root layout holds the splash on
  it so the first frame is already in the stored language), `setLanguage`,
  `adoptUserLanguage`, `activeLocale()` and `upper()`.

Rules that are easy to get wrong here:

- **Every `toLocale*` / `Intl.*` call passes `activeLocale()`.** A bare
  `undefined` formats against the host locale and puts an English date under an
  Arabic heading (desktop's #902).
- **UI copy is sentence case** — no uppercased labels since the 2026-10
  redesign. Where an uppercase string is genuinely needed (an acronym), use
  `upper(t(...))`, never `.toUpperCase()` on translated text
  (locale-invariant casing is wrong in Turkish and a no-op in Arabic).
- **RTL is `I18nManager`.** `setLanguage` calls `forceRTL` for the language's
  `dir`, which React Native applies on the **next launch**; `LanguageSection`
  shows `mobile:language.restartRequired` until then (`layoutRestartPending`).
  Never write the direction before `hydrateLanguage` has read the stored
  choice: Android's Fabric re-reads it on every root measure, so a boot-time
  `forceRTL(false)` from the device-locale guess un-mirrors that whole launch
  (`tests/rtl-boot.test.ts`).
  Yoga mirrors flex layout and `left/right` positioning on its own; what it
  cannot mirror is a glyph, so `components/icons.tsx` flips only the icons
  that encode direction (`back`, `fwd`, the arrows).
- **Emoji search is localized** through the CLDR annotations the generator
  emits under `components/emoji/annotations/` (#901); `useEmojiAnnotations`
  loads the active locale's table and English's.

Adding a locale: follow the desktop README, then add the same row to
`mobile/i18n/languages.ts` and run `node scripts/mobile-i18n-resources.mjs`.
`scripts/i18n-check.mjs` (which also scans mobile call sites) and
`tests/i18n.test.ts` fail until both are done.

The Maestro flow `.maestro/flows/i18n.yaml` is the device-tier proof: copy
re-renders on switch, the choice survives a relaunch, Arabic mirrors after one.

## Rust bridge (`modules/pollis-native`)

The bridge is a local RN turbo-module that links our `pollis-core` Rust crate into the Expo app via JSI.

- `ubrn.config.yaml` points at `pollis-core` (`directory: ../../../pollis-core`)
- `cargo-ndk` compiles `pollis-core` for `arm64-v8a`, `armeabi-v7a`, `x86_64` as `.a` (static)
- `uniffi-bindgen-react-native` generates TS bindings (`src/generated/`), C++ JSI glue (`cpp/`), and CMake setup (`android/CMakeLists.txt`)
- CMake statically links the Rust `.a` into a turbo-module `.so`

### Dev loop — Rust changes

```bash
cd mobile/modules/pollis-native
uniffi-bindgen-react-native build android --config ubrn.config.yaml --and-generate
cd ../../android && ./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.pollis.mobile/.MainActivity
```

### Dev loop — TS-only changes

Metro hot-reload handles it. No rebuild.

### iOS path

First smoke-tested (on SDK 55) on macOS Tahoe (26.4.1) + Xcode 26.4.1 + iOS 26.4 simulator. `version()` round-trips from Rust through the JSI bridge to the QR screen, then swipe through the card stack works end-to-end.

**Expo SDK 55 and later require Xcode 26 / macOS 26 at minimum.** Earlier Xcode/macOS hits `@MainActor` parse errors in `expo-modules-core` (see [expo/expo#42525](https://github.com/expo/expo/issues/42525) — closed, won't fix). SDK 54 is the last line that supports Xcode 16.x if a downgrade is ever needed.

#### Xcode 27 / iOS 27 SDK

Three things changed under Xcode 27, each of which stops a build or a launch outright:

- **UIScene life cycle is mandatory.** An app linked against the iOS 27 SDK that still uses the legacy `AppDelegate`-owns-the-window life cycle traps at launch (`EXC_BREAKPOINT` in `_UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption`) — before any JS runs, so it looks like a native crash, not a config problem. SDK 57's template still generates the legacy `AppDelegate`, so we opt in with `expo-build-properties` `ios.enableSceneSupport: true` in `app.json` (the flag landed in expo-build-properties 57.0.19, the runtime half was backported in expo 57.0.23 — expo/expo#50191, #50205; background in [expo/fyi ios-scene-lifecycle.md](https://github.com/expo/fyi/blob/main/ios-scene-lifecycle.md)). Prebuild then makes `AppDelegate` conform to `ExpoReactNativeFactoryProvider`, drops its own `UIWindow` / `startReactNative`, and adds a `UIApplicationSceneManifest` naming `EXExpoAppSceneDelegate`. Deep links still work — a cold start is rebuilt from the scene's `connectionOptions`, a warm one arrives via the scene's `openURLContexts` → `AppDelegate` → `RCTLinkingManager` — and push is unaffected (`UNUserNotificationCenter` is not tied to the life cycle). **On SDK 58 the flag is a no-op; remove it then.**
- **Pod targets below iOS 15.0 are rejected.** On SDK 57 nothing custom is needed: RN 0.86's `post_install` raises library targets to ≥ 15.1, and expo-modules-autolinking raises resource-bundle targets (it logs `Raised resource bundle deployment targets to match their pods: RNSVG-RNSVGFilters, SDWebImage-SDWebImage`). After `pod install` every Pods target is 15.1 or 16.4. If a future pod trips this, fix it in a config plugin, not by editing the generated `Podfile`.
- **There is no Simulator.app**, so `expo run:ios` fails with `Can't determine id of Simulator app`. Boot, build, install and launch by hand — see "Dev loop — iOS". Simulator names also changed: Xcode 27 ships **iPhone 18 Pro** and **iPad Pro 13-inch (M5)**, not the iPhone 17 Pro / iPad Pro 13-inch (M4) older notes and scripts assume.

**An Xcode update silently breaks every simulator build until you download the matching runtime.** Xcode auto-updated 26.4.1 → 26.6 mid-session on 2026-08-22; the new SDK is iOS 26.5, no iOS 26.5 *simulator runtime* was installed, and `xcodebuild` then enumerates **zero** simulator destinations. Every build fails with `Unable to find a destination matching the provided destination specifier`, and the only "ineligible" entry it prints is the physical-device placeholder — which reads like a code-signing problem and is not one. Booting an older-runtime simulator does **not** help. The fix is one command and an 8.5 GB download:

```bash
xcodebuild -downloadPlatform iOS
xcodebuild -workspace ios/Pollis.xcworkspace -scheme Pollis -showdestinations   # sanity: should list simulators, not just the placeholder
```

Two related traps while you are in there: address simulators by **UDID** (`xcrun simctl list devices`) and boot them first — a name matching no booted simulator makes xcodebuild fall through to a device destination; and the store `.ipa` currently in `build/export` was produced under Xcode **26.4.1**, so a rebuild under 26.6 is not byte-identical.

### Dev loop — iOS

```bash
# One-time per machine
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
cargo install --git https://github.com/jhugman/uniffi-bindgen-react-native \
  --tag 0.31.0-6 --locked uniffi-bindgen-react-native --force
brew install cocoapods

# Per Rust change
cd mobile/modules/pollis-native
uniffi-bindgen-react-native build ios --config ubrn.config.yaml --and-generate
# Regenerates PollisNativeFramework.xcframework + cpp/ + src/generated/.

# Per build (also after ubrn regen). Xcode 27 has no Simulator.app, so
# `pnpm expo run:ios` cannot boot or find a simulator; do its steps by hand.
cd mobile
pnpm expo prebuild -p ios --no-install   # cleans ios/ by default since SDK 57; --no-install so it can't run a workspace-blind pnpm install
(cd ios && pod install)
xcrun simctl list devices available   # pick a UDID (e.g. iPhone 18 Pro)
xcrun simctl boot <udid>
xcodebuild -workspace ios/Pollis.xcworkspace -scheme Pollis \
  -configuration Release -destination id=<udid> \
  -derivedDataPath ios/build build
xcrun simctl install <udid> ios/build/Build/Products/Release-iphonesimulator/Pollis.app
xcrun simctl launch <udid> com.pollis.ios
xcrun simctl io <udid> screenshot shot.png   # no Simulator window to look at
```

`-configuration Release` embeds the JS bundle, so the app runs without Metro
(and is what Maestro needs anyway); use `Debug` plus `pnpm expo start` for a
hot-reloading dev client. **`expo prebuild` cleans by default since SDK 57** —
it regenerates `ios/` and `android/` from scratch each run, so anything edited
by hand in them is gone; every native change belongs in `app.json` or a
config plugin. (`--clean` in older commands here is now redundant.)

TS-only changes are picked up by Metro hot-reload. No rebuild.

## Environment (gradle + cargo-ndk)

**Arch Linux box:**
```
JAVA_HOME=/usr/lib/jvm/java-17-openjdk
ANDROID_HOME=/opt/android-sdk
ANDROID_SDK_ROOT=/opt/android-sdk
ANDROID_NDK_ROOT=/opt/android-ndk
ANDROID_NDK_HOME=/opt/android-ndk
```
Persisted in `~/.bashrc` and `.envrc`. If you're in a fresh shell and builds fail with "SDK location not found", these are missing.

**macOS (Apple Silicon) — Android build verified here 2026-06-18.** `source
mobile/android-env.sh` before any ubrn/gradle/adb command. Toolchain (all
no-sudo): OpenJDK 17 via `brew install openjdk@17`; SDK at
`~/Library/Android/sdk` (cmdline-tools unzipped to `cmdline-tools/latest/`);
`sdkmanager` packages `platform-tools platforms;android-36 build-tools;36.0.0
ndk;27.1.12297006 cmake;3.22.1`, plus `emulator
system-images;android-36;google_apis;arm64-v8a` for the Maestro tier, whose
AVD is `pollis_e2e` (`maestro-run.sh`'s default): `echo no | avdmanager create
avd -n pollis_e2e -k "system-images;android-36;google_apis;arm64-v8a" -d
pixel_8`. AVDs live in `~/.android/avd`, outside the SDK, so they survive an SDK
reinstall but are invisible (`emulator -list-avds` is empty) until the SDK and
that system image are back. If the emulator dies at boot with `detected a
hanging thread 'QEMU2 main loop'` (seen on a heavily loaded Mac, right after
"Vulkan emulation initialized"), kill any orphan `qemu-system-aarch64` still
holding the AVD and boot it headless on the software renderer —
`emulator -avd pollis_e2e -no-snapshot -no-boot-anim -no-window -gpu
swiftshader_indirect` came up in ~30 s where the default boot crashed every
time; `maestro-run.sh` reuses an emulator that is already running. A clean `gradlew assembleDebug` produces
`android/app/build/outputs/apk/debug/app-debug.apk` with `libpollis-native.so`
embedded for arm64-v8a / armeabi-v7a / x86_64.

**iOS build verified on macOS 26.4.1 (Tahoe) + Xcode 26.4.1, 2026-06-22 (SDK 55).** A
clean run from a fresh checkout works end to end with no source changes: `ubrn
build ios` → `PollisNativeFramework.xcframework` (device `ios-arm64` + universal
`ios-arm64_x86_64-simulator` slices), `expo prebuild --platform ios` →
`pod install`, then `pnpm expo run:ios` compiles RN-from-source and launches on
the iPhone 17 Pro simulator; the bridge initializes against live Turso and the
app reaches the auth screen. Toolchain (all no-sudo): the three iOS Rust targets
(`rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios`),
`uniffi-bindgen-react-native` CLI (then `0.31.0-2`; now `0.31.0-6`, see
"Rough edges" #1), and CocoaPods. Under Xcode 27 the `expo run:ios` step no
longer works — see "Xcode 27 / iOS 27 SDK".

**Expo SDK 55+ requires Xcode 26 / macOS 26** (verified June 2026: SDK
54 was the last to accept Xcode 16.x). On an older macOS/Xcode (e.g. macOS 14.7
+ Xcode 15.1) the build hits `@MainActor` parse errors in `expo-modules-core`
([expo/expo#42525](https://github.com/expo/expo/issues/42525) — closed, won't
fix); that machine needs a macOS/Xcode upgrade and any iOS xcframework it
carries is a stale prior artifact.

## Build profiles + CI (`eas.json`, #706 / PL-18)

`eas.json` holds the three conventional EAS profiles (`development` / `preview` /
`production`). Since #707 the app is linked to a real EAS project,
**`@pollis/mobile`** — `expo.owner` plus `expo.extra.eas.projectId` in
`app.json`, written by `eas init`.

- `cli.appVersionSource: "local"` — the version lives in `app.json`; no EAS
  server holds it.
- **`runtimeVersion.policy: "fingerprint"`, in `app.json`.** `pollis-core` is
  a native Rust lib reached over uniffi, so a JS-only OTA is unsafe the moment
  the core changes. `fingerprint` ties the runtime version to a hash of the
  native layer, so any native change forces a fresh binary instead of letting an
  incompatible OTA land on an old one — `sdkVersion`/`appVersion` would allow
  exactly that. Do not switch it without understanding this. It is computed
  locally, with no EAS involved, and `fingerprint.config.js` widens it to cover
  pollis-core — see [OTA updates](#ota-updates-1250).
  **It belongs to the app config, not to a build profile:** `runtimeVersion` is
  not in the eas.json schema, and a profile carrying it makes current EAS CLI
  reject the entire file (`"build.<profile>.runtimeVersion" is not allowed`) —
  so every `eas` command fails, not just builds. It sat that way until #707.
- **No `credentials` block** — EAS holds the credentials remotely
  (`credentialsSource: "remote"`); do not add placeholders. The push
  credentials themselves (APNs key, FCM v1 service account) are uploaded to EAS
  by hand, see below.

**CI builds a real APK** (the first thing that ever did): the `android-build`
job in `.github/workflows/mobile-core-check.yml` runs the same chain as the
workstation dev loop above — `ubrn build android --and-generate`, then
`expo prebuild --platform android`, then `gradlew :app:assembleRelease` (arm64-v8a
only, `-PreactNativeArchitectures=arm64-v8a`, to keep the gate fast; the sideload
release builds all three ABIs) — on a stock `ubuntu-latest` runner, and uploads
the APK as `pollis-android-apk`. It
installs the RN-pinned NDK (`27.1.12297006`) + CMake (`3.22.1`) so Gradle and
`cargo-ndk` agree, and pins the ubrn CLI to `0.31.0-6` (CLI ↔ npm alignment, see
"Rough edges" #1). It uses `expo prebuild` + Gradle **directly, never `eas build`** — that
predates the EAS project and still holds: the job needs no account, no token and
no queue. The release APK is signed with the throwaway **debug keystore** the
Expo template generates (no signing secret); real upload signing is blocked
(needs the Play console).

**`expo-doctor` is a required, clean gate** (21/21 on SDK 57). The dependency
set is pinned to the Expo SDK 57 canonical versions (`bundledNativeModules.json`
in the installed `expo` package), so doctor's version, duplicate-native-module,
and missing-peer checks all pass — including TypeScript, which doctor now
expects at `~6.0.3`. Things worth knowing:

- **Nothing is excluded from doctor's version check.** `react-native` used to be
  held back via `expo.install.exclude` on the theory that ubrn generates C++
  against RN's headers, so moving RN caused `no member named 'string_to_buffer'`.
  That diagnosis was wrong: `string_to_buffer` is ubrn's *own* helper
  (`cpp/includes/UniffiString.h` in the npm package, added in ubrn PR #378), and
  the error means the cargo-installed ubrn CLI and the npm package are different
  versions — "Rough edges" #1, not an RN mismatch. The exclusion is gone and RN
  tracks the SDK. ubrn's nightly CI covers RN 0.84–0.87 (iOS builds; Android only
  compares generated output), so an RN bump inside that range is not by itself a
  reason to move ubrn.
- **`react-native-worklets` is a direct dependency** (`0.10.1`, the version SDK 57
  pairs with Reanimated `4.5.1`). Reanimated 4 split worklets into its own native
  module; a native peer must be a direct dep so autolinking picks it up, or
  doctor's missing-peer check fails.

- **Doctor drifts red on its own.** Expo ships patch releases to the current SDK line
  continuously, so a set of packages that passed last month goes stale and fails
  the gate on **every** PR regardless of content (this happened: 10 packages at
  once). The fix is to re-pin to the versions doctor names, as a standalone PR.
  Do **not** reach for `expo install --fix` to do it — the Expo CLI shells out to
  `pnpm` without `--ignore-workspace`, which hoists mobile's tree into the root
  `node_modules` and destroys the install (see "Project isolation" above). Edit
  the versions in `package.json` by hand, then
  `npx -y pnpm@10.25.0 install --ignore-workspace`.

`app.json` carries **no** `newArchEnabled` / `android.edgeToEdgeEnabled` — both are
unconditional defaults since SDK 55 / RN 0.83 and were dropped from the config
schema, so listing them fails doctor's schema check for no behavioural gain.

## Store builds (no EAS — local `expo prebuild` + native tooling)

Published by Pollis Inc. (dan@pollis.com), Apple team `8JT3WH45NZ`, iOS bundle id `com.pollis.ios` (Android keeps `com.pollis.mobile`). The 2026-08 to 2026-10 builds below were made on the retired personal team `9JF7WWYMU2`, whose app record never passed review and now holds `com.pollis.mobile`.

**Version bookkeeping is derived — edit `package.json` `version`, nothing else.**
`app.config.js` reads it and computes all three store fields, so they cannot
drift apart the way three hand-bumped numbers do:

| field | value | from |
|---|---|---|
| `expo.version` | `1.0.0` | `package.json` verbatim |
| `expo.ios.buildNumber` | `"1000000"` | derived |
| `expo.android.versionCode` | `1000000` | derived |

```
code = ((major * 100 + minor) * 100 + patch) * 100 + build     # build = $POLLIS_BUILD, default 0

1.0.0            -> 1000000
1.0.0 (build 1)  -> 1000001    same user-facing version, still uploadable
1.0.1            -> 1000100    still strictly greater
```

The separate `build` counter exists because **both stores reject a reused build
number, and only tell you after the upload.** Deriving the build number from
the version alone would make the second upload of a version impossible — which
is the common case, not the rare one: ship a TestFlight build, fix one thing,
upload again, users still see 1.0.0. To re-upload an unchanged version, set
`POLLIS_BUILD=1` (2, 3, …) for the prebuild; nothing else moves.

`versionCode` must increase strictly across every release Play has ever seen,
and this is monotonic in (major, minor, patch, build) provided minor/patch/build
each stay under 100 — `app.config.js` throws rather than let a component wrap,
because a versionCode that goes backwards cannot be fixed without a new package
name. `app.json` keeps everything else (plugins, icons, privacy manifests); it
no longer carries `version`, `ios.buildNumber` or `android.versionCode` at all.

**Environment first.** `export POLLIS_OTA=production` for the prebuild AND the
native build (Xcode archive / Gradle): it switches on signed OTA updates
(#1250), and `app.config.js` fails if the OTA code-signing certificate is
missing. Without it the store build ships with updates disabled and can never
take one. Then, because `EXPO_PUBLIC_*` vars inline into the shipped JS bundle,
check `mobile/.env`:

- `EXPO_PUBLIC_POLLIS_DELIVERY_URL` **must** be `https://api.pollis.com` (prod
  DS), not api-dev.

**Give every store build a private Metro cache.** Metro's transform cache lives
in `$TMPDIR/metro-cache`, which every worktree and every agent on this machine
shares, and an inlined `EXPO_PUBLIC_*` value is **not** part of its cache key.
So a store build of the same source can silently reuse a module transformed by
an earlier api-dev build: the 1.0.22 AAB first came out with
`https://api-dev.pollis.com` baked in while `.env` and the environment were
both prod. Run the whole store build (prebuild, Xcode archive, Gradle) with
`export TMPDIR="$(mktemp -d)"`, and still grep the finished IPA/AAB bundle for
`api-dev` before uploading (the binary checks below).
- `EXPO_PUBLIC_LIVEKIT_API_KEY` / `EXPO_PUBLIC_LIVEKIT_API_SECRET` /
  `EXPO_PUBLIC_RESEND_API_KEY` / `EXPO_PUBLIC_R2_ACCESS_KEY_ID` /
  `EXPO_PUBLIC_R2_SECRET_KEY` / `EXPO_PUBLIC_TURSO_TOKEN` must **not** exist in
  `.env`. Delete them.
  **Be precise about why, because this bullet used to get it wrong:** Expo
  inlines an `EXPO_PUBLIC_*` value only where code *references* it. An
  unreferenced var sits in `.env` doing nothing and never reaches a bundle. So
  the danger is never the file on its own — it is a `process.env.EXPO_PUBLIC_…`
  read that outlives the secret it was written for. That is exactly what #995
  found: the Resend key had been "moved server-side" in #393, but
  `app/_layout.tsx` still read it, so it alone kept getting inlined while the
  others were inert. Grep for the read, not just for the var.

**Install channel (#1443) — leave `POLLIS_RELEASE_KEY` unset (or `upload`).**
Self → App → Update opens where the install updates from, and the build decides
which: `app.config.js` writes `extra.installChannel` = `sideload` when
`POLLIS_RELEASE_KEY=sideload` is in the **environment**, else `store` when
`POLLIS_OTA=production`, else `dev`. The copy that counts is the one
expo-constants embeds during the **native** build (Gradle preBuild →
`assets/app.config`; the Xcode phase → `EXConstants.bundle/app.config`), so the
env of the Gradle/Xcode step decides it, and an OTA update cannot change it
(`lib/buildInfo.ts` reads that embedded copy, never `Constants.expoConfig`).
`store` opens the Play listing on Android, and on iOS TestFlight — one IPA
serves TestFlight and the App Store, so no build can tell them apart — until
`IOS_APP_STORE_ID` in `lib/installChannel.ts` is set to the App Store record's
Apple ID (JS, so an OTA carries it). `sideload` opens pollis.com/android, `dev`
pollis.com. Pass the sideload key as an env var, never as `-PPOLLIS_RELEASE_KEY`:
Gradle would sign with it but the config would not see it. The channel is
stripped from the fingerprint (`fingerprint.config.js`), so it never moves a
runtime version. Binaries from before 1.0.17 embed no channel: Android ones
open pollis.com/android (which links Play and the APK), iOS ones the Apple
destination. Check it in every binary (commands below; the APK workflow
asserts `sideload`).

### iOS

Whole path, verified end-to-end 2026-08-21 (Xcode 26.4.1, iPhoneOS SDK 26.4) —
`Pollis.ipa`, 31 MB, signed `Apple Distribution: Daniel Kral (9JF7WWYMU2)`:

```bash
# 1. Rust core → xcframework. RUN FROM modules/pollis-native, not mobile/ —
#    ubrn resolves package.json from the CWD, and mobile/package.json has no
#    `repository` field, so from the app root it panics instead of building.
cd mobile/modules/pollis-native
uniffi-bindgen-react-native build ios --config ubrn.config.yaml --and-generate \
  --release --no-sim
# `--release` matters: the debug staticlib is 700 MB against 140 MB.
# `--no-sim` builds only the ios-arm64 slice a store build needs; it also LEAVES
# the xcframework without simulator slices, so re-run without it before going
# back to a simulator build.

# 2. Native project + pods (`--ignore-workspace`: from mobile/ a bare
#    `pnpm install` installs the ROOT workspace instead, and the archive then
#    dies in Metro on a missing module such as react-i18next)
cd ../..
pnpm install --ignore-workspace --frozen-lockfile
export POLLIS_OTA=production   # OTA updates on; also needed by the archive's build phase
pnpm expo prebuild -p ios --no-install
cd ios && pod install && cd ..

# 3. Archive UNSIGNED, then sign on export.
xcodebuild -workspace ios/Pollis.xcworkspace -scheme Pollis \
  -configuration Release -destination 'generic/platform=iOS' \
  archive -archivePath build/Pollis.xcarchive \
  DEVELOPMENT_TEAM=8JT3WH45NZ CODE_SIGNING_ALLOWED=NO
# 4. Give the unsigned app AND its extension their entitlements, or push is
#    silently lost (below). Inner bundle first: signing the app seals PlugIns/.
codesign -f -s - --entitlements ios/PollisNotificationService/PollisNotificationService.entitlements \
  build/Pollis.xcarchive/Products/Applications/Pollis.app/PlugIns/PollisNotificationService.appex
codesign -f -s - --entitlements ios/Pollis/Pollis.entitlements \
  build/Pollis.xcarchive/Products/Applications/Pollis.app
xcodebuild -exportArchive -archivePath build/Pollis.xcarchive \
  -exportOptionsPlist store/ExportOptions.plist -exportPath build/export \
  -allowProvisioningUpdates \
  -authenticationKeyPath ~/.appstoreconnect/private_keys/AuthKey_$ASC_KEY_ID.p8 \
  -authenticationKeyID "$ASC_KEY_ID" -authenticationKeyIssuerID "$ASC_ISSUER_ID"
# ASC_KEY_ID / ASC_ISSUER_ID: doppler secrets get … -p pollis -c prd_prod --plain
```

**Authenticate the export with the ASC API key, not Xcode's Apple ID
session.** The session silently expires; on 2026-10-02 the export failed with
`No Accounts` / `No signing certificate "iOS Distribution" found` until the
`-authenticationKey*` flags were added. With them, cloud signing mints the
distribution cert from the API key alone.

`CODE_SIGNING_ALLOWED=NO` on the archive is deliberate: automatic *archive*
signing wants a development profile, and the team has zero registered devices,
so it fails. Signing at export instead needs only a distribution cert, which
`-allowProvisioningUpdates` cloud-mints with the ASC API key above (no EAS
account exists, deliberately — `eas.json` is vestigial).

**Step 4 is load-bearing.** An unsigned archive carries no entitlements, and
the export signs with only the entitlements the archived binary *requests* —
it does not copy them from the profile. So `aps-environment` (which the store
profile allows) silently vanished from the 2026-08 and first 2026-10 IPAs, and
the app would have shipped unable to receive push. Ad-hoc signing (`-s -`) with
`Pollis.entitlements` records the request; the export re-signs with the
distribution cert and maps `development` → `production` from the profile. No
local distribution identity is needed — it is cloud-managed.

`store/ExportOptions.plist` (committed — `ios/` is generated, so it can't live
there) sets `method: app-store-connect`, `teamID: 8JT3WH45NZ`,
`uploadSymbols: true`. Upload the resulting `.ipa` with Xcode Organizer or
`xcrun altool`/Transporter.

Check the exported `.ipa` before an upload — all held on the 2026-10-02 build
(1.0.0 / 1000000, the first ever uploaded):

```bash
unzip -q build/export/Pollis.ipa -d /tmp/ipa
codesign -dvvv /tmp/ipa/Payload/Pollis.app          # Authority=Apple Distribution
codesign -d --entitlements :- /tmp/ipa/Payload/Pollis.app   # get-task-allow=false, aps-environment=production, app group + 8JT3WH45NZ.com.pollis.shared
codesign -d --entitlements :- /tmp/ipa/Payload/Pollis.app/PlugIns/PollisNotificationService.appex   # same app group + keychain group
/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' /tmp/ipa/Payload/Pollis.app/Info.plist
strings /tmp/ipa/Payload/Pollis.app/main.jsbundle | grep -o 'https://api[a-z.-]*pollis.com'
jq -r .extra.installChannel /tmp/ipa/Payload/Pollis.app/EXConstants.bundle/app.config   # store
```

The bundle-URL check is the one people forget: it is the only direct proof the shipped
JS bundle inlined the **prod** DS and not api-dev.

Upload with the ASC API key (`~/.appstoreconnect/private_keys/AuthKey_<id>.p8`,
id + issuer in Doppler `prd_prod`):

```bash
xcrun altool --upload-app -f build/export/Pollis.ipa -t ios \
  --apiKey "$(doppler secrets get ASC_KEY_ID -p pollis -c prd_prod --plain)" \
  --apiIssuer "$(doppler secrets get ASC_ISSUER_ID -p pollis -c prd_prod --plain)"
```

#### Notification Service Extension (#1599)

`plugins/withNotificationServiceExtension.js` adds the `PollisNotificationService`
app extension (`com.pollis.ios.NotificationService`, Swift in
`plugins/notification-service/`, deployment target = the app's). It links
`libpollis_core.a` from the same xcframework the app uses and calls
`pollis_notify_fetch(<App Group>/pollis, h, 20000)` off the main thread. The
push carries `aps.mutable-content: 1` (the DS sets Expo's `mutableContent` for
tokens registered with `caps: ["rich"]`) and `body.h`, where expo-notifications
puts `data`. What it shows:

| fetch result | banner |
|---|---|
| ok, mode `full` | title = sender (group: "sender · conversation"), body = preview or a localised placeholder for `body_key`, `threadIdentifier` = conversation id, and a Messages-style communication notification with the app's letter avatar (a circle for the sender, a rounded square for a group) |
| ok, mode `name` | the same title, body "Sent a message" |
| ok, mode `none`, nothing new, or every item muted | localised "New message / You have a new message" |
| `ok:false` (not logged in, lock busy, timeout, …), no App Group, `serviceExtensionTimeWillExpire` | the push exactly as it arrived |

A muted item still gets the generic text, not silence: dropping a push needs
Apple's `com.apple.developer.usernotifications.filtering` entitlement, which we
do not have. The DS already skips muted channels, so this only happens when a
fetch catches up on a muted conversation alongside the one that pushed. The
intent carries no message text, so the system's interaction store never holds
any. `userInfo` is never touched, so a tap routes by `h` exactly as before and
#1508 still finds the notification by `data.h`. Rich content starts only
after the first PIN unlock on a build that has the extension, because that
unlock escrows the DB key for the extension (pollis-core). Until then the fetch
fails and the push shows as it arrived. Strings come from
`mobile:push.*` in the shared catalogues (the same keys as the Android handler); the plugin writes
`<lproj>/Localizable.strings` at prebuild, so the extension follows the iOS
language, not the in-app choice.

Both targets get the App Group `group.com.pollis.ios` and the keychain access
group `8JT3WH45NZ.com.pollis.shared`. That is the team spelled out, because the
unsigned archive leaves `$(AppIdentifierPrefix)` empty. Both are also written to
the Info.plists as `PollisAppGroup` / `PollisKeychainAccessGroup`. The app also
gets `com.apple.developer.usernotifications.communication` and
`NSUserActivityTypes: [INSendMessageIntent]`. `modules/pollis-native/ios/PollisAppGroup.mm`
calls `pollis_set_data_dir_override` and `pollis_set_keychain_access_group` in
`+load`, before JS can start the core. `init_pollis` then moves the old
`Library/Application Support/pollis` into the container once. The binary
advertises `rich` only through its **embedded** `extra.richPush.ios`
(`lib/push/caps.ts`), so an OTA bundle cannot claim it for an older binary.

**Developer-portal setup a store build needs first** (team `8JT3WH45NZ`, by
hand, once):

1. Identifiers → App Groups → register `group.com.pollis.ios`.
2. Identifiers → `com.pollis.ios` → enable **App Groups** (assign the group
   above) and **Communication Notifications**. Push stays on. Existing profiles
   for it become invalid; that is expected, because export re-mints them.
3. Identifiers → register App ID `com.pollis.ios.NotificationService`
   (explicit) with **App Groups** (same group). Keychain Sharing needs no
   capability: a `<team>.<suffix>` group is allowed for every App ID of the team.
4. Nothing in App Store Connect. The extension ships inside the app's record.
   No new app record or version is needed.

Then the export (step 4 above, both `codesign` lines) signs both bundles.
`-allowProvisioningUpdates` with the ASC key mints an App Store profile per
bundle id. It can probably also register a missing App ID or capability, but
that is not verified, so do 1–3 by hand. Until they are done, build with
`POLLIS_IOS_NSE=off` for the prebuild: no extension, no new entitlements, and no
`rich` capability. That binary behaves exactly like 1.0.23. Simulator builds
need none of this. The sim neither enforces App Groups nor needs a profile.

Size: the extension statically links the core a second time, but dead-stripped
to what `pollis_notify_fetch` reaches. On the 2026-10-29 Release simulator
build that came to 10.8 MB of binary: 6.8 MB `__TEXT`, 0.6 MB `__DATA_CONST`
and 48 KB `__DATA`. Code pages are clean and file-backed, so they do not count
against the ~24 MB extension memory limit. What counts is the fetch's heap
(SQLCipher cache, MLS state, TLS), and that is not measured yet: see "Testing
the extension".

**Testing the extension.** `xcrun simctl push` (Xcode 27) posts the request
straight to the notification center through CoreSimulatorBridge and never
starts a service extension. The log shows `mutableContent: YES` on the trigger,
but no `PollisNotificationService` process. Only a real APNs delivery runs it.
So the Swift is tested by `plugins/notification-service/harness/run.sh`, which
compiles the real `NotificationService.swift` for macOS against a stub core and
checks every row of the table above. `tests/notification-service-extension.test.ts`
covers the plugin.

**Avatars.** `LetterAvatar.swift` draws the app's letter avatar with CoreGraphics:
the initial in bold at 0.38 of the size, `semantic.dim` on `semantic.high`,
using the `theme/derive.ts` recipe (the same one as Android's
`LetterAvatar.kt`). The colours come from the live accent and background.
`components/theme.tsx` writes them on start and on every change through
`PollisPushState.setThemeColors`
(`modules/pollis-native/ios/PollisPushState.mm`) into the App Group's
UserDefaults, as `pollis.theme.accent` and `pollis.theme.bg`; without them the
defaults are used. The intent is a combination iOS will donate: sender,
recipients (this user, plus the sender for a group) and the conversation id.

The extension logs outcomes only (mode, reason, item count, ms and `peakKB`, the
process's peak physical footprint), never content, under subsystem
`com.pollis.ios.NotificationService`:
`log stream --predicate 'subsystem == "com.pollis.ios.NotificationService"'`.
<!-- measured numbers: see PR #1599-ios-nse -->

**Deployment target: iOS 16.4.** Expo SDK 56 raised the minimum
(`ExpoModulesCore.podspec` requires 16.4), so the app **no longer supports
iOS 15.x–16.3**; Android is unaffected. The app side is the top-level
`expo.ios.deploymentTarget: "16.4"` in `app.json`, which prebuild feeds to the
generated `ios/Podfile` (`platform :ios`). The Rust side is
`modules/pollis-native/ubrn.config.yaml`, which pins
`IPHONEOS_DEPLOYMENT_TARGET="16.4"` via its ios `cargoExtras`
(`cargo --config env.…`); the two must stay in lockstep. Without the Rust pin,
rustc targets iOS 10.0 while cc-rs builds the
vendored OpenSSL 3.5 + SQLCipher against the current SDK, and the
`libpollis_core.dylib` link dies on `___chkstk_darwin` (a libSystem stub that
only exists from iOS 12). It bites **only in release** — a debug link at 10.0
succeeds — which is why it can sit latent until someone cuts a store build.

The pin used to live in the global `[env]` table of `.cargo/config.toml` and
must never return there: Apple clang infers the target *platform* from
whichever deployment-target env var is set when a compile has no explicit
`-target`, so the global pin retargeted the macOS **desktop** build to iPhone
and broke the v1.10.2 release inside webrtc-audio-processing-sys's meson
probe. Cargo has no `[target.<triple>.env]` to scope it (that spelling
silently parses as a build-script `links` override for a native library named
"env" — three inert blocks in that file made the same mistake until #1010
removed them). `mobile-core-check.yml`'s ios-check job
reads the pin from ubrn.config.yaml, exports it for its own cargo build, and
asserts the min-version reached the built artifact — so ubrn.config.yaml stays
the single source of truth CI reads. Keep it in lockstep with `app.json`.

**Encryption export compliance:** `app.json` sets
`ITSAppUsesNonExemptEncryption: false`, and that is the current, deliberate
answer — `docs/store-listing.md` §5 is the source of truth. The key asks about
Apple's *documentation* exemption, not whether the app encrypts: Pollis uses
only standard published algorithms (MLS RFC 9420, AES-GCM, ML-DSA-44), has no
proprietary crypto and is not distributed in France, so no export
documentation is uploaded. It flips to `true` plus an
`ITSEncryptionExportComplianceCode` only when France is added (after the ANSSI
declaration). The BIS annual self-classification report (due Feb 1) is owed
regardless. An earlier version of this paragraph said the opposite; it was
written before the §5 decision and is superseded.

### Android

```bash
cd mobile
source android-env.sh
pnpm install --ignore-workspace --frozen-lockfile
# Rust core for every Android ABI, release profile
(cd modules/pollis-native && uniffi-bindgen-react-native build android \
  --config ubrn.config.yaml --and-generate --release)
export POLLIS_OTA=production   # OTA updates on (prebuild AND gradle)
pnpm expo prebuild -p android --no-install
cd android && ./gradlew :app:bundleRelease
# → android/app/build/outputs/bundle/release/app-release.aab
# Native libs for armeabi-v7a / arm64-v8a / x86_64 ONLY — pinned by
# `expo-build-properties` android.buildArchs in app.json. React Native
# defaults to also building 32-bit x86, which pollis-core is not compiled
# for: the bundle shipped an x86 slice with no Rust core, which Play would
# serve to x86-only devices to crash at launch. Check after every build:
#   unzip -l <aab> | grep -o "base/lib/[a-z0-9_-]*" | sort -u   # 3 entries  (~180 MB, 4 ABIs)
```

**Permissions and shrinking (1.0.13).** Release builds run R8 with resource
shrinking (`expo-build-properties` `enableMinifyInReleaseBuilds` /
`enableShrinkResourcesInReleaseBuilds`; Play had rated 1.0.11's 65.5 MB of DEX
"Low"). `android.blockedPermissions` strips what libraries add but the app never
uses: the LiveKit/WebRTC set (SYSTEM_ALERT_WINDOW, FOREGROUND_SERVICE, Bluetooth,
MODIFY_AUDIO_SETTINGS — voice is not wired on mobile, #343), expo-secure-store's
biometric pair (no biometric auth is requested), DUMP, and sixteen vendor
launcher-badge permissions (no badge count is ever set). Storage stays: saving
to the library on Android 7–9 needs WRITE_EXTERNAL_STORAGE. When voice is built,
unblock its permissions in the same PR. `tests/android-permissions.test.ts`
pins both lists and the R8 flags. Check the merged manifest after a dependency
bump — Play's bundle explorer lists every permission a build carries.

Verify, then upload with `scripts/play-publish.py` (Play Developer API through
the `play-publisher@pollis.iam.gserviceaccount.com` service account; key in
Doppler `PLAY_SERVICE_ACCOUNT_JSON`, copy in 1Password):

```bash
jarsigner -verify -certs android/app/build/outputs/bundle/release/app-release.aab  # CN=Daniel Kral, OU=Pollis
unzip -p android/app/build/outputs/bundle/release/app-release.aab base/assets/index.android.bundle \
  | strings | grep -o 'https://api[a-z.-]*pollis.com'                              # prod DS only
unzip -p android/app/build/outputs/bundle/release/app-release.aab base/assets/app.config \
  | jq -r .extra.installChannel                                                    # store
scripts/play-publish.py upload --track alpha --status draft --name "1.0.0 (1000000)"
scripts/play-publish.py listing --icon <512px.png> [--feature-graphic <1024x500.png>] [--phone <shots>]
```

`listing` pulls the title/short/full description from `docs/store-listing.md`
(the same parser the ASC metadata script uses). Every run is one Play *edit*:
committed on success, discarded on any error. The service account needs, per
app, *Release to testing tracks*, *Manage testing tracks*, and *Manage store
presence* for `listing`. Without that last one the edit commits are refused
with a 403. The first `upload` (2026-10-02, versionCode 1000000, closed track
`alpha`, draft) also enrolled the app in Play App Signing with the upload key.

Release signing is wired by `plugins/withReleaseSigning.js` (registered in
`app.json`), a local Expo config plugin that patches the generated
`android/app/build.gradle` at prebuild. `signingConfigs.release` picks one of
two keys by `POLLIS_RELEASE_KEY` (gradle property or env; default `upload`) and
reads only that key's own `POLLIS_<KEY>_STORE_FILE` / `_STORE_PASSWORD` /
`_KEY_ALIAS` / `_KEY_PASSWORD`:

- **`upload`** (default) — the Play upload key, `POLLIS_UPLOAD_*` from
  `~/.gradle/gradle.properties` or the environment. **Falls back to the debug
  keystore when unset**, so CI's `assembleRelease` keeps working with zero
  config. Generate it once with `scripts/generate-upload-keystore.sh`
  (RSA-4096 at `~/.pollis/pollis-upload.jks`, refuses to overwrite); it prints
  the gradle.properties lines.
- **`sideload`** — the pollis.com APK key, `POLLIS_SIDELOAD_*` (below). **No
  fallback**: unset, the Gradle configuration fails.

Separate prefixes are load-bearing, not tidiness: gradle properties beat
environment variables, so with one shared set of names a workstation whose
`gradle.properties` holds the upload key would sign a "sideload" build with
it. `tests/release-signing.test.ts` pins the generated block. Never commit a
keystore (`*.jks` is gitignored) or its passwords.

### Android sideload APK (pollis.com/android)

For people who will not use Google Play: **one signed APK per ABI**
(`arm64-v8a`, `armeabi-v7a`, `x86_64` — pollis-core's ubrn targets) published by
`.github/workflows/mobile-apk-release.yml` to
`cdn.pollis.com/releases/android/v<version>/pollis-v<version>-android-<abi>.apk`
(plus `releases/android/latest/pollis-latest-android-<abi>.apk` aliases and
`releases/android/latest.json`) and a GitHub Release, linked from
`website/android.html`.

**Why per-ABI, not universal:** Android stores native libs uncompressed, and
pollis-core is 24-34 MB per ABI. Measured 2026-10-02: a universal APK is
~261 MB (~210 MB even without the 32-bit x86 libs RN builds by default), the
arm64-v8a split ~90 MB. `plugins/withAbiSplits.js` adds a Gradle `splits.abi`
block that is **off unless `POLLIS_ABI_SPLITS=true`**, so no other build
changes; the release also passes
`-PreactNativeArchitectures=arm64-v8a,armeabi-v7a,x86_64`, because RN's default
list includes x86, for which pollis-core is never built — an x86 APK would
install and crash loading the core.

- **Own key.** `~/.pollis/pollis-sideload.jks`, alias `pollis-sideload`,
  RSA-4096 / 10000 days, `CN=Pollis Android Sideload, OU=Pollis, O=Pollis`.
  Keystore (base64) + passwords live in Doppler `pollis`/`prd_prod` as
  `POLLIS_SIDELOAD_KEYSTORE_B64` / `_STORE_PASSWORD` / `_KEY_ALIAS` /
  `_KEY_PASSWORD`, which Doppler syncs to the repo's Actions secrets. It is
  never the Play upload key. Play re-signs what it serves with Google's
  app-signing key, so a Play install and a sideloaded install have different
  signatures and **cannot update each other**: switching channels means an
  uninstall, and the new install starts empty (loss #2 in the root CLAUDE.md).
  The download page says so.
- **Pinned certificate.** `store/android-sideload-cert.sha256` holds the cert's
  SHA-256 (lowercase hex, what `apksigner verify --print-certs` prints). The
  workflow fails if the APK's signer differs, and if `website/android.html`
  does not show the same value. Rotating the key = new keystore + Doppler
  values + this file + the page, in one PR — and every existing sideload user
  has to uninstall, so do not.
- **Release:** bump `package.json` `version`, merge, then tag
  `mobile-v<version>` on that commit and push the tag. The tag must equal
  `mobile-v` + `package.json` `version` or the run fails. A
  `workflow_dispatch` on a branch is a dry run (build + every check, APK as a
  run artifact, publishes nothing); dispatched on the tag it re-publishes.
  `POLLIS_BUILD` is the dispatch input `build` (default 0), same derivation as
  the store builds. `mobile-v*` cannot match the desktop's `v*` glob, so
  mobile and desktop never fire each other.
- **CI checks every APK before anything is published:** `apksigner` v2
  signature with exactly one signer = the pinned cert; `aapt2 dump badging`
  shows `com.pollis.mobile`, `versionName` = `package.json`, `versionCode` = the
  `app.config.js` derivation, not debuggable; native libs for exactly its own
  ABI, `libpollis-native.so` included; and the raw bytes of `assets/index.android.bundle` contain `api.pollis.com`
  and not `api-dev.pollis.com` (Hermes bytecode — `strings` can miss it); and
  `assets/app.config` embeds `extra.installChannel: "sideload"` (#1443 — Self →
  App → Update then opens this page, not Play). After
  upload it re-downloads both CDN URLs and `latest.json` and compares hashes.
- **No native auto-update.** Nothing in the app checks for a newer APK; users
  come back to the page for native changes. JS-only changes reach sideloaded
  installs over the air like any other prod build (the workflow builds with
  `POLLIS_OTA=production`; see [OTA updates](#ota-updates-1250)). Push still goes
  through Expo → FCM, so it needs Play services on the phone; the page says
  that too.

Local build, same steps as the workflow (no `mobile/.env` needed — the
`EXPO_PUBLIC_*` values come from the environment, which wins over `.env`):

```bash
cd mobile
source android-env.sh
pnpm install --ignore-workspace --frozen-lockfile
(cd modules/pollis-native && uniffi-bindgen-react-native build android \
  --config ubrn.config.yaml --and-generate --release)
export POLLIS_OTA=production
pnpm expo prebuild -p android --no-install --clean
export POLLIS_RELEASE_KEY=sideload POLLIS_SIDELOAD_STORE_FILE=~/.pollis/pollis-sideload.jks
export POLLIS_SIDELOAD_KEY_ALIAS=pollis-sideload
export POLLIS_SIDELOAD_STORE_PASSWORD="$(doppler secrets get POLLIS_SIDELOAD_STORE_PASSWORD -p pollis -c prd_prod --plain)"
export POLLIS_SIDELOAD_KEY_PASSWORD="$POLLIS_SIDELOAD_STORE_PASSWORD"
export EXPO_PUBLIC_POLLIS_DELIVERY_URL=https://api.pollis.com POLLIS_ABI_SPLITS=true
(cd android && ./gradlew :app:assembleRelease \
  -PreactNativeArchitectures=arm64-v8a,armeabi-v7a,x86_64)
"$ANDROID_HOME/build-tools/36.0.0/apksigner" verify --print-certs \
  android/app/build/outputs/apk/release/app-arm64-v8a-release.apk   # digest = store/android-sideload-cert.sha256
```

(The key is PKCS12, so its key password is the store password.)


## OTA updates (#1250)

The app updates its own JS and assets without prompting: on every launch
expo-updates asks `api.pollis.com/updates/api/manifest` in the background
(`checkAutomatically: ON_LOAD`, `fallbackToCacheTimeout: 0` — launch is never
held), downloads a newer update if there is one, and runs it on the **next
cold start**. There is no UI. Our own server speaks the Expo Updates protocol;
there is no EAS Update and no EAS account involved.

**What it can ship:** anything in the JS bundle and its assets — screens,
copy, hooks, styling, the bridge *calls* (not the bridge). **What it cannot:**
anything native — pollis-core (Rust), a new native module or version bump,
a config plugin, permissions, entitlements, icons, `app.json` native fields.
Those change the runtime version, so they need a store build; an update built
afterwards targets only the binaries built afterwards.

### How it fits together

- **App** (`app.config.js`): `updates` is on only when the build sets
  `POLLIS_OTA=production` (store builds, the sideload APK workflow). Then it
  has the URL, `ON_LOAD`/`0`, the channel header `expo-channel-name:
  production`, and `codeSigningCertificate: ./store/ota-code-signing.pem` +
  `codeSigningMetadata {keyid: main, alg: rsa-v1_5-sha256}` — the app refuses
  any unsigned or wrongly signed manifest or directive. **No per-install
  identifier on update requests:** expo-updates puts a random per-install UUID
  in `EAS-Client-ID` on every manifest and asset request (and the last fatal
  JS error in `Expo-Fatal-Error`); `requestHeaders` overrides them with a
  constant all-zero UUID and `""`. This works because both native
  FileDownloaders apply `requestHeaders` after their own headers (a test pins
  that ordering in the installed expo-updates — re-check it on an SDK bump).
  What remains is per-release (runtime version, current/embedded/failed update
  ids), the platform, and the OS HTTP client's generic User-Agent. Every other build
  (dev client, Maestro's api-dev Release builds, CI's debug APK) gets
  `enabled: false` and **no URL**, so it cannot fetch anything: a dev build
  never takes a prod update, and nothing publishes a dev update a prod build
  could take. `POLLIS_OTA=production` without the certificate, or with an
  api-dev DS in the environment, fails the config. `tests/ota-config.test.ts`
  pins all of it.
- **Runtime version** = the native fingerprint (`runtimeVersion.policy:
  "fingerprint"`), computed by `@expo/fingerprint` **locally**: expo-updates'
  Xcode phase / Gradle plugin bakes it into the binary at build time, and the
  publish script computes the same function. Confirmed working with no EAS
  (and identical with/without the generated ubrn files present).
  `fingerprint.config.js` (a) adds a hash of every git-tracked file of
  pollis-core and of each crate it reaches by `path =` (computed, so a new
  crate dependency is covered automatically), plus `Cargo.lock`,
  `rust-toolchain.toml`, `.cargo/config.toml` and the ubrn version — so any
  core change means a new store build before an update can land; (b) ignores
  ubrn's generated output (`cpp/`, `src/`, the xcframework, jniLibs), which
  exists only where `ubrn build` ran; (c) skips the version fields and the
  `updates` block, which move without the native code moving; (d) hashes the
  code-signing certificate, so a rotated certificate is a new runtime version
  and old binaries keep their own, old-key-signed pointers; (e) hashes
  node_modules files that **Gradle rewrites in place** as Gradle leaves them,
  and ignores build output tools leave inside packages (#1427). A store build
  runs Gradle/Xcode and the OTA job does not, so without (e) one commit had two
  runtime versions: masked-view's `build.gradle` strips `package=` from its
  own `AndroidManifest.xml` in node_modules, so the Play 1.0.14 binary carries
  `5466ba19…` while the OTA job computed `35e62c3e…` and its updates reached
  nobody. Likewise the npm `ubrn` bin compiles itself into
  `node_modules/uniffi-bindgen-react-native/target/`, which iOS hashed. It was
  never Linux vs macOS: a fresh install on a Mac gives CI's value.
  **Gradle bakes the value mid-build** (`createReleaseUpdatesResources`), so a
  file that exists only while Gradle runs counts too: the Kotlin plugin keeps
  `.kotlin/sessions/*.salive` in each included build's root, and
  `expo-updates-gradle-plugin/` is a hashed dir. That gave the sideload APKs
  1.0.14–1.0.16 their own runtime versions (#1429; the 1.0.16 APK `27b59ee3…`
  against Play/OTA `8f1befed…`); `node_modules/**/.kotlin/` is now ignored.
  `tests/ota-fingerprint.test.ts` pins (e) and fails if another installed
  package's `build.gradle` starts writing into its own sources. CI **fails**
  any APK whose baked value a Gradle-free recompute does not reproduce: every
  PR's `android-build` job and every sideload release.
  Consequence: **a pollis-core change blocks OTA for every binary built before
  it** — that is intended.
- **Store-build ledger** (`store/runtime-versions.json`): every shipped binary
  and the runtime version baked into it. `ota-publish.ts build` prints which
  recorded binaries each platform's update reaches, warns (`::warning::`) for
  a platform that reaches none, and **refuses a publish that reaches no
  recorded binary at all** (a dry run passes `--unreached warn` and only
  warns). **After every store build, add its entry** (a PR, before the next
  OTA): Android `unzip -p app-release.aab base/assets/fingerprint` (APK:
  `assets/fingerprint`); iOS `unzip -p Pollis.ipa
  Payload/Pollis.app/EXUpdates.bundle/fingerprint`. The sideload APK workflow
  prints its value and fails on a mismatch (`Check the baked runtime version
  and record its sources`; Gradle's log prints the baked per-source hashes on
  the `createReleaseUpdatesResources` line). The OTA
  `build` job uploads `ota-fingerprint-sources` (the `--debug` sources per
  platform) on every run; when an update misses a binary, diff that against
  `node_modules/.bin/fingerprint fingerprint:generate --platform <p> --debug`
  on the build machine.
- **Runtime-version aliases (#1446).** A binary whose baked value is stray but
  whose native code is identical to another build of the **same platform and
  commit** carries `nativeIdenticalTo: <that build's runtime version>` in the
  ledger — today the sideload APKs 1.0.14/1.0.15/1.0.16 (`cd2015f4…`,
  `a2ebce84…`, `27b59ee3…`), aliased to Play's `5466ba19…`, `3d24cb87…`,
  `8f1befed…` (#1429). `parseStoreBuilds` refuses an alias whose target is not
  a recorded runtime version of that platform and commit, an alias of an
  alias, or a value recorded both as an alias and as a real runtime version.
  `build` then issues each update **also under every alias of its own runtime
  version** — never any other, so an update built for `8f1befed…` reaches Play
  and sideload 1.0.16 but not 1.0.14/1.0.15. An alias is a **separate
  manifest, re-signed**: same bundle, assets and config, its own update id,
  and `runtimeVersion` set to the alias. Copying the original's bytes would not
  work: `runtimeVersion` is inside the signed body, the Worker refuses a
  pointer whose body names another runtime version, and expo-updates launches
  only an update whose `runtimeVersion` equals the binary's own
  (`LauncherSelectionPolicyFilterAware`). Each alias is its own plan entry
  (`aliasOf` in the summary's "alias of" column), live pointer, archive and
  transparency leaves (`arch` = the alias). `sign` re-checks every alias
  against its own checkout's ledger and the target entry (same bytes, only id
  + runtimeVersion differ) and refuses a plan that leaves a recorded alias out.
  The reach check and summary count aliased binaries ("sideload 1.0.16 (alias
  27b59ee3…)"). `prepare-republish` and `prepare-rollback --runtime-version
  <rv>` expand aliases the same way, so an aliased binary rolls back with its
  target. Only record an alias for a build whose native code you know matches
  (same commit, value differing for a reason like #1429) — a wrong alias
  ships JS to a binary it was not built against.
- **Server**: the DS front-door Worker itself (`pollis-delivery/worker/updates.ts`,
  wired in `worker/index.ts`) — no separate project, domain or bucket. Under
  `/updates/` on api.pollis.com it serves `GET /updates/api/manifest`
  (protocol v1, multipart/mixed, routed on `expo-platform` +
  `expo-runtime-version`, 204 when nothing is published) and
  `GET /updates/assets/<sha256>` (content-addressed, immutable), **in the
  Worker, straight from R2**: an update request never reaches the DS container
  or its Durable Object, so it neither wakes the single instance nor queues
  behind message traffic. Storage is the `ota/` prefix of the public release
  bucket `pollis` (binding `UPDATES`, `OTA_CHANNEL=production`, prod config
  only; api-dev binds neither and answers 404 — no dev build enables updates,
  and a dev deploy must not hold a handle on prod's bucket). It passes
  pre-signed bytes and their `expo-signature` through unchanged and **never
  holds the key**: a compromised Worker can withhold or replay, never forge.
  Workers Logs + Logpush off on both DS configs, no logging, no
  client-identifying header read
  (`pollis-delivery/worker-tests/updates-no-client-ip.test.ts`). It ships with
  the normal DS deploy (`delivery-deploy-prod.yml`).
- **Publish** (`.github/workflows/mobile-ota-release.yml`, driven by
  `scripts/ota-publish.ts`): `build` (expo export with prod `EXPO_PUBLIC_*`
  from the environment, `EXPO_NO_DOTENV`, runtime versions, the store-build
  checks: bundle names `api.pollis.com` and never `api-dev`, only the four
  public `EXPO_PUBLIC_*` reads, no secret value in any file) → `sign` in the
  **`ota-signing` environment** (no reviewer — the tag push is the approval; re-derives every hash and
  the prod-DS check from the bytes, bare node with no npm tree, then signs) →
  `publish`: upload bytes + archive, SLSA provenance, **append to the
  transparency log**, only then flip the live pointers, then re-fetch through
  `api.pollis.com/updates/` and verify signature + every hash. The update
  store is written with the release workflows' existing R2 credentials
  (`R2_ACCESS_KEY_ID` / `R2_SECRET_KEY`); the CLI adds the `ota/` prefix.
- **Transparency**: each update is three `binaries`-tree leaves — the signed
  manifest (`ota-manifest`, which commits to the update id, runtime version and
  every asset hash), the launch bundle (`ota-bundle`) and the asset list
  (`ota-assets`) — and each rollback directive one (`ota-directive`); `platform`
  = ios/android, `arch` = the runtime version, release tag
  `mobile-ota-<group id>`. `pollis-verify release https://verify.pollis.com
  mobile-ota-<group>` checks them like a desktop release.

### Keys

- **Private key**: generated once by the owner with
  `scripts/generate-ota-signing-key.sh` (writes `~/.pollis/ota-signing/`,
  refuses to overwrite, proves the pair, prints the next steps). It goes to
  Doppler as `OTA_CODE_SIGNING_KEY`, synced **only** to the `ota-signing`
  environment secret, plus an offline copy; then it is deleted locally. Never
  in the repo, never a repository-level secret, never on the DS or its Worker.
- **Certificate**: `store/ota-code-signing.pem` (the one `*.pem` that
  `.gitignore` lets through), compiled into every prod build. 20-year
  validity — expo-updates checks it, and an expired certificate silently ends
  OTA for every build carrying it. Rotation = new key + new certificate + a
  store build; the certificate is part of the fingerprint, so the new build has
  a new runtime version, and builds carrying the old certificate keep taking
  only old-key-signed updates (or none, once the old key is retired). If the
  key leaked: `docs/signing-key-compromise-runbook.md` → "OTA code-signing key".

### Runbook

```bash
# Publish the JS on a commit that is on main (signs after approval):
git tag mobile-ota-$(date -u +%Y%m%d)-1 <commit> && git push origin mobile-ota-…
# Dry run on any branch (build + every check, nothing signed or published):
gh workflow run mobile-ota-release.yml --ref <branch> -f action=dry-run
# Roll back to a known-good update (new ids, old bytes), from main:
gh workflow run mobile-ota-release.yml --ref main -f action=republish -f from_group=<group id>
# Roll back to the store binary's embedded bundle (panic button), from main:
gh workflow run mobile-ota-release.yml --ref main -f action=rollback-to-embedded \
  -f platform=all -f runtime_version=all
# Check what a phone gets right now (signature + every hash):
cd mobile && node scripts/ota-publish.ts verify --platform ios --runtime-version <rv>
```

Every operation runs unattended in the `ota-signing` environment (refs limited to
`main` and `mobile-ota-*` tags, no reviewer) and is logged before it goes live. The group id is in each run's summary and in `ota/groups/` in
the `pollis` bucket. Rolling forward after a rollback is simply the next publish (a newer
`createdAt` wins). The client keeps a crashing update from bricking the app on
its own (expo-updates rolls back to the previous update if one fails before
its first render), but a bug that renders fine needs one of the two rollbacks
above. Each step runs by hand too (`node scripts/ota-publish.ts help`); the
private key is only ever handed to `sign`.

**First OTA-capable build:** expo-updates is a native module, so the first
store build that can receive updates is the next one cut after this lands
(**1.0.2**), built with `POLLIS_OTA=production` and the certificate committed.
Nothing published before then reaches anyone; 1.0.0/1.0.1 never check. The
update routes go live with the first DS prod deploy after this lands (they
answer 204 until something is published); there is no other server to set up.

---

## Rough edges — technical

### 1. `uniffi-bindgen-react-native` CLI ↔ npm package coupling

The Rust CLI binary (from `cargo install --git`) generates C++ that calls helpers shipped in the npm package (`mobile/node_modules/uniffi-bindgen-react-native/cpp/includes/`). They **must be the same version**, or the native build fails with errors like `no member named 'string_to_buffer'` — the generated code references a helper (`UniffiString.h`, ubrn PR #378) that the other side's headers do not have. It looks like a React Native header problem and is not one.

If either side updates, rebuild the CLI from the exact git tag matching the npm version:

```bash
cargo install --git https://github.com/jhugman/uniffi-bindgen-react-native \
  --tag 0.31.0-6 --locked uniffi-bindgen-react-native --force
```

Currently pinned: **`0.31.0-6`**, in places that move together — `uniffi-bindgen-react-native` **and `@ubjs/core`** in `mobile/package.json` and `modules/pollis-native/package.json`, and the CLI tag (dev loop + CI workflows). `@ubjs/core` is the TS runtime that code generated by 0.31.0-5+ imports (older generated code imported it from `uniffi-bindgen-react-native`); without it the Release build compiles every native target and then fails in Metro on `Unable to resolve module @ubjs/core`. ubrn 0.31.0-5+ accepts any uniffi 0.31.x, so `pollis-core` stays on uniffi 0.31. **Avoid `0.31.0-3`** (its Android CMake setup breaks on a `require.resolve` bug). If you install the CLI from `main` without a tag, you'll hit the mismatch within a commit or two.

### 2. `android/android/…` double-nesting in `jniLibs` path

ubrn's CMakeLists resolves `${CMAKE_SOURCE_DIR}/android/src/main/jniLibs/` to `mobile/modules/pollis-native/android/android/src/main/jniLibs/`. It works — the ubrn config + generated CMake agree. Don't try to flatten it without regenerating both sides.

### 3. No justfile/Makefile yet

The "regenerate + gradle + install + launch" loop is manual four-command sequence. If iteration frequency rises, add a justfile.

### 4. NDK version pinning

RN 0.86.3 pins NDK **27.1.12297006** (r27b) — unchanged since RN 0.83, as are compileSdk 36 and AGP 8.12. Arch AUR's `android-ndk` package ships r29 at `/opt/android-ndk`. Both coexist: r29 at `/opt/android-ndk` (used by cargo-ndk), r27b at `/opt/android-sdk/ndk/27.1.12297006/` (used by gradle). Removing either breaks one side.

### 5. Arch AUR `android-sdk` is root-owned

The AUR `android-sdk` package installs to `/opt/android-sdk` as root-owned read-only. Gradle needs write access (licenses, package auto-install). Fix, done once:
```bash
sudo chown -R $USER:$USER /opt/android-sdk /opt/android-ndk
```
Redo if you reinstall the AUR package.

### 6. Old sdkmanager broken on JDK 17

`/opt/android-sdk/tools/bin/sdkmanager` uses `javax.xml.bind` (removed in JDK 9+) and crashes. Use the modern cmdline-tools at `/opt/android-sdk/cmdline-tools/latest/bin/sdkmanager` — installed manually from [commandlinetools-linux-11076708_latest.zip](https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip).

### 7. Expo SDK upgrade = re-pin every sibling package by hand

Expo packages version independently but their `latest` dist-tag tracks the current SDK. Bumping `expo` alone leaves sibling packages at the old SDK's versions, producing weird Kotlin compile errors in `expo-dev-menu` et al. **Do not use `expo install --fix` (or `expo install <pkg>`) to fix it** — the Expo CLI shells out to `pnpm` without `--ignore-workspace`, which hoists mobile's tree into the root `node_modules` (see "Project isolation"). Instead, read the SDK's version map from the new `expo` package and copy it into `package.json` by hand:

```bash
cd mobile && cat node_modules/expo/bundledNativeModules.json
```

then reinstall (#8). `pnpm dlx expo-doctor` afterwards names anything still off.

### 8. After an SDK bump, delete `node_modules` AND the lockfile

pnpm treats whatever is already in `node_modules` as preferred versions, so after an SDK bump stale old-SDK peers survive the reinstall — on the 55 → 57 bump `@expo/metro-runtime` 55 and `@expo/dom-webview` 55 survived **even a deleted lockfile**, because `node_modules` still had them. Remove both, and reinstall with CI's pnpm (see "Project isolation"):

```bash
cd mobile && rm -rf node_modules pnpm-lock.yaml && npx -y pnpm@10.25.0 install --ignore-workspace
```

### 9. Bridge commands MUST run off the JS thread (small-stack overflow)

uniffi-bindgen-react-native polls exported `async` futures **directly on the
JS/Hermes thread**, which has a small stack (~1 MB iOS, often less on Android).
Some synchronous work we call into needs far more — notably libsql's recursive
SQL parser (`yyParser::yy_reduce`), which runs client-side on **every** query.
On the JS thread it overflows the guard page and hard-crashes the whole app
with SIGBUS (`KERN_PROTECTION_FAILURE` at the stack guard region) on the first
DB query. This bit us at `verify_otp` (the first query in the auth flow) and
reproduced identically on iOS and Android. Desktop never hits it — Tauri runs
commands on a multi-threaded Tokio runtime with generous worker stacks.

Fix (in `pollis-core/src/bridge.rs`): `init_pollis` and `invoke` immediately
hand off to `run_on_worker`, which spawns the real work on a dedicated
multi-threaded Tokio runtime whose worker threads (named `pollis-bridge`) carry
an 8 MB stack; the JS thread only `await`s the join handle. **Any new bridge
entry point that does real work must go through `run_on_worker`** — don't run
command bodies inline on the uniffi-exported future. To confirm the runtime is
live at runtime: `sample <app-pid> 1 | grep pollis-bridge` (note `ps -M`
truncates thread names and won't show them).

---

## App structure (expo-router)

Dark, two-colour (accent + background) UI in Geist, re-skinned in 2026-10 to
match the desktop refined skin: titles and back at the top, grouped lists,
bottom sheets, AA contrast throughout (see "Design tokens").

```
app/
  _layout.tsx          root Stack (every route registered), fonts, splash gate
  index.tsx            redirect → /(auth)/email
  (auth)/              email → otp → pin → initializing (gestureEnabled: false)
  (tabs)/              groups · direct · self (custom <TabBar>; its 3rd cell is the shield shortcut, not a tab)
  group/[id].tsx       one group's channel panel (phones: pushed, no tab bar; iPad: redirects to the Groups tab)
  group/*              new, invite, invite-links, members, settings, emoji, requests, discover
  chat/[id].tsx        conversation (Header + list + composer); chat/thread.tsx
  dm/{new,info,safety,requests}.tsx · conversation/{info,pins}.tsx · user/[id].tsx · report.tsx
  media.tsx            full-screen attachment viewer (#1248, #1492): zoom, swipe pager, swipe-down dismiss, video, audio, save/share
  vault.tsx            the Vault (#1316): chat-with-yourself + media grid, opened from the top of Direct
  self/*               preferences, user-settings, security, link-device, blocked, saved, …
components/
  ui.tsx               primitives: Txt, Screen, Header, IconButton, SectionTitle,
                       ListRow, ActionRow, Group, Card, Field, Avatar, Chip/Pill,
                       Badge, Button, Toggle, Dot, Divider, BottomAction, Body
  chat/SheetOverlay.tsx the one bottom sheet (every *Sheet is built on it)
  icons.tsx            lucide wrapper (Icon.*, stroke 1.75)
  TabBar.tsx           custom bottom tab bar (accent active, unread badges, shield → self/link-device)
  search/GlobalSearchResults.tsx  the global search results Self shows under its field
  auth|chat|direct|groups|search|self/   per-area pieces (rows, sheets, pads)
theme/tokens.ts        semantic / type / fonts / r / space / layout (derive.ts: colour math)
```

Navigation: no native header — every screen draws a `<Header>`
(components/ui.tsx) at the top: a 44×44 back chevron top-left (`btn-back`,
"Back" / "Back to <x>"), title 17/700, actions at the end. Pushed screens
also get the system edge swipe (every push, settings pages included, uses
`drillIn`). Tab roots use `<Header variant="large">` (no back). Leave out
`title` for a back-only bar (auth steps, `group/[id]`, whose heading is in
the body); `backLabel` / `backTestID` override the spoken label and id.
There is no bottom back strip any more (`Ctx`/`Crumb` are gone). Auth
screens disable the edge swipe, so their top-left back is the only way out.
Sub-screens are stack pushes outside `(tabs)`.

Tabs and search (#1550): the bar is Groups · Direct · shield · Self. The
shield (`tab-link-device`, labelled "Link") is a shortcut, not a tab: it
pushes `self/link-device` (Scan to sign in a device, the same page and PIN
confirm as Self → Security) onto the root stack, so back returns to the tab
it was tapped from, and it is never the selected cell. `TabBar` matches cells
to routes by name, never by index, and a second tap on the focused tab calls
`emitTabReselect` (`lib/tabReselect.ts`; Self ends a search on it). Global
search is not a tab: it sits at the top of Self
under the large title, Settings-style — focusing the field or typing swaps
the Self menu for `GlobalSearchResults` in place (no navigation); Cancel, the
clear button, a second tap on the Self tab or Android back restore the menu.
The state survives opening a hit and coming back. A group's "Search <group>"
field navigates to `/(tabs)/self?search=1`, which focuses it.

iPad (regular width, `hooks/useLayoutClass`): Groups and Direct are a
two-pane (`components/MasterDetail.tsx`) — list left, conversation right, tab
bar visible. Conversation pages (info, pins, members, group settings/invite/emoji/
requests, thread, profile, a DM's "You & <name>" safety number — `PANE_PATHS` in `components/pane/paneContext.ts`)
push INSIDE the right pane: the tab owns a small entry stack and `DetailPane`
renders the route's own component. Those screens read params with
`useRouteParams` and navigate with `useNav` (both fall back to
`useLocalSearchParams` / `useRouter` on phones) and pass `onBack={nav.onBack}`
to `<Header>`. Opening a conversation/group from elsewhere goes through
`hooks/useOpenConversation` (phones: push; iPad: select it + return to its tab);
`chat/[id]` and `group/[id]` redirect to the tab on regular width as a net.
A selected group with no conversation open auto-opens its first channel in
the right pane (Groups tab, while focused). Content width on iPad: every
full-screen page and every pane page uses `<Screen>`'s one
`layout.screenMaxWidth` column; only auth steps pass `centered`
(`layout.authMaxWidth`) — tests/screen-widths.test.ts pins this.

Keyboard (#1246): `react-native-keyboard-controller` owns it. `KeyboardProvider`
wraps the root layout; `<Screen>` and `SheetOverlay` use its
`KeyboardAvoidingView` (`behavior="padding"`) on both platforms, and a
`<Screen>` nested in another skips its own. `<Screen>` must NOT pass
`automaticOffset` (#1432): it reads the window position asynchronously, and
on Android that can answer before the screen sits below the status bar, so
the stale frame left every keyboard on the screen a status bar short — the
chat composer hidden behind it, mostly in long conversations. The layout
frame alone is exact, since a `<Screen>` starts at the window's top; only
`SheetOverlay` (inside a Modal, never moves) keeps `automaticOffset`. Never import
`KeyboardAvoidingView` from `react-native` or hand-roll an inset from
`Keyboard` events: the app is edge-to-edge, so `adjustResize` is inert and
`keyboardDidShow` heights come out short. A screen that mounts with the
keyboard already up (email → OTP) seeds from the current keyboard state.
`tests/keyboard.test.ts` pins this.

## Backend integration — wired vs pending

Most of what older notes called "stubs" is now wired through the
`pollis-core/src/bridge.rs` uniffi dispatcher (`invoke()` from `lib/native/`).
Current state:

- **Wired:** auth (email → OTP → PIN → initialize, real `invoke` calls + session
  restore via `get_session`); every tab/group/DM/chat/self screen consumes real
  React Query hooks (no hardcoded mock arrays); message send / receive / ingest /
  reactions / edit / delete; profile, devices, blocking, safety numbers,
  preferences. Safety numbers live in the DM, not on a profile (#1563): DM info
  → `dm/safety` ("You & <name>", one per other member in a group DM). Self → Security additionally carries: in-app **account deletion**
  (typed-DELETE full-screen confirm → `delete_account` + best-effort
  `wipe_local_data` → sign-out; App Store 5.1.1(v)); **auto-lock** (#899,
  `lib/autolock.tsx` — RN-layer deadline against the plain `lock` arm, since
  core's autolock Tauri event can't reach mobile; device-local window in
  expo-secure-store, Off/1/5/15/60 min, plus a manual "Lock now" row);
  the **security-event audit list** (`list_security_events`). There is no
  public-key line: the ML-DSA-44 account key is 2,624 hex characters, and the
  stub that once fed one (`get_identity`) always answered "none". `bridge.rs` covers ~every
  command the hooks call.
- **Chat parity set (2026-08, stacked PRs; bridge arms land in PR #967):**
  load-older paging (`useMessages` is an infinite query over `next_cursor`;
  inverted FlatList, `onEndReached`); reaction pills + full emoji picker
  (`get_reactions` batched per conversation in one queryFn; generated
  `components/emoji/emojiData.ts` — regenerate with
  `scripts/generate-emoji-data.py`, which writes frontend + mobile together);
  custom group emoji (`list_usable_emoji` / `list_group_emoji` /
  `upload_group_emoji` / `remove_group_emoji`; rendering via `get_emoji_path`
  → cached `file://`, see `lib/emojiCache.ts`; `CustomEmojiImage` never mounts a source-less `<Image>` (iOS fires `onError` for one, which showed every custom emoji as `:shortcode:` text on iOS, #1562) and retries a failure (`lib/emojiRetry.ts`); Maestro reads what it drew from `EmojiProbe` (`flows/custom-emoji.yaml`, `scripts/maestro-custom-emoji.sh`); management at
  `app/group/emoji.tsx` — every member sees Add, only admins see Remove; the DS enforces it, #1280); DM receipts (`get_conversation_receipts` +
  `mark_messages_read` via FlatList viewability 60%/600ms + AppState gate;
  synced top-level `send_read_receipts` key, default true); threads
  (`read_thread_messages` / `list_thread_summaries`, `send_message` with
  `threadId`, screen at `app/chat/thread.tsx`, replies filtered out of the
  main timeline); mentions (`lib/mentions.ts` port, roster-only candidates,
  composer autocomplete + MentionToken rendering); saved + permalinks
  (`toggle_saved_message` / `list_saved_messages` /
  `resolve_message_permalink`, `pollis://m/<conv>/<msg>` deep link at
  `app/m/[...permalink].tsx`, Saved screen at `app/self/saved.tsx`, verified
  clipboard copy via expo-clipboard); attachments (picker → `upload_media`
  path arg → desktop's exact `_att`/`_txt` envelope from `lib/attachments.ts`;
  inbound images render via `components/Media.tsx` + `get_media_path`; tapping any
  attachment opens the full-screen viewer `app/media.tsx`, #1248; the picker takes
  photos and videos, and a pending one shows in the composer as desktop's
  preview card — `components/chat/ComposerAttachmentPreview.tsx`, 120pt (desktop's
  80px + #1568's half), a video's first frame via `hooks/useVideoPoster.ts`, the
  shared top-end `AttachmentToolbar` — ONE capsule on the raised surface
  holding every glyph, never side-by-side discs; a toggle that is on shows it
  by an accent glyph alone — tap → the viewer on the picked file
  (`source=draft`), rules in `lib/composerAttachments.ts`, #1585); sensitive
  media (#1568): the toolbar's eye (before remove, images and videos only)
  marks a card, which then shows its blurhash under a crossed-out eye
  (`SensitiveCover`, blurhash from `Image.generateBlurhashAsync`) and opens
  nothing; it is sent as `sensitive` → the envelope's `sens: true`. Every
  surface (row tile, viewer page, Vault grid) covers a sensitive one until a
  tap reveals it — that tap only reveals, so a video never opens or plays on
  it — and a revealed one carries the one-action toolbar capsule to hide it
  again. Reveal state is `stores/sensitiveRevealStore.ts` (in memory, per
  attachment id); rules in `lib/sensitiveMedia.ts`; Maestro
  `flows/sensitive-media.yaml`; quote replies
  (#1270, desktop's Reply: `send_message` with `replyToId`, a `ReplyBar` over the
  composer, a quote line on the replying row via `lib/replyPreview.ts`, tap jumps
  to the original). A reply starts from a Discord-style **swipe toward the start
  edge** (`components/chat/SwipeToReply.tsx`: a Gesture Handler pan + Reanimated
  on the UI thread, JS called only when the reply fires; it claims only travel
  toward the start edge, so the back gestures and the list's scroll are never
  taken; numbers and curve in `lib/swipeReply.ts`), a **double tap** (a short
  spring nudge, then the same), the message sheet's Reply row, or the row's
  Reply accessibility action. Haptics: see "Haptics" under Design tokens. A finger
  resting on a row that opens the message sheet fades it one surface up
  (#1282: `bg` → `raised`, or `accentFaint` → `accentSoft` on a tinted row)
  after a 100 ms delay so a scroll never flashes rows, held through the
  350 ms long press, cleared on release, on the list taking the touch, or in
  the swipe's own `onStart` worklet; one shared value, timings and surfaces in
  `lib/pressFeedback.ts`. The DS base URL
  is threaded through `initializeNativeBridge` as `pollis_delivery_url`
  (`EXPO_PUBLIC_POLLIS_DELIVERY_URL`, dev → api-dev.pollis.com) — required, and
  since #987 the ONLY backend: OTP bootstrap, every remote write and every remote
  read go through the DS. Dev builds also pass `EXPO_PUBLIC_POLLIS_DEV_ACCESS_KEY`
  (#1242: api-dev serves only clients that send its gate key; Rust sends it only
  to a non-prod DS); a store, APK or OTA build refuses to configure while it is
  set, so clear it in `mobile/.env` first (`docs/deployments.md`, "Dev gate"). Full
  sign-in verified end-to-end on an Android emulator against the live dev DS.
- **Pinned messages (#1315, desktop's #99):** client-only, over the bridge's
  existing `pin_message` / `unpin_message` / `list_pinned_messages` arms
  (`hooks/queries/usePins.ts`, a port of desktop's). Same semantics, because
  they live in pollis-core and the DS: any member may pin or unpin, in channels
  and DMs, no count limit, both idempotent; a pin is a content snapshot sealed
  under the conversation's pin key, so a later edit/delete does not change it;
  a device still waiting for that key gets desktop's "not available yet" copy.
  Pin / Unpin is a row in the message sheet (main timeline only, as desktop),
  the list is `app/conversation/pins.tsx` behind the chat header's pin button
  (a pane page on iPad), and there is no per-row pin marker (desktop has none).
  Pins carry no realtime event, so the list refetches every time it opens
  (`usePinnedMessages(id, { fresh: true })`); the sheet's label reads the
  chat screen's 30 s-cached copy.
  A list row taps back to the conversation scrolled to the message: the same
  `resolve_message_permalink` as the permalink route, then
  `stores/messageJumpStore.ts` (desktop's store, same shape) — the chat screen
  pages back until the row is rendered (`lib/messageJump.ts`, ≤ 20 pages) and
  tints it. Saved and `pollis://m/…` links now jump the same way. Maestro:
  `flows/pins.yaml` and, across members, `scripts/maestro-pins.sh` (dev-DS
  peer `pollis-tui/examples/pins_peer.rs`).
- **Vault (#1316, desktop parity for #107):** `app/vault.tsx`, reached from a row
  at the top of the Direct tab (desktop puts it first in the DM list) and from the
  page quick-jumps of the search at the top of Self. Same core commands as desktop
  (`get_vault_messages` / `send_vault_message` / `edit_vault_message` /
  `set_vault_message_pinned` / `delete_vault_message` — `bridge.rs` already had the
  arms, so no native change), hooks in `hooks/queries/useVault.ts`, pure rules in
  `lib/vault.ts` (search over caption + filenames, the media roll, an edit that keeps
  an entry's files). Long press → entry sheet (pin/unpin, edit, delete); edit
  reuses the chat's `EditBar`, delete arms `VaultDeleteBar` in place of the
  composer (desktop's confirm, "on every device, cannot be undone"); the Media chip
  shows a grid (a virtualised `FlatList`) that opens `app/media.tsx` with
  `source=vault`. The viewer then swipes through the grid's images and videos in
  grid order, newest first (`vaultViewerItems`, #1492): a pager that mounts only
  the page on screen ± 1, so only those hold decrypted files; pinch/double-tap
  zoom stops the paging until back at 1×, a video pauses when swiped away, a
  downward drag dismisses, and "12 / 100" sits under the filename
  (`text-media-position`). Maestro: `.maestro/flows/vault-swipe.yaml`. Attachments come
  from the photo library, as in chat — desktop's arbitrary-file drop has no mobile
  picker yet (no `expo-document-picker`). A failed save names its reason
  (`vaultSaveFailure`), puts the draft back and offers "Try again"
  (`btn-vault-retry`); a retry uploads only the files that failed (#1497).
  Maestro: `.maestro/flows/vault.yaml`, `.maestro/flows/vault-retry.yaml`.
- **Keystore-at-rest (Android + iOS):** the file-backed keystore (the only
  backend on mobile) envelope-encrypts its contents with an AES-256-GCM master
  key held in the platform secure store — same on-disk blob (`iv(12)||gcm-ct`,
  `keystore.pks`, `dev-keystore.json` before #950) on both platforms. #882 gave desktop the same treatment
  under a machine-bound key, so mobile is no longer the only encrypted backend;
  the desktop file prefixes a `magic(4)||salt(16)` header ahead of the identical
  envelope, and mobile's format is unchanged.
  - **Android** — `android_kek` in `pollis-core/src/keystore.rs`: non-exportable
    key in the **AndroidKeyStore** (JNI-from-Rust: `JNI_OnLoad` captures the
    `JavaVM`; system `KeyStore`/`KeyGenerator`/`Cipher`, alias `pollis_keystore_kek`).
    Runtime-verified on an emulator (full sign-in, cold-relaunch unlock).
  - **iOS** — `ios_kek` in the same file: 32-byte master key as a Keychain
    generic-password item (service `pollis`, account `pollis_keystore_kek`,
    `AccessibleAfterFirstUnlockThisDeviceOnly` → excluded from all backups),
    AES-GCM done in Rust (`kek_envelope`, host-unit-tested). Works in the
    simulator (Secure-Enclave-resident keys deliberately not used — no simulator
    support; possible later hardening). **On-device/simulator verification
    pending** (needs the Tahoe/Xcode-26 machine — see iOS dev loop above).
  Identity keypair + session survive a cold process kill and decrypt behind the
  device PIN. (`accounts.json` beside it is a public-metadata index only — no
  secrets.) Both `#[cfg]` branches are CI-gated by `mobile-core-check.yml`
  (android + ios cross-compile jobs).
- **Media:** `get_media_path` decrypts an R2 object to a sandbox `file://` for
  `expo-image` — mobile can't run desktop's loopback media server. See `lib/media/`.
  Every path that ends a session (sign-out, account deletion, revoked-device
  sign-out) goes through `endSession()` (`lib/session/`): it resets the store,
  pops every signed-in screen and lands on sign-in, then clears the React Query
  cache, decrypted media (`pollis-media/`), export archives (`pollis-export/`)
  and the emoji cache. Never call `appStore.logout()` or route to sign-in by
  hand. The root layout runs `sweepStalePlaintext()` once per process before
  any session restores (not `app/index.tsx`, which is re-entered mid-session).
  `tests/session-teardown.test.ts` enforces all of it.
- **Foreground realtime (scaffold):** mobile joins the same SFU rooms as desktop
  via the JS LiveKit SDK in **data-only** mode (`lib/realtime/`;
  `useConversationRealtime` for the open chat, `useInboxRealtime` for the
  groups/DM lists). It ingests + invalidates on `new_message` / `dm_created` /
  `membership_changed`. Graceful no-op until `get_livekit_token` exists on the
  bridge. A missed wake-up is caught up, not waited out: returning to the
  foreground re-ingests every listed conversation (so does landing in the app —
  cold start, sign-in, unlock — once both lists load, #1422), and a LiveKit `Reconnected`
  re-ingests that room (`lib/messageWake.ts`) — a message sent from the user's
  other device has no push to fall back on. Such a message is shown live but
  never counted unread (decided after ingest from the newest row's sender).
  Every one of those catch-ups refetches the open conversation, so an
  optimistic send must survive a refetch: the newest page carries any
  `pending-…` stub over, and the send's success swaps it — or restores the
  confirmed row if a refetch lost it — then refetches once the row is stored
  (`hooks/queries/pendingMessages.ts`; the same for thread replies). Without
  this a just-sent message blinked out to "No messages yet".
- **Durable outbox (#1335):** the core stores every outgoing message
  (`send_state: "pending"`) BEFORE any network work, under an id the send asks
  for first (`new_message_id`) and tags its stub with (`outboxId`), so a
  refetch that reads the stored row drops the stub instead of doubling it. A
  send the OS killed mid-flight is still there after relaunch and is delivered
  by the drain: core spawns it after `unlock`, and `useInboxRealtime` calls
  `flush_outbox` on sign-in, on resume and on a LiveKit `Reconnected` — never
  on a timer. A failed send is a stored `failed` row: `MessageRow` shows
  Retry / Discard (`components/chat/UnsentActions.tsx`, `btn-retry-message-<id>`
  / `btn-discard-message-<id>`; `useRetryMessage` / `useDiscardUnsent`); a
  `rejected` row (no retry can fix it, so the drain stops trying) looks the
  same. Attachments are not uploaded by the hook any more: `send_message` gets
  the caption plus `attachments` (`lib/attachments.ts` →
  `outgoingAttachments`, picker paths), the core copies the files into its
  encrypted local DB with the row, and the outbox uploads them — so a kill
  mid-upload loses nothing; until then the row's attachments have no key and
  `components/Media.tsx` draws them pending. Discard of a row that was ever
  sealed first withdraws it from everyone. Pure
  rules + tests: `pendingMessages.ts`, `tests/pending-messages.test.ts`;
  device tier: `.maestro/flows/outbox.yaml`. A core change, so phones get it
  with the next store build. **Edits too (#1555):** `edit_message` stores the
  edit in the core's edit outbox before posting and the row reads back with
  `edit_state` until the DS confirms; `MessageRow` labels a failed one
  "(edit not sent)" (`editUnsent`), and the same drain delivers it.
- **Android notification small icon (#1606):** `assets/notification-icon.png`,
  the expo-notifications plugin's `icon` (tinted with its `color` in the shade).
  Prebuild writes it as `@drawable/notification_icon` and points both the FCM
  and the expo local-notification `default_notification_icon` meta-data at it;
  anything posting its own notifications natively should read that meta-data.
  The lock glyph alone, white on transparency, 96px (xxxhdpi 24dp) with its tall
  side 22dp — without it Android fell back to the launcher icon, safe-zone padding
  and all, and the status-bar lock was tiny. Generated from the SVG by
  `scripts/gen-icons.sh`; `tests/android-notification-icon.test.ts` pins it.
- **Push (client wired):** `lib/push/` + `usePushNotifications` — contextual
  permission (asked on first conversation open, not at login), token registration
  (`register_push_token`, best-effort), content-free tap/data handlers, and a
  Notifications row in Preferences. Since #1423 (`lib/push/registration.ts`,
  `hooks/usePushRegistration.ts`, `tests/push-registration.test.ts`) a user counts
  as registered only after the DS accepted the token, and the app also registers
  without prompting on sign-in/unlock and every return to the foreground, and
  re-registers on `Notifications.addPushTokenListener` (token rotation). None of
  that runs before the device is past the PIN (#1620, `createRegistrationTriggers`):
  registering is DS-signed, and on a cold start `appStore.isLocked` reads false
  until the PIN screen mounts, so the hook waits for `isInSignedInApp`, not the
  lock flag alone. A rotation on the PIN screen is kept for the unlock, and a
  failed attempt is retried only by the next trigger, never by itself. Push covers backgrounded/closed delivery;
  foreground delivery is the realtime path above. A tap is held
  (`lib/push/tapGate.ts`) until the device is past the PIN, and the tap that
  cold-started the app is picked up with `getLastNotificationResponse` (#1422):
  before, a cold-start tap was dropped and a tap on a locked app pushed the
  chat over the PIN screen.
- **Read notifications leave the tray (#1508).** Once a conversation is read on
  any client its notifications clear from every phone, matched by handle, never
  by conversation id (`lib/push/dismiss.ts`): (1) `markConversationRead` clears
  this device's own for that conversation at once, by every route
  (`dismissConversationNotifications`, resolving each tray handle); (2) core
  reports a moved cursor to `POST /v1/push/dismiss` and the DS sends a silent,
  data-only `{ d: [handles] }` push that the expo-notifications background task
  `lib/push/dismissTask.ts` clears — foreground, background, and on Android with
  the app killed; (3) on landing and every resume `reconcileReadNotifications`
  asks `dismissed_push_handles` which tray handles were read and clears them
  (iOS force-quit, Doze). Never from local unread counts — an un-ingested
  message would look read. The task is defined in **`mobile/index.ts`, the app
  entry (`package.json` `main`)**, which imports it before `expo-router/entry`:
  expo-task-manager may load the bundle headless just to run it, so it must
  not live in a component, and it imports no bridge or store. Android finds a
  system-posted notification by its `tag` (the DS sets it to the handle) via
  expo's `expo-notifications://foreign_notifications?tag=…` identifier; iOS
  keeps `data.h`. Native pieces (store build only): `expo-task-manager`, and
  `enableBackgroundRemoteNotifications` on the expo-notifications plugin
  (iOS `UIBackgroundModes: remote-notification`; expo-task-manager adds
  `fetch`), and an R8 keep rule for `expo.modules.adapters.react.apploader.**`
  (`extraProguardRules`): expo-modules-core names `RNHeadlessAppLoader` only in
  manifest meta-data, so minified release builds stripped it and the first
  data-only push to a killed app crashed in `TaskService.executeTask`.
  Maestro: `scripts/maestro-push-dismiss.sh` (Android, real pushes, tray read
  with `dumpsys notification`). `tests/push-dismiss.test.ts`;
  full design in `.codesight/wiki/notifications.md`.
- **Android rich push (#1599).** The app's only FCM service is
  `com.pollisnative.push.PollisMessagingService`, a subclass of expo's
  (`plugins/withRichPush.js` removes expo's declaration); it handles data-only
  `{h}` pushes itself (headless `pollis_notify_fetch` over the JNI shim in
  `pollis-native/android/cpp-adapter.cpp`, `MessagingStyle`, tag = `h`) and
  hands every other push to expo via `super`. Every notification it posts
  taps through `PushTapActivity`, which gives the tap to expo-notifications
  directly (#1614: a launcher intent is lost when Android re-creates the
  activity from Recents). `cpp-adapter.cpp` and the module's
  `build.gradle` are hand-extended and listed in `ubrn.config.yaml`
  `noOverwrite` — never let ubrn regenerate them. Notification copy is Android
  string resources generated by `node scripts/mobile-android-push-strings.mjs`.
  Debug builds have an adb test hook (`RichPushTestReceiver`); see
  `.codesight/wiki/notifications.md` → "Android rich push".
- **Unread badges are persisted (#1422, desktop's #844):**
  `hooks/queries/useUnread.ts` reads `get_unread_counts` into
  `appStore.unreadCounts` (whole-set replace, `lib/unread.ts`) on landing in
  the app, on resume and after every ingest, so a message that arrived while
  the app was backgrounded or closed badges the tab, the DM row, the group dot
  and the channel count. `markConversationRead` (clears the badge and runs
  `mark_conversation_read`) replaces every bare `appStore.markRead`; the chat
  screen marks itself read on focus, on blur and as messages arrive, and
  records itself in `appStore.openConversationId`, which never badges.
  `refresh_read_cursors` (pull, merge, push only if ahead; #1628) runs on
  unlock, resume, `realtime_reconnected` and the DS's `read_cursors_changed`
  nudge, so reading on one device clears the others' badges and, through
  `dismissConversationNotifications`, their tray. A live wake never bumps a
  badge: it ingests and the counts are re-read from Rust (#1628).
  Maestro: `scripts/maestro-unread.sh` (dev-DS peer `unread_peer`).
  The tab badges also count what is waiting on the user (#1561,
  `lib/tabBadges.ts`): Direct adds pending message requests
  (`useDMRequests`), Groups pending group invites (`usePendingGroupInvites`),
  Slack/Discord style, and the spoken label names each part. Both queries are
  refreshed by the inbox realtime (`dm_created`, `membership_changed`); accept
  and block drop the request from the cache on the tap. Maestro:
  `scripts/maestro-pending-badges.sh` (dev-DS peer `pending_badge_peer`).
- **Voice — libraries installed, NOT activated (#343).** Mobile will take the
  **JS LiveKit SDK** path (`@livekit/react-native` + `@livekit/react-native-webrtc`)
  rather than desktop's Rust media pipeline — see the architectural note in epic
  #342. The npm packages are installed, but nothing is wired yet: **no** Expo
  config plugins, **no** `registerGlobals()`, and deliberately **no microphone /
  camera-for-voice permissions** (we do not want to request mic/video access from
  users — not now, not speculatively). The `CAMERA` permission that exists is for
  QR pairing only. Never set a plugin's `cameraPermission` to `false` to keep it
  that way: expo-image-picker turns `false` into a blocked CAMERA on Android
  (`tools:node="remove"`, which also strips expo-camera's) and a deleted
  `NSCameraUsageDescription` on iOS — that shipped in 1.0.0/1.0.1 (#1255);
  `tests/camera-permission.test.ts` pins it. When voice is actually built, add the LiveKit/webrtc Expo
  config plugins, `registerGlobals()`, the permission declarations, and the call
  UI together — all in one go, under #343.

### Still pending (need the native build env to compile/verify)

- ~~**`get_livekit_token` bridge command**~~ — **DONE.** The `get_livekit_token`
  arm in `bridge.rs` now calls `ds_livekit_token` (`POST /v1/livekit/token`); the
  DS derives the `{user_id}:{device_id}` identity from this device's verified
  signature (matching desktop's `connect_rooms`) and returns the JWT. No on-device
  signer — the LiveKit API secret was removed from the bundle (#393). Activates
  foreground realtime once `EXPO_PUBLIC_LIVEKIT_URL` is set (#185). On-device
  verification still pending.
- **Push backend (code DONE; per-platform credentials pending).**
  - `push_token` Turso table — migration `000006_push_token.sql` (additive
    `CREATE TABLE`/`CREATE INDEX`; ships to prod via the release pipeline's
    `db-apply.sh`).
  - `register_push_token` — `bridge.rs` arm → `commands::push::register_push_token`
    (upsert keyed on the token, so a re-register reassigns ownership).
  - Content-free fan-out — **`pollis_delivery::push::notify_new_message`, in the
    DS, not the client** (#987 moved it off every client so nobody needs a
    credential that can read other people's `push_token` rows). Driven by the
    `push_to` field on `POST /v1/messages/send`. Since #1431 every group-channel
    message pushes every other member, minus whoever muted that channel (a
    per-account `channel_mute` row on the DS, toggled from the channel's
    "More" sheet, `toggle-channel-mute`) unless the message names them by
    `@username` (`push_mentions`). Because the DS filters, a muted channel is
    silent in the foreground too. Payload is `{ h }` — an opaque
    per-notification handle, resolved via `POST /v1/push/resolve` (#1122/#1157)
    — and a generic "New message" body. Never plaintext, sender, or the
    conversation id.
  - The DS authenticates the send with `EXPO_TOKEN` (#707), wired through the
    full Doppler → sync script → wrangler → `worker/index.ts` → container chain
    that `scripts/check-ds-config-chain.py` enforces. Optional until "Enhanced
    Security for Push Notifications" is enabled on the Expo account; required
    the moment it is, so set it **before** flipping enforcement.
  - **EAS project — DONE (#707):** `@pollis/mobile`, `projectId` in `app.json`.
    Registration therefore works now; before this it could not, on any device.
  - **Platform credentials — DONE (#707), and EAS holds them, not this repo.**
    iOS: an APNs `.p8` that `eas credentials -p ios` generated on the Apple
    Developer Portal (key `DZB37YSPU7`) and kept; the material was never
    downloaded, so re-running that command is the only way to see or replace it.
    Android: an FCM v1 service-account key for Firebase project `pollis-38b6f`,
    uploaded the same way. That JSON is a live credential and stays out of the
    repo — but `google-services.json` **is** committed here (public identifiers
    only) and wired as `expo.android.googleServicesFile`, because without it
    `getExpoPushTokenAsync` throws on Android: the Expo token wraps an FCM
    device token it could not otherwise obtain. If its `package_name` ever
    drifts from `expo.android.package`, the Google Services Gradle plugin fails
    the `android-build` job rather than shipping silent-dead push.
  - **The one thing left:** delivery to a *locked physical device*, which needs
    a store-signed build (PL-24).
- **webrtc Expo config plugin** + an AndroidManifest mic/camera **removal** rule
  so the data-only realtime path adds no voice/video permission.
- **QR sign-in, both directions (#1207, #1361).** Self → Security → "Scan to
  sign in a device" (`app/self/link-device.tsx`) is a **scanner**. It reads the
  v2 code a new desktop or phone shows (`pollis-link:v2:`, the new device's
  ephemeral key, no secret), takes the PIN, then `accept_device_offer` +
  `finish_device_offer`; Rust wraps only to the scanned key. It used to SHOW a
  v1 code, which only ever reached a desktop by being retyped; desktop still
  shows v1 for a new phone. On sign-in, `app/(auth)/link.tsx` scans a desktop's
  v1 code (or takes it as text), or with "Show a code instead" shows this
  phone's own v2 code (`components/auth/OfferCodeView`). `lib/linkCodes.ts`
  tells the two apart. Maestro: `scripts/maestro-qr-link.sh` (phone ↔ phone)
  and `scripts/maestro-qr-scan.sh` (phone ↔ desktop peer). Design:
  `docs/qr-device-link-design.md`.
- **A new device's approval request comes to the user (#1360).** Desktop's
  "Approve from another device" files a request; the DS nudges
  `inbox-<user>` with `enrollment_requested` (honoured only with no publishing
  participant, as desktop does). `hooks/useEnrollmentApprovals` opens
  `app/approve-device.tsx` (type the code off the new device, Approve / Reject)
  from that nudge, from a `list_pending_enrollment_requests` check on landing in
  the signed-in app (sign-in, unlock, cold start) and from one on every return
  to the foreground — once per request per process; backing out leaves it on
  Security. QR-link requests (`link_id`) are skipped. Rules:
  `lib/enrollmentPrompt.ts`. The inbox subscription itself starts only once
  the device is in the signed-in app: `currentUser` is set at email
  verification, before the device can sign a room-token request, and a room
  whose connect failed was never retried — so the inbox was dead for the whole
  session after every fresh sign-in. `useInboxRealtime` also re-opens any room
  whose connect failed when the app returns to the foreground. Maestro:
  `scripts/maestro-approval-prompt.sh`.
- ~~**`device_revoked`** self-sign-out on the inbox connection~~ — **DONE.**
  `useInboxRealtime` treats the event payload as advisory, confirms with
  `is_current_device_registered`, then hands off to `deviceWasRevoked()`
  (`lib/deviceRevoked.ts`). A device that missed the nudge (closed, asleep,
  offline) finds out on its next command instead: the DS answers its signature
  with `device_revoked`, the core fails every command with it, and the native
  bridge passes each failure to `noticeDeviceRevoked`, so the root layout's one
  handler confirms with `is_current_device_revoked` (a late or stray rejection
  never ends a healthy session), runs `logout` with `deleteData: false` (the
  core wipes the revoked device itself, and only that one) and `endSession()`,
  and sign-in shows `text-session-notice` (#1338; Maestro
  `scripts/maestro-device-revoked.sh`).

## Secrets architecture — settled (#393, #987, #995)

This was for a long time the mobile app's blocking release risk, and it is now
closed. Kept because the reasoning is what stops it coming back.

The hazard: **Expo inlines an `EXPO_PUBLIC_*` value into the JS bundle wherever
code references it**, so anything referenced ships inside the APK/IPA in
plaintext and `unzip` + grep recovers it. Note the precise rule — *referenced*,
not merely *present in `.env`*. A var nothing reads is inert; a read that
outlives its secret is the actual leak. The client now holds **no secret of any
kind**, and every credential below moved server-side rather than being scoped
down:

- ✅ **Resend key** — server half moved in #393 (OTP runs on the DS); the client
  half was missed until **#995**, which deleted `resendApiKey` from `InitConfig`,
  the init-JSON mapping and `app/_layout.tsx`. Until then it was the one secret
  still being inlined, because it was the one still being read — `pollis-core`'s
  `InitConfig` has no `resend_api_key` field, so serde parsed and discarded it.
  This section claimed "✅ DONE" for the whole of that window, which is the real
  lesson: mark a bullet done against the code, not against the intent.
- ✅ **R2 keys** — DONE (#393). `r2_access_key_id` / `r2_secret_access_key` are
  gone from the bundle; `commands/r2.rs` presigns every get/put/delete via the DS
  (`POST /v1/r2/presign`) and only the non-secret `EXPO_PUBLIC_R2_ENDPOINT` /
  `_R2_PUBLIC_URL` remain. Never re-add the R2 secret env vars here.
- ✅ **LiveKit API secret** — DONE (#393). `livekit_api_key` / `livekit_api_secret`
  are deleted from the client; tokens come from `POST /v1/livekit/token` and
  server-side fan-out/roster from `/v1/livekit/send-data` + `/v1/livekit/participants`.
  Only the non-secret `EXPO_PUBLIC_LIVEKIT_URL` remains. Never re-add the API
  key/secret env vars here.
- ✅ **Turso token** — DONE (#987), and by removal rather than by scoping. The
  client holds **no database credential of any kind**: `EXPO_PUBLIC_TURSO_URL` /
  `EXPO_PUBLIC_TURSO_TOKEN` are gone from `InitConfig`, `commands/turso_token.rs`
  and `RemoteDb` are deleted, `POST /v1/turso/token` is deleted, and `pollis-core`
  does not depend on `libsql` at all. Every remote read is a signed
  `POST /v1/read/…` to the DS, on the same transport as the writes. The
  "true per-user row scoping needs per-user DBs (#261)" line this bullet used to
  end on is moot — there is no client token left to scope.

The fix was an **authorized-secrets broker** — a server-side endpoint (the DS)
that holds the real secrets and never ships them to the client:

1. **Bootstrap (pre-auth):** ✅ DONE — `POST /otp/request` + `/otp/verify` run
   server-side (the DS calls Resend). The phone never holds the Resend key; the
   last client-side remnant of one went in #995.
2. **Post-auth, scoped + short-lived creds:**
   - **Turso:** ✅ DONE — and then deleted (#987). The client reads through the
     DS and holds no token; the mint endpoint is gone with it.
   - **LiveKit:** ✅ DONE — the DS signs the JWT (`/v1/livekit/token`) and does
     server-side SendData/ListParticipants; the client holds no API secret.
   - **R2:** ✅ DONE — the DS returns presigned URLs; the client holds no R2 keys.

This settled the tension with the old "no backend server — Rust talks to Turso
directly (1 hop)" principle. That model was tenable for a signed desktop binary
(extraction is harder, though not impossible) and **fundamentally incompatible**
with keeping a DB token secret inside a client JS bundle — which is what made
mobile the forcing function. #393 introduced the broker for the bootstrap and
credential-minting paths; #987 finished the argument by moving the READS behind
the DS too and deleting the database credential entirely. The E2E model is
untouched throughout: the server still never sees message plaintext or a private
key.

## Design tokens

Defined in `theme/tokens.ts`; the colour math lives in `theme/derive.ts` (pure,
no react-native) and the accent presets in `theme/accents.ts`.

- **Exactly two base colours: accent and background.** Every other colour is
  derived by `deriveTheme(accent, bg)` — the background mixed toward white for
  the neutrals, the accent mixed into them for tint, the accent at an alpha for
  hairlines. No other hex may appear in UI code. Defaults: accent `#fabf5a`
  (brand amber), background `#0a0907`. Both are runtime-configurable
  (`setAccentRgb/Hex`, `setBackgroundRgb/Hex`; `<ThemeProvider>` holds them and
  `useTheme()` re-renders subscribers). `palette.*`, `semantic.*` and the
  `type.*` colours are getters over the live derived theme.
- **The saved accent paints the first frame (#1447).** The synced
  `accent_hex` preference lands only after sign-in and a DS round trip, so
  the device also keeps the last accent it showed in `expo-secure-store`
  (`theme/accentStore.ts`, key `pollis.theme.accent`), and `theme/tokens.ts`
  reads it with the SYNCHRONOUS `SecureStore.getItem` while the module loads —
  before any render, PIN screen included. `ThemeProvider` starts from the live
  tokens, refetches the synced blob the moment the app is unlocked, applies a
  saved `accent_hex` and writes every change back. `get_preferences` answers
  `{}` before the unlock (remote read fails, local cache locked), so a blob
  with no accent means the default only when read after the unlock; sign-out
  also resets to the default. Never read a token at module scope
  (`StyleSheet.create` / a top-level `const`): it is frozen at import.
- **Every themed surface must re-render on a change.** `useTheme()` re-renders
  only its caller: a subscribing `<Screen>` does NOT re-render the `children`
  its route created, and `observer()` is a memo that skips parent re-renders.
  So every route component, and every `observer`/`memo` component that reads
  tokens, calls `useThemeRerender()` (or `useTheme()`), and a `FlatList` whose
  `renderItem` is a `useCallback` passes `extraData={accentHex}`.
  `tests/theme-startup.test.ts` fails on a miss. `<Screen>` draws the
  invisible e2e hooks `theme-accent-<rrggbb>` / `theme-boot-accent-<rrggbb>`
  (`components/ThemeProbe.tsx`; Maestro `theme-startup.yaml`).
- `semantic`: `text` / `dim` / `muted` (text ramp), `bg` < `panel` < `raised`
  < `high` (surfaces), `hair` / `hairSoft` (translucent separators), `edge`
  (opaque control border, ≥3:1 vs raised), `accent`, `accentFaint/Soft/Mid/Line`
  (opaque accent tints), `onAccent` (= bg, text on accent fills), `backdrop`,
  `sheetBg` (= raised, opaque — #1193). `danger` = accent: there is no third
  hue; destructive actions are told apart by label, icon and separation.
  Old names (`ink`, `ink2`, `mute`, `mute2`, `hairStrong`, `fieldBg`, `cardBg`)
  are deprecated aliases.
- `tests/theme-contrast.test.ts` asserts WCAG AA for every preset: text tiers
  and accent ≥4.5:1 on every surface, edge ≥3:1 vs raised, onAccent ≥4.5:1.
  Tune `RECIPE` in `derive.ts` if a new preset fails — never special-case it.
- `type.*`: Display 28/700, Title 20/700, Heading 17/700, Body 16/400, Secondary
  14/400, Section 13/600, Meta 12/400, Tab 12. Sentence case, no tracked
  capitals, nothing below 12; all text follows system font scaling.
- `r` radii 4/10/12/14/16/20(sheet); `layout.touchMin` 44.
- Font: Geist via `fonts.regular/medium/semibold/bold` (one family per
  weight, never with `fontWeight`); spread a `type.*` step rather than
  hand-setting size + family.

**Haptics (#1286, `expo-haptics`, 1.0.4+).** Every significant action that
is not plain navigation gives tactile feedback; the event table is
`lib/hapticEvents.ts` and `lib/haptics.ts` plays it (iOS impact / selection /
notification generators; Android `performAndroidHapticsAsync`, i.e.
View.performHapticFeedback, which follows the phone's touch-feedback setting —
never the raw vibrator). Fire-and-forget, never throws into the interaction.

- **Mutations — one chokepoint.** Tag the hook's `useMutation` with
  `meta: { haptic: { success | mutate, error? } }`; the `MutationCache` in
  `lib/queryClient.ts` plays it. `success` = constructive (verified, reported,
  approved, saved, created, unlocked), `destructive` = a destructive action
  landed (block, delete, remove, revoke, leave, reject), `toggle` = a mark
  flipped (saved, unblock, role, verified off), `mutate` for optimistic
  actions (send, react). Any tagged mutation that fails plays `error` (a
  wrong PIN or code, a failed send). `tests/haptics.test.ts` fails on a new
  mutation hook with no tag unless it joins the short `NO_HAPTIC` list.
- **Gestures and controls:** swipe to reply ticks crossing the threshold
  either way (the worklet hops to JS only on that frame), the double tap,
  every long press (MessageRow and `ListRow`), every `Toggle` / `ToggleRow`,
  the first tap of every two-tap confirm (`confirmArm`), every PIN keypad key
  (`keyTap`, digits and delete), and clipboard copies (success, or error if
  the copy failed).
- Never add one to plain navigation (opening a screen, back, tabs).

**Press-in (#1529).** Where an accidental press is free to undo — back,
closing a sheet, switching a tab or view, opening a menu, a sheet, a page or
the picker — the control acts on **touch-down**: an opt-in `pressIn` prop on
`IconButton`, `Chip`, `ListRow` and `Button`, `PressInPressable` for a bare
Pressable, and `usePressIn` underneath (rules pure in `lib/pressIn.ts`,
pinned by `tests/press-in.test.ts`). `onPress` stays wired as the
screen-reader path (TalkBack's double tap is a click with no touch-down;
VoiceOver's is a synthesised touch), and a guard swallows the release of a
touch that already acted, so the action runs once either way. On by default
in `Header` (back, and the title when it is a button), `TabBar` and
`SheetOverlay` (Close and the backdrop); opted in on the chat header's pins /
info / more, the composer's attach and emoji buttons (never send), the reply
quote, `GroupPanel`'s header (menu, invite, search), Direct's new message,
the Vault view chips, the media viewer's prev / next / open-settings, the
menu rows of `ChannelMenuSheet` / `GroupMenuSheet` (not Leave) /
`AddGroupSheet`, and `AudioPage`'s play / pause / skip (not the seek bar).
**On Android nothing inside a `SheetOverlay` acts on touch-down** (Close,
the backdrop and the sheet's rows fall back to release through
`InSheetContext` / `pressInSupported`): dismissing the Modal while its
touch is still down left that touch unfinished in JS, and the next tap on
the screen underneath was swallowed (the tour's first Back after
`btn-menu-info` did nothing). iOS ends the touch, so sheets keep touch-down
there.
Rules: **never on a row inside a `ScrollView` / `FlatList` / `Body`** —
touch-down lands before the scroll takes the touch, so the row would open on
every scroll that starts on it (those rows keep `onPress`; a
`unstable_pressDelay` variant is a possible follow-up, untested); never with
`onLongPress` (the `ListRow` types refuse the pair and it falls back to
release at runtime); never on anything that sends, persists, notifies
others, confirms or toggles; no haptic (it is navigation).

UI rules: two base colours — use `semantic.*` / `type.*`, never a hex or
rgba in `app/` or `components/` (QrCode's black/white is the one deliberate
exception, for scannability); icons/text on an accent fill use
`semantic.onAccent`. 44pt targets (hitSlop/padding for small visuals); one
spoken label per row (`ListRow` is one element; `ActionRow` when the row
holds its own button; a message row exposes "View profile" as an
accessibility action); headings `accessibilityRole="header"`; state never by
colour alone; system font scaling stays on (`minHeight`, not `height`, on
text containers); start/end edges, never left/right; never a coloured
stripe on one edge of a rounded box (emphasis is a full tint, a full border,
weight, a dot or a count); sentence case; no "end-to-end encrypted" copy.
Titles and back live at the TOP (`<Header>`). Bottom sheets are
`SheetOverlay` (Modal, full-screen backdrop, title + 44×44 Close —
`btn-sheet-close`, or the sheet's `closeTestID` — no drag handle, no extra
Cancel button): rows in `Group surface="high"`.
The entrance (#1249) runs on Reanimated on the UI thread: one shared value
drives backdrop opacity and the card's translateY, starting from the window
height on the Modal's `onShow` (no layout measurement), with a 600 ms
fallback that settles to static styles — `tests/sheet-entrance.test.ts` pins
that net. `@gorhom/bottom-sheet` was evaluated and rejected: on Reanimated
4.4+ its sheets can mount invisible (gorhom #2721, #2696) and its modal can
wedge after an interrupted dismiss (#2762); upstream had no fix as of 2026-10.
