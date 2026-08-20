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
- If Expo complains about missing packages, first check that `node_modules` is inside `mobile/` and not at the repo root.

## Stack

- Expo SDK 55, React Native 0.83.6, React 19.2
- Expo Router v6 (file-based routing in `app/`)
- Reanimated 4, Gesture Handler 2.30
- `@gorhom/bottom-sheet` 5 for sheets
- `expo-camera`, `expo-secure-store`, `expo-notifications`
- `expo-image` (cached + blurhash), `react-native-blurhash`
- `lucide-react-native` icons, wrapped in `components/icons.tsx` (stable `Icon.*` API, strokeWidth pinned to 1.2 to match the design's monoline spec). `react-native-svg` is still used directly for the Initializing dot-field.
- Sora (UI) via `@expo-google-fonts/sora`. Monospace (crypto keys, e.g. the Security public-key line) uses the **system** mono face — `fonts.mono*` = `Platform.select({ ios: 'Menlo', android: 'monospace' })`, no bundled font.
- Rust core via `pollis-native` Turbo Module (uniffi-bindgen-react-native)

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

Smoke-tested on macOS Tahoe (26.4.1) + Xcode 26.4.1 + iOS 26.4 simulator. `version()` round-trips from Rust through the JSI bridge to the QR screen, then swipe through the card stack works end-to-end.

**Expo SDK 55 requires Xcode 26 / macOS 26.** Earlier Xcode/macOS hits `@MainActor` parse errors in `expo-modules-core` (see [expo/expo#42525](https://github.com/expo/expo/issues/42525) — closed, won't fix). SDK 54 is the last line that supports Xcode 16.x if a downgrade is ever needed.

### Dev loop — iOS

```bash
# One-time per machine
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
cargo install --git https://github.com/jhugman/uniffi-bindgen-react-native --tag 0.31.0-2 uniffi-bindgen-react-native
brew install cocoapods

# Per Rust change
cd mobile/modules/pollis-native
uniffi-bindgen-react-native build ios --config ubrn.config.yaml --and-generate
# Regenerates PollisNativeFramework.xcframework + cpp/ + src/generated/.

# Per build (also after ubrn regen)
cd mobile && pnpm expo run:ios
# prebuild runs implicitly if ios/ is missing; pod install runs if Podfile changed.
```

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
ndk;27.1.12297006 cmake;3.22.1`. A clean `gradlew assembleDebug` produces
`android/app/build/outputs/apk/debug/app-debug.apk` with `libpollis-native.so`
embedded for arm64-v8a / armeabi-v7a / x86_64.

**iOS build verified on macOS 26.4.1 (Tahoe) + Xcode 26.4.1, 2026-06-22.** A
clean run from a fresh checkout works end to end with no source changes: `ubrn
build ios` → `PollisNativeFramework.xcframework` (device `ios-arm64` + universal
`ios-arm64_x86_64-simulator` slices), `expo prebuild --platform ios` →
`pod install`, then `pnpm expo run:ios` compiles RN-from-source and launches on
the iPhone 17 Pro simulator; the bridge initializes against live Turso and the
app reaches the auth screen. Toolchain (all no-sudo): the three iOS Rust targets
(`rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios`),
`uniffi-bindgen-react-native` CLI pinned to `0.31.0-2`, and CocoaPods.

**Expo SDK 55 requires Xcode 26 / macOS 26** (verified still true June 2026: SDK
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
`cargo-ndk` agree, and pins the ubrn CLI to `0.31.0-2` (template alignment, see
below). It uses `expo prebuild` + Gradle **directly, never `eas build`** — that
predates the EAS project and still holds: the job needs no account, no token and
no queue. The release APK is signed with the throwaway **debug keystore** the
Expo template generates (no signing secret); real upload signing is blocked
(needs the Play console).

**`expo-doctor` is a required, clean gate.** The dependency set is pinned to the
Expo SDK 55 canonical versions (`expo install --check` / `bundledNativeModules`),
so doctor's version, duplicate-native-module, and missing-peer checks all pass.
Two things worth knowing:

- **`react-native` is deliberately held at `0.83.6`** even though SDK 55 now
  recommends `0.83.10`. `uniffi-bindgen-react-native` (`0.31.0-2`) generates C++
  JSI glue against RN 0.83.6's headers; moving RN underneath a pinned codegen is
  the `no member named 'string_to_buffer'` failure class. It is the **only**
  package excluded from doctor's version check, via `expo.install.exclude` in
  `package.json` (the narrowest supported mechanism — every other check stays
  live). Re-pin ubrn to an RN-0.83.10-aligned tag before dropping the exclusion;
  the two move together.
- **`react-native-worklets` is a direct dependency** (`0.7.4`, the version SDK 55
  pairs with Reanimated `4.2.1`). Reanimated 4 split worklets into its own native
  module; a native peer must be a direct dep so autolinking picks it up, or
  doctor's missing-peer check fails.

- **Doctor drifts red on its own.** Expo ships patch releases to the SDK 55 line
  continuously, so a set of packages that passed last month goes stale and fails
  the gate on **every** PR regardless of content (this happened: 10 packages at
  once). The fix is to re-pin to the versions doctor names, as a standalone PR.
  Do **not** reach for `expo install --fix` to do it — the Expo CLI shells out to
  `pnpm` without `--ignore-workspace`, which hoists mobile's tree into the root
  `node_modules` and destroys the install (see "Project isolation" above). Edit
  the versions in `package.json` by hand, then
  `pnpm install --ignore-workspace`.

`app.json` carries **no** `newArchEnabled` / `android.edgeToEdgeEnabled` — both are
now unconditional defaults in SDK 55 / RN 0.83 and were dropped from the config
schema, so listing them fails doctor's schema check for no behavioural gain.

## Store builds (no EAS — local `expo prebuild` + native tooling)

Published as a private individual account, Apple team `9JF7WWYMU2`. Version
bookkeeping is **local and explicit** in `app.json`: `ios.buildNumber` (string)
and `android.versionCode` (int) — bump both by hand for every store upload
(stores reject a re-used number), bump `version` for user-facing releases.

**Environment first.** `EXPO_PUBLIC_*` vars inline into the shipped JS bundle,
so before any store build check `mobile/.env`:

- `EXPO_PUBLIC_POLLIS_DELIVERY_URL` **must** be `https://api.pollis.com` (prod
  DS), not api-dev.
- `EXPO_PUBLIC_LIVEKIT_API_KEY` / `EXPO_PUBLIC_LIVEKIT_API_SECRET` /
  `EXPO_PUBLIC_RESEND_API_KEY` must **not** exist in `.env` — the code stopped
  reading them in #393, but a stale var would still be inlined into the bundle
  as plaintext. Delete them.

### iOS

```bash
cd mobile
pnpm expo prebuild -p ios
cd ios && pod install && cd ..
xcodebuild -workspace ios/Pollis.xcworkspace -scheme Pollis \
  -configuration Release -destination 'generic/platform=iOS' \
  archive -archivePath build/Pollis.xcarchive \
  DEVELOPMENT_TEAM=9JF7WWYMU2 -allowProvisioningUpdates
xcodebuild -exportArchive -archivePath build/Pollis.xcarchive \
  -exportOptionsPlist store/ExportOptions.plist -exportPath build/export \
  -allowProvisioningUpdates
```

`store/ExportOptions.plist` (committed — `ios/` is generated, so it can't live
there) sets `method: app-store-connect`, `teamID: 9JF7WWYMU2`,
`uploadSymbols: true`. Upload the resulting `.ipa` with Xcode Organizer or
`xcrun altool`/Transporter.

**Encryption export compliance:** `app.json` sets
`ITSAppUsesNonExemptEncryption: true` deliberately. `false` means "no
encryption, or only Apple-exempt encryption (auth, DRM, HTTPS-only)" — Pollis
uses standard-algorithm E2EE (MLS RFC 9420: AES-GCM, ML-DSA-44) for content
confidentiality in its own protocol, which is **not** on the exempt list, so
`false` would be a false export declaration. `true` is the honest answer; it
skips the per-build compliance interruption in App Store Connect and instead
requires (one-time/annual, operational not code): answering ASC's export
compliance questions (standard algorithms → mass-market self-classification,
annual self-classification report to the U.S. BIS by Feb 1), and the French
encryption declaration to ANSSI if distributing in France. Do not flip this to
`false` to silence ASC.

### Android

```bash
cd mobile
pnpm expo prebuild -p android
cd android && ./gradlew :app:bundleRelease
# → android/app/build/outputs/bundle/release/app-release.aab
```

Release signing is wired by `plugins/withReleaseSigning.js` (registered in
`app.json`), a local Expo config plugin that patches the generated
`android/app/build.gradle` at prebuild: `signingConfigs.release` reads
`POLLIS_UPLOAD_STORE_FILE` / `POLLIS_UPLOAD_STORE_PASSWORD` /
`POLLIS_UPLOAD_KEY_ALIAS` / `POLLIS_UPLOAD_KEY_PASSWORD` from gradle
properties (`~/.gradle/gradle.properties`) or the environment, and **falls back
to the debug keystore when unset** — so CI's `assembleRelease` keeps working
with zero config. Generate the upload keystore once with
`scripts/generate-upload-keystore.sh` (RSA-4096 at `~/.pollis/pollis-upload.jks`,
refuses to overwrite); it prints the gradle.properties lines. Never commit a
keystore (`*.jks` is gitignored) or its passwords.

---

## Rough edges — technical

### 1. `uniffi-bindgen-react-native` CLI ↔ npm package coupling

The Rust CLI binary (from `cargo install --git`) and the npm package at `mobile/node_modules/uniffi-bindgen-react-native/` share the generated C++ template. They **must be the same revision**, or CMake fails with errors like `no member named 'string_to_buffer'`.

If either side updates, rebuild the CLI from the exact git tag matching the npm dist-tag:

```bash
cargo install --git https://github.com/jhugman/uniffi-bindgen-react-native \
  --tag 0.31.0-2 uniffi-bindgen-react-native --force
```

Currently pinned: **`0.31.0-2`**. If you install the CLI from `main` without a tag, you'll hit this bug within a commit or two.

### 2. `android/android/…` double-nesting in `jniLibs` path

ubrn's CMakeLists resolves `${CMAKE_SOURCE_DIR}/android/src/main/jniLibs/` to `mobile/modules/pollis-native/android/android/src/main/jniLibs/`. It works — the ubrn config + generated CMake agree. Don't try to flatten it without regenerating both sides.

### 3. No justfile/Makefile yet

The "regenerate + gradle + install + launch" loop is manual four-command sequence. If iteration frequency rises, add a justfile.

### 4. NDK version pinning

RN 0.83.6 pins NDK **27.1.12297006** (r27b). Arch AUR's `android-ndk` package ships r29 at `/opt/android-ndk`. Both coexist: r29 at `/opt/android-ndk` (used by cargo-ndk), r27b at `/opt/android-sdk/ndk/27.1.12297006/` (used by gradle). Removing either breaks one side.

### 5. Arch AUR `android-sdk` is root-owned

The AUR `android-sdk` package installs to `/opt/android-sdk` as root-owned read-only. Gradle needs write access (licenses, package auto-install). Fix, done once:
```bash
sudo chown -R $USER:$USER /opt/android-sdk /opt/android-ndk
```
Redo if you reinstall the AUR package.

### 6. Old sdkmanager broken on JDK 17

`/opt/android-sdk/tools/bin/sdkmanager` uses `javax.xml.bind` (removed in JDK 9+) and crashes. Use the modern cmdline-tools at `/opt/android-sdk/cmdline-tools/latest/bin/sdkmanager` — installed manually from [commandlinetools-linux-11076708_latest.zip](https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip).

### 7. Expo SDK upgrade = always rerun `expo install --fix`

Expo packages version independently but their `latest` dist-tag tracks the current SDK. Bumping `expo` alone leaves sibling packages at the old SDK's versions, producing weird Kotlin compile errors in `expo-dev-menu` et al. Always:

```bash
cd mobile && pnpm expo install --fix
```

Prefer `pnpm expo install <pkg>` over `pnpm add <pkg>` for Expo-ecosystem packages — it consults the SDK version map.

### 8. `pnpm-lock.yaml` can get stale vs package.json after SDK bumps

`expo install --fix` updates package.json but sometimes leaves the install tree intact. If versions visible in `node_modules/<pkg>/package.json` don't match the package.json range, nuke and reinstall:

```bash
cd mobile && rm -rf node_modules pnpm-lock.yaml && pnpm install --ignore-workspace
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

The whole UI was rebuilt from the `design_handoff_pollis_mobile` spec — sci-fi
monochrome-amber, dark. Old light/Geist card-stack UI is gone.

```
app/
  _layout.tsx          root Stack, font loading, splash gate
  index.tsx            redirect → /(auth)/email
  (auth)/              email → otp → pin → initializing (gestureEnabled: false)
  (tabs)/              groups · direct · search · self (custom <TabBar>)
  group/[id].tsx       GroupDetail   (pushed; <Ctx> back bar, no tab bar)
  chat/[id].tsx        TextChat      (pushed; <Ctx> + composer)
  self/{preferences,user-settings,security}.tsx
components/
  ui.tsx               primitives: Screen, Crumb, SectionTitle, ListRow, Field,
                       Avatar, Chip, Button, Toggle, Card, Ctx, Diamond, Body…
  icons.tsx            monoline SVG icon set (Icon.*)
  TabBar.tsx           custom bottom tab bar (amber active indicator)
  PollisMark.tsx       auth-screen wordmark
theme/tokens.ts        palette / t(alpha) / semantic / type / r / space / layout
```

Navigation rules from the handoff: no header (every screen draws its own
`<Crumb>` at top); back lives at the **bottom** in the `<Ctx>` strip, not a
header button; sub-screens are stack pushes (pushed routes live outside
`(tabs)` so the tab bar is replaced by `<Ctx>`/composer).

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
  inbound images render via `components/Media.tsx` + `get_media_path`). The DS base URL
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
  QR pairing only. When voice is actually built, add the LiveKit/webrtc Expo
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
    `push_to` field on `POST /v1/messages/send`. Payload is `{conversationId,
    kind}` and a generic "New message" body — never plaintext or sender.
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

## ⚠️ Secrets architecture — KNOWN CRITICAL ISSUE (do before any public release)

Local dev currently feeds real credentials to the Rust bridge via
`EXPO_PUBLIC_*` env vars (`mobile/.env`, gitignored). **Expo inlines every
`EXPO_PUBLIC_*` value into the JS bundle at build time**, so they ship inside
the APK/IPA in plaintext — `unzip` + grep recovers them. This is fine for a dev
build on a trusted device; it is **not shippable**. What leaks today:

- ✅ **Resend key** — DONE, moved server-side (OTP runs on the DS).
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

The fix is an **authorized-secrets broker** — a thin server-side endpoint (a
Cloudflare Worker fits; CF is already in the stack) that holds the real secrets
and never ships them to the client:

1. **Bootstrap (pre-auth):** `POST /otp/request` + `/otp/verify` run server-side
   (server calls Resend). The phone never holds the Resend key.
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

Defined in `theme/tokens.ts`. One bg + one accent; everything else is a
translucent amber tier via `t(alpha)`.

- `palette.bg` `#0a0907` (just-above-black), `bg2`/`bg3` raised tiers
- Accent default `#fabf5a` — the same brand amber as the desktop app + website.
  It is **runtime-configurable** (Self → Preferences → Accent), mirroring
  desktop's accent picker. `t()`, `palette.accent`, `semantic.*`, and the
  `type.*` colors are getters that re-derive from the live accent; the
  `<ThemeProvider>` (in `components/theme.tsx`) holds the chosen hex and
  `<Screen>` + `<TabBar>` subscribe via `useTheme()`, so a change re-renders
  the whole tree. `palette.danger` `#c46a2e`.
- `semantic.*` — ink/ink2/mute/mute2/hair*/accentSoft/fieldBg/cardBg (all `t()` tiers)
- `r` = { sm: 3, lg: 4 }; `space` = irregular 6/8/10/12/14/18/22 (do NOT
  normalize to an 8px grid — the rhythm is part of the look)
- `type.*` — Sora UI scale + JetBrains Mono for keys. Letter-spacing is
  pre-converted from em (RN has no em). Uppercase labels/crumbs are tracked.

Aesthetic: "The Expanse readouts / Nier Automata menus" — one-handed, all
controls in the bottom command zone, top of every screen is a passive crumb.
Corner brackets appear **only** on the Initializing screen.
