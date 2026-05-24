# Electron migration

**Status:** in progress
**Branch:** `feature/electron-migration`
**Goal:** swap Tauri's webview for Electron's Chromium renderer; keep every line of Rust (pollis-core, MLS, voice, libwebrtc, R2, Turso) running in-process via napi-rs.

## Why

WebKitGTK on Linux ships without WebRTC. Three days of custom WebKit builds, a wry fork, and a half-finished Rust↔webview loopback bridge proved that even when we get `RTCPeerConnection` working in the webview, GStreamer's SDP parser chokes on LiveKit's offer (`GstIntRange` assertion). The maintenance burden of a forked WebKitGTK is permanent. Chromium handles every WebRTC edge case out of the box.

We're staying with Rust for everything that matters — memory safety, no GC, libwebrtc, MLS, voice DSP. Electron is only the renderer. `pollis-core` was designed runtime-agnostic specifically for this; it already has no Tauri dependency and an `EventSink` trait abstraction for non-Tauri hosts.

## Non-goals

- No business logic moves into JS. pollis-core stays the source of truth.
- Voice does NOT migrate to `livekit-client`. Stays Rust (better APM/RNNoise/device routing already tuned).
- No CEF detour. Electron's WebRTC story is the mainstream battle-tested one; cef-rs has open `tauri-cef` bugs we don't need to be early adopters of.
- No native overlay / video-hole architecture. Considered, declined: Wayland forbids client window positioning by design; an inside-window NSView/HWND/GtkOverlay would need yet another wry fork plus a from-scratch wgpu video renderer.

## Architecture

```
pollis-core (Rust workspace crate — UNCHANGED)
  ├── loaded via napi-rs into → electron/ (TS main process)      [NEW]
  ├── still loaded via #[tauri::command] shims → src-tauri/      [stays alive during migration, deleted Phase 8]
  └── exported via uniffi → mobile bindings                       [unchanged]

frontend/ (React + Vite + Tailwind — mostly unchanged)
  └── invoke() routed through frontend/src/bridge.ts
        ├── if window.__TAURI_INTERNALS__ → tauri invoke
        └── if window.electronAPI         → electron ipcRenderer.invoke
```

**Decision: napi-rs over sidecar.** One process. Zero IPC overhead for Rust calls. Crash isolation handled via `std::panic::set_hook` converting panics to napi errors. libwebrtc threads cohabit fine with Node's libuv. Used in production by 1Password, Bitwarden, Storybook, Prisma, swc.

**Decision: side-by-side, not replace-in-place.** Both binaries build from the same `pollis-core` + `frontend/`. `pnpm dev` keeps working; `pnpm dev:electron` is new. We flip the default only when Electron is at full parity. Never break main.

## Phases

| # | Scope | Effort | Unblocks |
|---|-------|--------|----------|
| 0 | Inventory + scaffold + bridge (5 parallel agents) | 1 day | everything |
| 1 | Hello-world Electron ↔ napi-rs round trip with one real command | half day | 2–6 |
| 2 | Port every `#[tauri::command]` to napi + ipcMain | 1–2 days | 6 |
| 3 | Port Channel events via napi `ThreadsafeFunction` | half day | 6 |
| 4 | Platform plumbing: tray, autostart, deep links, single instance, window state, notifications | 1–2 days | 7 |
| 5 | Permissions / entitlements (Info.plist, electron-builder, sandbox flags) | half day | 7 |
| 6 | WebRTC via livekit-client; screenshare publish via `getDisplayMedia` | half day | — |
| 7 | electron-builder packaging + GH Actions matrix + electron-updater + macOS notarization | 1–2 days | flip default |
| 8 | Delete `src-tauri/`, wry fork pin, custom WebKit script, loopback bridge; update CLAUDE.md + wiki | half day | done |

**Total: 6–9 focused days plus testing buffer.**

## Risks (ranked)

1. **napi-rs `ThreadsafeFunction` for high-frequency event streams** (voice peak meter, screenshare frame stats). Documented pattern but the first wiring needs care.
2. **Linux Wayland screen capture via PipeWire + xdg-desktop-portal in Electron** — should work, but verify early before depending on it.
3. **macOS notarization** — always a 2-hour debug session the first time per bundle id. Apple Developer credentials already exist from Tauri builds; carry them over.
4. **Single-instance + deep-link argv handling on Linux** — fiddlier than the docs admit. Tauri's `tauri-plugin-deep-link` papered over this; we'll feel it.

## Done criteria

- `pnpm dev:electron` runs Pollis with full feature parity to `pnpm dev`
- Screenshare receive + publish works on Linux Wayland, Linux X11, macOS, Windows
- Voice works on all three platforms
- Every Tauri command has a napi equivalent
- Bundled artifacts (`.dmg`, `.exe`/`.nsis`, `.AppImage`, `.deb`) build in CI on native runners
- `src-tauri/` deleted
- CLAUDE.md + `.codesight/wiki/` updated

## Parallel work fan-out (Phase 0)

| Agent | Output | Touches |
|-------|--------|---------|
| A | `docs/electron-migration-inventory.md` — every command/event/Tauri API/plugin/config detail | read-only |
| B | `frontend/src/bridge.ts` + import refactor across `frontend/` | `frontend/` |
| C | `pollis-node/` crate + `electron/` main process + ping() smoke test | new dirs + workspace `Cargo.toml` + root `package.json` |
| D | `electron/electron-builder.yml` + `entitlements.mac.plist` + Info.plist fragment | `electron/build/` |
| E | `docs/electron-migration-plumbing.md` — Tauri→Electron API translation table | read-only |

Agents run in parallel via git worktrees. Their branches merge back into `feature/electron-migration` once verified.

## Out of scope for this ticket

- Mobile (Tauri mobile builds are unaffected; pollis-core's uniffi exports continue working).
- Website (`website/` is static HTML on Cloudflare, unrelated).
- Replacing voice with livekit-client (separate decision; not in this migration).
