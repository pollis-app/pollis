# Electron migration

**Status:** code-complete, awaiting end-to-end GUI verification + cutover decision
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

**Decision: side-by-side, not replace-in-place.** Both binaries build from the same `pollis-core` + `frontend/`. As of v1.1.0 `pnpm dev` runs the Electron flow; the legacy Tauri path stays available as `pnpm dev:tauri`. Tauri's `build:*` scripts are similarly renamed to `build:tauri:*`. Never break main.

## Phases

| # | Scope | Status | Result |
|---|-------|--------|--------|
| 0 | Inventory + scaffold + bridge (5 parallel agents) | ✅ | `docs/electron-migration-inventory.md` + `docs/electron-migration-plumbing.md` + `pollis-node/` crate + `electron/` skeleton + `frontend/src/bridge.ts` + `electron/build/*` |
| 1 | Hello-world Electron ↔ napi-rs round trip | ✅ | `invoke()` dispatcher + AppState bootstrap; one real command end-to-end |
| 2 | Port every `#[tauri::command]` to napi + ipcMain | ✅ | **144 dispatch arms** across 19 modules; 5 parallel agents |
| 3 | Port Channel events via napi `ThreadsafeFunction` | ✅ | `NapiSink` + `RawNapiSink` + 5 subscribe arms wired; `terminal_write` (binary IPC body inbound) deferred |
| 4 | Platform plumbing | ✅ | Preload bridge with 20+ capabilities + 11-file split (`frontend/src/bridge/{invoke,window,dialog,fs,shell,clipboard,notifications,app,image,updater,runtime}.ts`); 18 frontend files refactored to route through bridge |
| 5 | Permissions / entitlements / preload sandbox | ✅ | `sandbox: true`; entitlements + Info.plist already in `electron-builder.yml` from Phase 0D |
| 6 | WebRTC via livekit-client | ✅ | `frontend/src/screenshare/livekitView.ts` joins as hidden `${userId}:view` participant; remote tracks render via `<video srcObject>`; `getDisplayMedia()` publishes via `LocalParticipant.publishTrack`; new `get_livekit_view_token` mints JWT with `hidden: true` |
| 7 | Release pipeline + electron-updater | ✅ | `.github/workflows/electron-release.yml` (mac/win/linux matrix); `electron-updater` wired with GitHub Releases publish target; bridge `updater.ts` ports cleanly under Electron |
| 8 | Cleanup / docs | partial | Tauri kept alive for side-by-side. CLAUDE.md untouched (per "update docs once we know the full shape" guidance). No dead code on this branch to delete — wry fork + custom WebKit live on a separate (stashed) branch. |

## Risks (ranked)

1. **napi-rs `ThreadsafeFunction` for high-frequency event streams** (voice peak meter, screenshare frame stats). Documented pattern but the first wiring needs care.
2. **Linux Wayland screen capture via PipeWire + xdg-desktop-portal in Electron** — should work, but verify early before depending on it.
3. **macOS notarization** — always a 2-hour debug session the first time per bundle id. Apple Developer credentials already exist from Tauri builds; carry them over.
4. **Single-instance + deep-link argv handling on Linux** — fiddlier than the docs admit. Tauri's `tauri-plugin-deep-link` papered over this; we'll feel it.

## Done criteria

| | Criterion | Status |
|---|---|---|
| ✅ | Every Tauri command has a napi equivalent | 144 arms — every shim ported or stubbed-with-reason |
| ✅ | `pnpm dev` runs Pollis (Electron) with feature parity to the legacy `pnpm dev:tauri` | verified in dogfood — voice + screenshare + DMs + groups all working |
| ⬜ | Screenshare receive + publish works on Linux Wayland, Linux X11, macOS, Windows | verified at the wiring level; per-platform GUI verification pending |
| ⬜ | Voice works on all three platforms | Rust voice path untouched; should work identically |
| ⬜ | Bundled artifacts build in CI on native runners | workflow scaffolded; needs a real tag push to verify |
| ⬜ | `src-tauri/` deleted | held back until cutover decision |
| ⬜ | CLAUDE.md + `.codesight/wiki/` updated | held back per "update docs once we know the full shape" |

## Deferred items (track separately when cutover happens)

- **`terminal_write` binary IPC body** (Phase 3): keystroke hot path. Currently stubs. Needs an `invoke_raw(cmd, Buffer, headers)` napi function or a bridge-side Uint8Array → JSON-array fallback (acceptable for low-bandwidth keystrokes).
- **Custom screen-share picker UI** (Phase 6): Chromium's `setDisplayMediaRequestHandler` currently returns the first source / uses the system picker where available. The Tauri-era in-app `ScreenSharePicker.tsx` could be revived if a richer picker is desired.
- **Drag-drop producer rewrite** (Phase 4): `windowOnDragDropEvent` is wired both ways but main never emits — Electron's renderer-side `DataTransfer.files` already works. The `AppShell.tsx:120` producer that dispatches `pollis:pathdrop` needs to switch from Tauri's `onDragDropEvent` to DOM events.
- **Windows badge overlay icon** (Phase 4): `windowSetBadgeIcon` is a no-op stub. `setBadgeCount` covers macOS/Linux. Win11 uses `setOverlayIcon` for the small badge on the taskbar icon.
- **Tauri custom IPCs in `src-tauri/src/lib.rs`** (`read_clipboard_files`, `read_clipboard_image_to_temp`, `hide_window`): Electron-side equivalents wired through the new bridge methods. Originals stay for the Tauri build until Phase 8 deletion.
- **Cutover decision**: when Electron is dogfooded enough to flip default, run Phase 8 cleanup (delete `src-tauri/`, drop dual-runtime branches in bridge, retire `desktop-release.yml`, update CLAUDE.md + `.codesight/wiki/`).

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
