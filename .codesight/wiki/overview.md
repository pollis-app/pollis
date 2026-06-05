# Overview

## Stack

**Electron 33** desktop app: React/TypeScript renderer + Node main process + a native Rust addon (`pollis-node`) loaded into the main process via `require('pollis-node')`. The renderer reaches the Rust backend through the preload bridge `window.electronAPI`.

- **No backend server.** The Rust core (loaded into the Electron main process by `pollis-node`) connects directly to Turso (1 network hop). All business logic runs in the main process; the renderer invokes commands via `window.electronAPI.invoke(cmd, args)`.
- **Media stays in Rust.** Voice and screenshare run inside `pollis-core` via the `livekit` + `libwebrtc` crates, not in the renderer. Two reasons: (a) cross-platform parity — the same Rust pipeline is consumed by mobile through uniffi bindings; (b) predictable allocation — multi-MB media buffers passed through V8's heap produce visible GC stutter, while Rust's manual allocation does not. The renderer's Chromium does have WebRTC available, but JS-based media APIs are reserved for small UI previews, not capture/publish/render.

## Data Flow

```
React component
  → invoke("command_name", { args })            // from frontend/src/bridge
    → window.electronAPI.invoke(...)            // preload (electron/src/preload.ts)
      → ipcRenderer.invoke("invoke", cmd, args)
        → ipcMain.handle("invoke", ...)         // main (electron/src/main.ts)
          → pollis-node dispatch                // pollis-node/src/dispatch/*.rs
            → pollis_core::commands::*          // pollis-core/src/commands/*.rs
              → Turso (remote, metadata) or SQLite (local, secrets)
            ← Result<T>
        ← Result<T>
    ← Result<T>
  ← React Query cache
```

The dispatch layer in `pollis-node/src/dispatch/<module>.rs` is mechanical — one match arm per command, deserialising the JSON args and calling the corresponding `pollis_core::commands::…` function. Real logic lives in the `pollis-core` workspace crate so other front-ends — a CLI, a TUI, mobile via uniffi — can consume it without any shell-runtime dependency. Legacy `#[tauri::command]` shims under `src-tauri/src/commands/` are preserved for rollback.

**React Query** is the source of truth for remote data. **MobX** holds only UI state (current user ref, transient session data).

## Frontend Routing

**TanStack Router** with **memory history** (no browser URL bar in a desktop app). Routes defined in `frontend/src/router.tsx`. `AppShell` is the root route component (sidebar + `<Outlet />`).

Key routes:
- `/` — Root (home)
- `/groups/$groupId` — Group landing
- `/groups/$groupId/channels/$channelId` — Text channel
- `/groups/$groupId/voice/$channelId` — Voice channel
- `/groups/$groupId/members`, `/groups/$groupId/invite`, `/groups/$groupId/leave` — Group management
- `/dms/$conversationId` — DM conversation
- `/preferences`, `/settings`, `/security` — User settings
- `/invites`, `/join-requests` — Pending invites and join requests
- `/search` — Global search

Navigation uses `useNavigate()` from TanStack Router. Pages use `useParams()` for route params (`$groupId`, `$channelId`, etc.).

## Project Structure

```
pollis-core/src/
  commands/          # Real command implementations (auth, groups, messages, mls, voice, ...)
  db/                # Turso + local SQLite connections + numbered migrations
  config.rs          # Env var config
  keystore.rs        # OS keystore (keyring crate)
  state.rs           # AppState shared across commands
  realtime.rs        # LiveKit room manager + event dispatch
  sink.rs            # EventSink trait (frontend-channel abstraction)
  signal/            # MLS storage backend
  lib.rs             # uniffi exports for mobile bindings

pollis-node/src/
  lib.rs             # napi-rs addon entry; loads .env.development; ThreadsafeFunction registration
  state.rs           # Per-process AppState shared with pollis-core
  events.rs          # Rust → Node event channel plumbing
  dispatch/          # invoke dispatch — one arm per command module (active path)

electron/src/
  main.ts            # Electron main process — loads pollis-node, registers ipcMain handlers, owns BrowserWindow + auto-updater
  preload.ts         # Exposes window.electronAPI to the renderer

src-tauri/src/       # Legacy Tauri shell (retained for rollback; not the active path)
  commands/          # Thin #[tauri::command] shims forwarding to pollis_core
  sink.rs            # ChannelSink adapter (Tauri's ipc::Channel → EventSink)
  test_harness.rs    # Multi-client integration harness (feature = "test-harness")
  lib.rs             # tauri::Builder, plugin setup, invoke_handler!, lifecycle
  main.rs            # Binary entry

frontend/src/
  bridge/            # Runtime-host bridge — invoke/Channel/window/dialog/fs/shell/app/updater route through window.electronAPI; legacy Tauri fallback retained
  components/        # React components (Auth/, Layout/, Message/, ui/, Voice/, ...)
  hooks/queries/     # React Query hooks (useGroups, useMessages, usePreferences, ...)
  pages/             # Route pages
  services/          # api.ts (invoke wrappers), r2-upload.ts
  stores/            # MobX class singletons (appStore.ts)
  types/             # TypeScript types

website/             # Static marketing site (Cloudflare Pages, not part of the app)
```

## Storage Model

| Store | Contents | Never stores |
|-------|----------|--------------|
| **Turso** (remote) | Users, groups, channels, membership, public keys, encrypted message envelopes, MLS commit log, MLS welcomes, GroupInfo | Message plaintext, private keys |
| **SQLite** (local, per-user, encrypted) | Decrypted messages, MLS group state (`mls_kv`), preferences cache | User profiles, groups, channels (fetched from remote) |
| **OS Keystore** | Ed25519 identity key pair, session token, device ID, DB encryption key | |

## Security Model

**Trusted:** User's device, local database, the signed Electron app binary (main process + preload + `pollis-node`/`pollis-core` addon) at the installed version, OS keystore.
**Untrusted:** Network, Turso, server operators.

## Realtime

LiveKit rooms carry realtime events (new_message, membership_changed, voice_joined, etc.). The Rust event loop in `livekit.rs` receives data events, dispatches them through `pollis-node`'s Rust → Node `ThreadsafeFunction`; the Electron main process forwards each envelope via `webContents.send("channel:<id>", payload)`; the renderer subscribes through `window.electronAPI.channelOn(id, handler)`. MLS operations (process commits, poll welcomes) fire as needed.

Events are a **convenience for speed**, not a correctness requirement. All MLS state is also read from the DB on every message send/receive, so offline devices catch up when they next interact.

## Voice

Voice channels run entirely in Rust by design — cross-platform parity with mobile (same uniffi-exposed `pollis-core`) and predictable allocation under heavy media buffers. The capture pipeline is `cpal mic → optional RNNoise → WebRTC APM (AGC2 + NS + HPF + AEC) → LiveKit publish`; remote playback is `NativeAudioStream → per-track buffers → mixer task (10 ms tick) → single cpal output stream`, which is also where the AEC render reference is tapped. Settings (mic boost, AGC target, NS level, AEC, click suppression) live in user preferences and apply mid-call via `set_voice_audio_processing` without rejoining. See [audio-processing.md](./audio-processing.md) for the full pipeline, framing constraints, and tuning surface.

Voice is end-to-end encrypted: each frame is AES-128-GCM-encrypted by libwebrtc's `FrameCryptor` post-Opus, keyed by a 32-byte secret derived from the channel's MLS group (`MlsGroup::export_secret("pollis/voice/v1", epoch, 32)`). The LiveKit SFU forwards ciphertext only; the key rotates automatically on every MLS epoch advance. See `pollis-core/src/commands/voice_e2ee.rs` and the "End-to-end encryption" section of [audio-processing.md](./audio-processing.md#end-to-end-encryption).

---
_Back to [index.md](./index.md)_
