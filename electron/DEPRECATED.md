# ⚠️ DEPRECATED — legacy Electron shell

**Pollis ships on Tauri.** This `electron/` directory is the *previous* desktop
shell, kept only as a rollback path. It is no longer the build that ships, and
new work should not target it.

## History

Pollis shipped on Electron through the v1.x line, then migrated back to Tauri
(`spike/tauri-revival`, PRs #386 / #389). As part of that cutover:

- The Tauri release workflow was re-armed on tag push; the Electron release
  pipeline (`.github/workflows/electron-release.yml`) is now **disabled** —
  commented out and inert (#386). Pollis no longer builds or ships the Electron
  app.
- An in-app **end-of-life download banner** was added to steer Electron users to
  the Tauri build (#386).

## What this means

- **Current shell:** `src-tauri/` (Tauri 2). Renderer → Tauri `invoke` →
  command handlers backed by `pollis-core`.
- **This shell:** `electron/` (Electron 33) → preload `electronAPI` →
  `pollis-node` (napi-rs) → `pollis-core`. Frozen.
- The shared frontend (`frontend/`) still runs under both via the
  shell-agnostic bridge at `frontend/src/bridge/invoke.ts`, which prefers Tauri
  and falls back to `electronAPI`.

## If you must run the legacy shell (rollback only)

```bash
pnpm dev:electron                         # legacy Electron dev
pnpm --filter @pollis/electron build      # build the Electron main bundle
pnpm --filter @pollis/electron exec electron-builder --config build/electron-builder.yml
```

Do not add features here. Anything new belongs in `src-tauri/`. When the rollback
window has safely closed, this directory (and the `pollis-node` napi-rs binding it
depends on) can be removed outright.
