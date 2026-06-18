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
  preferences. `bridge.rs` covers ~every command the hooks call.
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

- ~~**`get_livekit_token` bridge command**~~ — **DONE.** The pure-JWT mint now
  lives in the always-compiled `pollis-core/src/commands/livekit_jwt.rs`
  (desktop's `livekit/jwt.rs` re-exports it); the `get_livekit_token` arm in
  `bridge.rs` derives identity from the session (`{user_id}:{device_id}`,
  matching desktop's `connect_rooms`) and mints the token. Compiles clean for
  both host and `aarch64-apple-ios-sim`. Activates foreground realtime once
  `EXPO_PUBLIC_LIVEKIT_URL` is set (#185). On-device verification still pending.
- **Push backend (code DONE; credentials pending).** The Rust side is wired and
  compiles for host + `aarch64-apple-ios-sim`:
  - `push_token` Turso table — migration `000006_push_token.sql` (additive
    `CREATE TABLE`/`CREATE INDEX`; ships to prod via the release pipeline's
    `db-apply.sh`).
  - `register_push_token` — `bridge.rs` arm → `commands::push::register_push_token`
    (upsert keyed on the token, so a re-register reassigns ownership).
  - Content-free fanout — `commands::push::notify_new_message`, spawned
    fire-and-forget from `send_message` after the LiveKit publish. Resolves
    conversation members (minus sender), looks up their tokens, and POSTs a
    batched, **content-free** alert to Expo (`{conversationId, kind}` only —
    no plaintext/sender; generic "New message" body). Desktop runs it too
    (no tokens → no-op), which is what lets a desktop send wake a phone.
  - **Still operational (need EAS, not code):** `eas init` to populate
    `expo.extra.eas.projectId` in `app.json` (the JS degrades gracefully
    without it), plus APNs key / FCM v1 service-account credentials in EAS.
    On-device delivery test is the final gate (#344).
- **webrtc Expo config plugin** + an AndroidManifest mic/camera **removal** rule
  so the data-only realtime path adds no voice/video permission.
- **`device_revoked`** self-sign-out on the inbox connection (needs the local
  device id + a sign-out path), and on-device testing of realtime + push.

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
