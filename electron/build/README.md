# electron/build

electron-builder configuration + macOS entitlements for packaging Pollis.

## Files

- `electron-builder.yml` — bundle targets, icons, signing, platform deps. Translated 1:1 from `src-tauri/tauri.conf.json`.
- `entitlements.mac.plist` — hardened-runtime entitlements (mic, camera, network client/server for the local media loopback).

## Local builds

```bash
pnpm --filter @pollis/electron exec electron-builder --mac --x64
pnpm --filter @pollis/electron exec electron-builder --linux
pnpm --filter @pollis/electron exec electron-builder --win
```

Output lands in `electron/release/`.

## Skipping signing locally

```bash
CSC_IDENTITY_AUTO_DISCOVERY=false pnpm --filter @pollis/electron exec electron-builder --mac
```

## Icons

Icons resolve from `../../src-tauri/icons/` — the existing Tauri icon set is reused verbatim (`AppIcon.icns`, `icon.ico`, plus the `*.png` sizes for Linux).

## Status

This directory is for local testing only. Phase 7 of the Tauri -> Electron migration wires up the CI release pipeline, code-signing secrets, and `electron-updater` publish targets. Until then the `publish:` and `protocols:` blocks in `electron-builder.yml` are commented out with `TODO:` markers.
