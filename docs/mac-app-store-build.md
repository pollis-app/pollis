# Mac App Store build

Scaffolding for submitting Pollis to the Mac App Store (MAS). Tracks issue
[#157](https://github.com/actuallydan/pollis/issues/157). This doc covers
what already exists on disk and what still needs to happen before an
actual submission.

## What lives where

| Purpose | File |
| --- | --- |
| Rust feature gate (compiles out updater + process-relaunch) | `src-tauri/Cargo.toml` → `[features].mas` |
| MAS-specific Tauri config overlay | `src-tauri/tauri.mas.conf.json` |
| MAS sandbox entitlements | `src-tauri/Entitlements.mas.plist` |
| Non-MAS-only capability (`updater` + `process:allow-*`) | `src-tauri/capabilities/updater/updater.json` |
| Frontend build flag | `VITE_MAS_BUILD=true` (read via `import.meta.env.VITE_MAS_BUILD`) |
| Manual build workflow | `.github/workflows/build-mas.yml` |

The default (non-MAS) build is unchanged — `pnpm tauri build` still
produces the Developer ID–notarised bundle that ships to `cdn.pollis.com`
via `desktop-release.yml`.

## Building MAS locally

Check it compiles:

```bash
cd src-tauri
cargo check --no-default-features --features mas
```

Produce an unsigned `.app` bundle (good for smoke-testing sandbox
entitlements — you'll need to `codesign --deep -s - Pollis.app` ad-hoc
and run under `sandbox-exec` to verify the entitlements actually hold):

```bash
cd frontend && VITE_MAS_BUILD=true pnpm build
cd .. && pnpm tauri build \
  --target universal-apple-darwin \
  --no-default-features \
  --features mas \
  --config src-tauri/tauri.mas.conf.json \
  --bundles app
```

## What this scaffold does NOT do

Still required before a real submission:

1. **Replace `osascript` + `use framework "AppKit"` clipboard read**
   (`src-tauri/src/lib.rs::read_clipboard_files`) with an in-process
   `NSPasteboard` call via `cocoa` / `objc`. Under the sandbox,
   AppleScript loading AppKit requires
   `com.apple.security.automation.apple-events` and typically triggers a
   consent prompt — App Review flags this. Tracked as item 2 of #157.
2. **Tighten `tauri-plugin-shell` and `tauri-plugin-fs` capability
   scopes** — both are broadly opened in `capabilities/default.json`.
   Narrow to the exact surfaces the app uses. Tracked as item 4 of #157.
3. **Signing material** — the workflow references placeholder secret
   names (`MAS_CERTIFICATE_P12`, `MAS_CERTIFICATE_PASSWORD`,
   `MAS_PROVISIONING_PROFILE`, `MAS_TEAM_ID`) but does not attempt to
   import them. Before the first real build, you need to:
   - Create a "Mac App Distribution" cert and a "Mac Installer
     Distribution" cert in the Apple Developer portal.
   - Create an App Store provisioning profile for `com.pollis.app`.
   - Stuff both (plus team ID + a 1Password-generated p12 password) into
     GitHub Secrets under the names above (and into Doppler for local).
   - Uncomment the `Signing setup` / `Submit to App Store Connect`
     blocks in `build-mas.yml` and swap the STUB warnings for the real
     `security import` / `xcrun altool` calls.
4. **Keychain access group** — `Entitlements.mas.plist` has
   `$(AppIdentifierPrefix)com.pollis.app` as a placeholder. The
   `$(AppIdentifierPrefix)` variable is an Xcode build setting; once the
   real team ID is known, expand it to `<TEAM_ID>.com.pollis.app`
   (either by codesign variable substitution or by plain-text edit in
   the entitlements file).
5. **Transporter / altool dry run** — before flipping the submission
   live, validate the `.pkg` with
   `xcrun altool --validate-app --type osx --file Pollis.pkg ...`.
6. **SQLCipher license audit** — `rusqlite`'s `bundled-sqlcipher`
   feature can pull either BSD- or GPL-licensed SQLCipher depending on
   upstream config. Confirm the BSD variant before shipping to MAS.

## Relationship to the Developer ID release pipeline

`.github/workflows/desktop-release.yml` is the existing pipeline that
tag-triggers a Developer ID build, notarises it, and uploads to R2 for
direct download. `build-mas.yml` is **strictly additive**:

* It's `workflow_dispatch`-only — no tag / push triggers.
* It uses a separate Cargo feature set.
* It uses a separate entitlements file.
* It does not publish `update.json`, `latest.json`, or anything to R2.

Both pipelines can coexist on `main` without stepping on each other.
