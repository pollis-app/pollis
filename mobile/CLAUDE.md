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
- `lucide-react-native` for icons
- Geist font via `@expo-google-fonts/geist`
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
adb shell am start -n com.anonymous.mobile/.MainActivity
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

```
JAVA_HOME=/usr/lib/jvm/java-17-openjdk
ANDROID_HOME=/opt/android-sdk
ANDROID_SDK_ROOT=/opt/android-sdk
ANDROID_NDK_ROOT=/opt/android-ndk
ANDROID_NDK_HOME=/opt/android-ndk
```

Persisted in `~/.bashrc` and `.envrc`. If you're in a fresh shell and builds fail with "SDK location not found", these are missing.

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

---

## Rough edges — UX (known, not yet fixed)

Tracked here so they don't get lost between sessions.

1. **Sign-out drawer is one-way.** Once pulled down, you can't swipe the card stack back up to dismiss the drawer — you have to start a horizontal swipe instead. Needs bidirectional gesture handling on the pull.
2. **No reply-focus gesture on the current card.** Double-tap and swipe-up should both focus the current card and open an inline textarea for reply. Currently reply only triggers on swipe-right (which also dismisses the card). Focus + inline reply is a distinct UX from swipe-to-reply.
3. **"Sign out" → "Disconnect" with confirmation.** Rename label. Show a confirmation informing the user: *this device will stop receiving messages AND cannot reconnect without re-pairing from a desktop*. Use whichever native mobile pattern reads cleanest — a `@gorhom/bottom-sheet` confirmation sheet is the expected default here.
4. **Empty-state "pull down to sign out" hint is dead.** The pull-down gesture only works on the card stack, not the `<EmptyState>` screen. Either wire the gesture on empty state too, or drop the hint text.

## Rough edges — out-of-scope stubs (by design, not bugs)

- QR scan payload is ignored — any scanned code (or tapping Skip) routes to `/stack`.
- Reply sheet send button closes the sheet; does not actually send anything.
- Sign-out just routes to `/`; does not clear secure-store or any credentials.
- Notification permission is requested on first card-stack entry; no handlers are wired.
- No real MLS, auth, or Pollis backend integration. The only Rust call live is `version()` (shown below Skip button on the QR screen as a smoke-test readout).

## Design tokens (for consistency across new screens)

Defined in `theme/tokens.ts`. Summary:

- Background: `#f9f9f9`
- Surface container low (secondary): `#f2f4f4`
- Surface container lowest (floating): `#ffffff`
- Tertiary accent (electric indigo): `#4a4bd7` — use sparingly, only for critical focus / notifications
- On-surface text: `#2d3435`
- Card radius: 24px (`radius.xl`)
- **No borders or divider lines** — separate via tonal shifts or whitespace
- Font: Geist (all text goes through `components/Text.tsx`)

Aesthetic target: "The Expanse hand terminal" — minimalist, bottom-anchored, one-handed. All interactive elements in the bottom ~40% of the screen. No top nav, no back buttons in corners.
