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
- Geist (UI, the desktop refined skin's face) via `@expo-google-fonts/geist`, one family name per weight (`fonts.regular/medium/semibold/bold`; never pair with `fontWeight`). Monospace (crypto keys, e.g. the Security public-key line) uses the **system** mono face — `fonts.mono*` = `Platform.select({ ios: 'Menlo', android: 'monospace' })`, no bundled font.
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
instead of behind a 3-ABI cross-compile.

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
xcrun simctl launch <udid> com.pollis.mobile
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
  exactly that. Do not switch it without understanding this.
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
`expo prebuild --platform android`, then `gradlew :app:assembleRelease` — on a
stock `ubuntu-latest` runner, and uploads the APK as `pollis-android-apk`. It
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

Published as a private individual account, Apple team `9JF7WWYMU2`.

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

**Environment first.** `EXPO_PUBLIC_*` vars inline into the shipped JS bundle,
so before any store build check `mobile/.env`:

- `EXPO_PUBLIC_POLLIS_DELIVERY_URL` **must** be `https://api.pollis.com` (prod
  DS), not api-dev.
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
pnpm expo prebuild -p ios --no-install
cd ios && pod install && cd ..

# 3. Archive UNSIGNED, then sign on export.
xcodebuild -workspace ios/Pollis.xcworkspace -scheme Pollis \
  -configuration Release -destination 'generic/platform=iOS' \
  archive -archivePath build/Pollis.xcarchive \
  DEVELOPMENT_TEAM=9JF7WWYMU2 CODE_SIGNING_ALLOWED=NO
# 4. Give the unsigned app its entitlements, or push is silently lost (below).
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
`-allowProvisioningUpdates` cloud-mints from the Xcode Apple ID session (no EAS
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
there) sets `method: app-store-connect`, `teamID: 9JF7WWYMU2`,
`uploadSymbols: true`. Upload the resulting `.ipa` with Xcode Organizer or
`xcrun altool`/Transporter.

Check the exported `.ipa` before an upload — all held on the 2026-10-02 build
(1.0.0 / 1000000, the first ever uploaded):

```bash
unzip -q build/export/Pollis.ipa -d /tmp/ipa
codesign -dvvv /tmp/ipa/Payload/Pollis.app          # Authority=Apple Distribution
codesign -d --entitlements :- /tmp/ipa/Payload/Pollis.app   # get-task-allow=false, aps-environment=production
/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' /tmp/ipa/Payload/Pollis.app/Info.plist
strings /tmp/ipa/Payload/Pollis.app/main.jsbundle | grep -o 'https://api[a-z.-]*pollis.com'
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

Verify, then upload with `scripts/play-publish.py` (Play Developer API through
the `play-publisher@pollis.iam.gserviceaccount.com` service account; key in
Doppler `PLAY_SERVICE_ACCOUNT_JSON`, copy in 1Password):

```bash
jarsigner -verify -certs android/app/build/outputs/bundle/release/app-release.aab  # CN=Daniel Kral, OU=Pollis
unzip -p android/app/build/outputs/bundle/release/app-release.aab base/assets/index.android.bundle \
  | strings | grep -o 'https://api[a-z.-]*pollis.com'                              # prod DS only
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
  and not `api-dev.pollis.com` (Hermes bytecode — `strings` can miss it). After
  upload it re-downloads both CDN URLs and `latest.json` and compares hashes.
- **No auto-update.** Nothing in the app checks for a newer APK; users come
  back to the page. Push still goes through Expo → FCM, so it needs Play
  services on the phone; the page says that too.

Local build, same steps as the workflow (no `mobile/.env` needed — the
`EXPO_PUBLIC_*` values come from the environment, which wins over `.env`):

```bash
cd mobile
source android-env.sh
pnpm install --ignore-workspace --frozen-lockfile
(cd modules/pollis-native && uniffi-bindgen-react-native build android \
  --config ubrn.config.yaml --and-generate --release)
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
  (tabs)/              groups · direct · search · self (custom <TabBar>)
  group/[id].tsx       one group's channel panel (phones: pushed, no tab bar; iPad: redirects to the Groups tab)
  group/*              new, invite, invite-links, members, settings, emoji, requests, discover
  chat/[id].tsx        conversation (Header + list + composer); chat/thread.tsx
  dm/{new,info,requests}.tsx · conversation/info.tsx · user/[id].tsx · report.tsx
  media.tsx            full-screen attachment viewer (#1248): zoom/swipe, video, audio, save/share
  self/*               preferences, user-settings, security, blocked, saved, …
components/
  ui.tsx               primitives: Txt, Screen, Header, IconButton, SectionTitle,
                       ListRow, ActionRow, Group, Card, Field, Avatar, Chip/Pill,
                       Badge, Button, Toggle, Dot, Divider, BottomAction, Body
  chat/SheetOverlay.tsx the one bottom sheet (every *Sheet is built on it)
  icons.tsx            lucide wrapper (Icon.*, stroke 1.75)
  TabBar.tsx           custom bottom tab bar (accent active, unread badges)
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

iPad (regular width, `hooks/useLayoutClass`): Groups and Direct are a
two-pane (`components/MasterDetail.tsx`) — list left, conversation right, tab
bar visible. Conversation pages (info, members, group settings/invite/emoji/
requests, thread, profile — `PANE_PATHS` in `components/pane/paneContext.ts`)
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
`KeyboardAvoidingView` (`behavior="padding"`, `automaticOffset`) on both
platforms, and a `<Screen>` nested in another skips its own. Never import
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
  preferences. Self → Security additionally carries: in-app **account deletion**
  (typed-DELETE full-screen confirm → `delete_account` + best-effort
  `wipe_local_data` → sign-out; App Store 5.1.1(v)); **auto-lock** (#899,
  `lib/autolock.tsx` — RN-layer deadline against the plain `lock` arm, since
  core's autolock Tauri event can't reach mobile; device-local window in
  expo-secure-store, Off/1/5/15/60 min, plus a manual "Lock now" row);
  the **security-event audit list** (`list_security_events`) and the self
  public-key line (`get_identity`, system mono). `bridge.rs` covers ~every
  command the hooks call.
- **Chat parity set (2026-08, stacked PRs; bridge arms land in PR #967):**
  load-older paging (`useMessages` is an infinite query over `next_cursor`;
  inverted FlatList, `onEndReached`); reaction pills + full emoji picker
  (`get_reactions` batched per conversation in one queryFn; generated
  `components/emoji/emojiData.ts` — regenerate with
  `scripts/generate-emoji-data.py`, which writes frontend + mobile together);
  custom group emoji (`list_usable_emoji` / `list_group_emoji` /
  `upload_group_emoji` / `remove_group_emoji`; rendering via `get_emoji_path`
  → cached `file://`, see `lib/emojiCache.ts`; management at
  `app/group/emoji.tsx`); DM receipts (`get_conversation_receipts` +
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
  attachment opens the full-screen viewer `app/media.tsx`, #1248). The DS base URL
  is threaded through `initializeNativeBridge` as `pollis_delivery_url`
  (`EXPO_PUBLIC_POLLIS_DELIVERY_URL`, dev → api-dev.pollis.com) — required, and
  since #987 the ONLY backend: OTP bootstrap, every remote write and every remote
  read go through the DS. Full
  sign-in verified end-to-end on an Android emulator against the live dev DS.
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
  bridge.
- **Push (client wired):** `lib/push/` + `usePushNotifications` — contextual
  permission (asked on first conversation open, not at login), token registration
  (`register_push_token`, best-effort), content-free tap/data handlers, and a
  Notifications row in Preferences. Push covers backgrounded/closed delivery;
  foreground delivery is the realtime path above.
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
    `push_to` field on `POST /v1/messages/send`. Payload is `{ h }` — an opaque
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
- ~~**`device_revoked`** self-sign-out on the inbox connection~~ — **DONE.**
  `useInboxRealtime` treats the event payload as advisory, confirms with
  `is_current_device_registered`, then signs out (`logout` + store reset +
  navigation to `/(auth)/email`). On-device testing of realtime + push is
  still pending.

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
