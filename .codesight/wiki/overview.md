# Overview

## Stack

**Tauri 2** desktop app: a React/TypeScript renderer running in the OS WebView (WebKitGTK on Linux, WKWebView on macOS, WebView2 on Windows), with the Rust backend compiled into the Tauri host process. The renderer reaches the backend through the shell-agnostic bridge `frontend/src/bridge`, which routes to Tauri's `invoke`.

- **No backend server.** The Rust core (compiled into the Tauri host process) connects directly to Turso (1 network hop). All business logic runs in `pollis-core`; the renderer invokes commands via `invoke(cmd, args)` through the bridge.
- **Media stays in Rust.** Voice and screenshare run inside `pollis-core` via the `livekit` + `libwebrtc` crates, not in the renderer. Two reasons: (a) cross-platform parity — the same Rust pipeline is consumed by mobile through uniffi bindings; (b) predictable allocation — multi-MB media buffers passed through a JS heap produce visible GC stutter, while Rust's manual allocation does not. This also sidesteps WebKitGTK's lack of WebRTC on Linux — the Rust path is the only one that works on every target.

## Data Flow

```
React component
  → invoke("command_name", { args })            // from frontend/src/bridge
    → @tauri-apps/api/core invoke(...)          // bridge routes to Tauri
      → #[tauri::command] shim                   // src-tauri/src/commands/*.rs
        → pollis_core::commands::*              // pollis-core/src/commands/*.rs
          → Turso (remote, metadata) or SQLite (local, secrets)
        ← Result<T>
      ← Result<T>
    ← Result<T>
  ← React Query cache
```

The `#[tauri::command]` shims in `src-tauri/src/commands/<module>.rs` are mechanical — each is registered in `src-tauri/src/lib.rs`'s `invoke_handler!` and forwards the JSON-shaped call to the corresponding `pollis_core::commands::…` function. Real logic lives in the `pollis-core` workspace crate so other front-ends — a CLI, a TUI, mobile via uniffi — can consume it without any shell-runtime dependency. **Edit `pollis-core`, not the shims.**

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

src-tauri/src/       # Tauri desktop host — the shipping shell
  commands/          # Thin #[tauri::command] shims forwarding to pollis_core
  sink.rs            # ChannelSink adapter (Tauri's ipc::Channel → EventSink)
  test_harness.rs    # Multi-client integration harness (feature = "test-harness")
  lib.rs             # tauri::Builder, plugin setup, invoke_handler!, lifecycle
  main.rs            # Binary entry

frontend/src/
  bridge/            # Runtime-host bridge — invoke/Channel/window/dialog/fs/shell/app/updater route through Tauri's invoke
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

**Trusted:** User's device, local database, the signed Tauri application binary (Tauri host + WebView renderer + `pollis-core`) at the installed version, OS keystore.
**Untrusted:** Network, Turso, server operators.

## Network egress & the closed-overlay relay

Every outbound connection goes to a fixed, small set of first-party hosts: Turso (libSQL/Hrana), the Delivery Service, Cloudflare R2, and LiveKit. The optional **closed-overlay relay** (`pollis-relay` crate; design `docs/relay-overlay-design.md` §14) can route the metadata-sensitive **control plane** through a first-party relay so the services see a relay's IP instead of the user's. It is **off by default** and inert unless `POLLIS_OVERLAY` selects a non-off mode at runtime.

- **Config** (`config.rs`): `POLLIS_OVERLAY` = `off` (default) | `prefer` | `strict`; `POLLIS_OVERLAY_RELAY` = the relay endpoint. Unknown/empty → `off`.
- **Wiring** (`pollis-core/src/net/overlay.rs`): when a non-off mode is set, `AppState::new` starts a loopback SOCKS5 shim (`pollis_relay::OverlayShim`) once and stores the `OverlayHandle` on `AppState.overlay`. The routing policy sends Turso/DS/R2 through the overlay and keeps **LiveKit direct** (the media plane, §6.4).
- **Seams:** control-plane HTTP goes through `net::overlay::http_client(state.overlay.as_ref())` (a `socks5h://` reqwest client) instead of `reqwest::Client::new()`; the libsql Turso connection uses `RemoteDb::connect_with_overlay`, which attaches `overlay_connector` (a SOCKS-dialing `hyper-rustls` connector) — the inner TLS still terminates at the real service, so the relay only forwards opaque bytes.
- **Off = unchanged:** with `POLLIS_OVERLAY` unset, `overlay` is `None`, `http_client(None)` is a plain client, and `RemoteDb` takes libsql's unchanged `.build()` path — byte-for-byte the pre-overlay behavior.
- **Left direct:** Expo push (`push.rs`, non-first-party host) stays direct — documented residual leak (§14.4). The `ureq` transparency verifier is **no longer** direct: `build_agent` takes an optional SOCKS5 proxy and `transparency.rs` passes `socks5://<shim>` to the `verify_*_via` entry points, so the verify path routes through the overlay when on (§14.4 residual closed).
- **Modes:** `prefer` falls back to a direct dial if the overlay is unreachable; `strict` surfaces a degraded error rather than silently going direct (messages-must-work).
- **Relay auth (offline, no metadata-plane query).** The relay authenticates a connecting device with an **offline device-certificate chain** — no Turso query, no network call per connection — which is what keeps a relay node out of the metadata plane (design §11.1; `docs/relay-operations.md`). The client presents its device signing key + cert chain (`account_id_pub`, `device_cert`, `identity_version`, `issued_at`); the relay verifies possession (handshake signature) + membership (`verify_device_cert`) locally. The cert primitive lives in the shared **`pollis-device-cert`** crate (deps: `ed25519-dalek` + std), which `pollis-core` mints with and re-exports, and `pollis-relay` verifies with — one frozen format, no crate cycle. A device with no cert yet (pre-enrollment/OTP bootstrap) can't cert-auth and stays direct.
- **Deployable node** (`pollis-relay` bin): TOML config file (bind / allowlist / persisted QUIC identity / rate limits), generate-and-persist self-signed QUIC identity, graceful shutdown (drain on SIGTERM/SIGINT), and in-memory per-account / per-IP rate + concurrency limits (`Rejected(RateLimited)`). Stateless, disposable, rotatable; holds no Turso/DS credentials. See `docs/relay-operations.md`.

## Realtime

LiveKit rooms carry realtime events (new_message, membership_changed, voice_joined, etc.). The Rust event loop in `livekit.rs` receives data events and pushes them through the `EventSink` trait; `src-tauri/src/sink.rs`'s `ChannelSink` wraps a `tauri::ipc::Channel<E>` so the event rides Tauri's IPC channel to the renderer, which subscribes through the bridge's `channelOn(id, handler)`. MLS operations (process commits, poll welcomes) fire as needed.

Events are a **convenience for speed**, not a correctness requirement. All MLS state is also read from the DB on every message send/receive, so offline devices catch up when they next interact.

## Voice

Voice channels run entirely in Rust by design — cross-platform parity with mobile (same uniffi-exposed `pollis-core`) and predictable allocation under heavy media buffers. The capture pipeline is `cpal mic → optional RNNoise → WebRTC APM (AGC2 + NS + HPF + AEC) → LiveKit publish`; remote playback is `NativeAudioStream → per-track buffers → mixer task (10 ms tick) → single cpal output stream`, which is also where the AEC render reference is tapped. Settings (mic boost, AGC target, NS level, AEC, click suppression) live in user preferences and apply mid-call via `set_voice_audio_processing` without rejoining. See [audio-processing.md](./audio-processing.md) for the full pipeline, framing constraints, and tuning surface.

Voice is end-to-end encrypted: each frame is AES-128-GCM-encrypted by libwebrtc's `FrameCryptor` post-Opus, keyed by a 32-byte secret derived from the channel's MLS group (`MlsGroup::export_secret("pollis/voice/v1", epoch, 32)`). The LiveKit SFU forwards ciphertext only; the key rotates automatically on every MLS epoch advance. See `pollis-core/src/commands/voice_e2ee.rs` and the "End-to-end encryption" section of [audio-processing.md](./audio-processing.md#end-to-end-encryption).

---
_Back to [index.md](./index.md)_
